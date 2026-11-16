//! One HTTP proxy for every session browser in the app
//! (`specs/SESSION_BROWSER_PRD.md` §4.1, "The Windows design";
//! `remote-control-cefv.12.1`).
//!
//! WebView2 fixes the proxy for its whole browser process, and every
//! session's browser shares that process (one data folder, a profile per
//! session), so one proxy has to serve every session. It tells them apart by
//! the login each session's web views answer the proxy's 407 with: a random
//! username and password per session ([`ProxyHub::worker_session`],
//! [`ProxyHub::direct_session`]), which also keeps every other program on
//! this machine out, as the relay's SOCKS5 password does on macOS (B23).
//! Chromium caches a proxy login per profile, never across them (the
//! `cefv.11` spike), so one session's login never reaches another's route.
//!
//! - **Routes.** A worker session's connections go through that worker's
//!   tunnels ([`super::tunnel_http`]) with its key, replaced on reconnect
//!   ([`HubSession::set_key`]); a local session's connect directly from here
//!   (B9). With `--proxy-bypass-list=<-loopback>` loopback comes here too,
//!   so a worker session's `localhost` is the worker's, and no local
//!   forwards are needed (B22 is macOS only).
//! - **Requests.** `CONNECT host:port` (HTTPS, WebSockets) becomes one
//!   tunnel. Plain HTTP arrives in absolute form (`GET http://host/path`):
//!   its first request is rewritten to origin form, without the proxy's own
//!   headers and with `Connection: close`, so the browser opens a new
//!   connection for the next one rather than sending it, for another host
//!   perhaps, down this one.
//! - **Targets** go to the worker as `host:port`, an IPv6 address bracketed
//!   (`[::1]:3000`), as its `parse_target` wants.
//!
//! A target that cannot be reached is a `502` with the reason, and a
//! [`RelayEvent::Refused`] for the browser's own error page.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use super::http::HttpAccess;
use super::relay::RelayEvent;
use super::tunnel_http::{self, TunnelRefusal};

/// The realm the proxy's 407 names.
const REALM: &str = "FlightDeck";
/// The longest request head the proxy reads.
const MAX_HEAD: usize = 64 * 1024;
/// How long a browser has to send a request head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a local session's direct connection may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// See the module docs.
pub struct ProxyHub {
    inner: Arc<HubInner>,
    port: u16,
    stop: watch::Sender<bool>,
}

struct HubInner {
    /// Username → its session's route.
    sessions: Mutex<HashMap<String, Arc<Route>>>,
    stopped: watch::Receiver<bool>,
}

/// Where one session's connections go.
struct Route {
    password: String,
    way: Way,
    events: Mutex<Vec<RelayEvent>>,
}

enum Way {
    /// Through the worker at `access`, with the tunnel key granted to this
    /// window (replaced when it reconnects).
    Worker {
        access: HttpAccess,
        key: Mutex<String>,
    },
    /// From this machine (a local session, B9).
    Direct,
}

/// One session's place in the hub: the login its web views answer the
/// proxy with. Dropping it removes the session; its login stops working.
pub struct HubSession {
    hub: Weak<HubInner>,
    username: String,
    route: Arc<Route>,
}

impl ProxyHub {
    /// Start the proxy on a free port on `127.0.0.1`, with no sessions yet.
    pub fn start() -> io::Result<ProxyHub> {
        let runtime = crate::remote::runtime::shared();
        let _enter = runtime.handle().enter();
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let listener = TcpListener::from_std(listener)?;
        let port = listener.local_addr()?.port();
        let (stop, stopped) = watch::channel(false);
        let inner = Arc::new(HubInner {
            sessions: Mutex::new(HashMap::new()),
            stopped,
        });
        runtime.handle().spawn(accept_loop(listener, inner.clone()));
        Ok(ProxyHub { inner, port, stop })
    }

    /// The proxy's port on `127.0.0.1`.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// A session whose connections the worker at `access` makes, with
    /// tunnel `key`.
    pub fn worker_session(&self, access: HttpAccess, key: String) -> HubSession {
        self.add(Way::Worker {
            access,
            key: Mutex::new(key),
        })
    }

    /// A local session, whose connections this machine makes (B9).
    pub fn direct_session(&self) -> HubSession {
        self.add(Way::Direct)
    }

    fn add(&self, way: Way) -> HubSession {
        let username = crate::web::upload::new_ticket();
        let route = Arc::new(Route {
            password: crate::web::upload::new_ticket(),
            way,
            events: Mutex::new(Vec::new()),
        });
        lock(&self.inner.sessions).insert(username.clone(), route.clone());
        HubSession {
            hub: Arc::downgrade(&self.inner),
            username,
            route,
        }
    }
}

impl Drop for ProxyHub {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

impl HubSession {
    /// The username and password this session's web views answer the
    /// proxy's login with.
    pub fn credentials(&self) -> (&str, &str) {
        (&self.username, &self.route.password)
    }

    /// Send new tunnels with `key` from now on (the window reconnected).
    /// Nothing for a local session.
    pub fn set_key(&self, key: String) {
        if let Way::Worker { key: current, .. } = &self.route.way {
            *lock(current) = key;
        }
    }

    /// What happened since the last call, oldest first.
    pub fn take_events(&self) -> Vec<RelayEvent> {
        std::mem::take(&mut *lock(&self.route.events))
    }
}

impl Drop for HubSession {
    fn drop(&mut self) {
        if let Some(hub) = self.hub.upgrade() {
            lock(&hub.sessions).remove(&self.username);
        }
    }
}

async fn accept_loop(listener: TcpListener, inner: Arc<HubInner>) {
    let mut stopped = inner.stopped.clone();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let inner = inner.clone();
                tokio::spawn(async move {
                    let mut stopped = inner.stopped.clone();
                    tokio::select! {
                        _ = serve(stream, &inner) => {}
                        _ = stopped.wait_for(|s| *s) => {}
                    }
                });
            }
            _ = stopped.wait_for(|s| *s) => return,
        }
    }
}

async fn serve(stream: TcpStream, hub: &HubInner) {
    let mut client = BufReader::new(stream);
    let head = match tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut client)).await {
        Ok(Ok(Some(head))) => head,
        _ => return,
    };
    let Some(request) = Request::parse(&head) else {
        let _ = reply(&mut client, 400, "Bad Request", "Not an HTTP request.", "").await;
        return;
    };
    let Some(route) = authorize(hub, request.header("proxy-authorization")) else {
        let challenge = format!("Proxy-Authenticate: Basic realm=\"{REALM}\"\r\n");
        let _ = reply(
            &mut client,
            407,
            "Proxy Authentication Required",
            "This proxy is FlightDeck's session browser's.",
            &challenge,
        )
        .await;
        return;
    };

    let connect = request.method.eq_ignore_ascii_case("CONNECT");
    let (authority, forward) = if connect {
        (request.target.clone(), None)
    } else {
        match absolute_http(&request.target) {
            Some((authority, path)) => (authority, Some(request.origin_form(&path))),
            None => {
                let _ = reply(
                    &mut client,
                    400,
                    "Bad Request",
                    "This proxy takes CONNECT and http:// requests.",
                    "",
                )
                .await;
                return;
            }
        }
    };
    let Some(target) = normalize_target(&authority, if connect { None } else { Some(80) }) else {
        let _ = reply(
            &mut client,
            400,
            "Bad Request",
            "No host:port to reach.",
            "",
        )
        .await;
        return;
    };

    match open(&route, &target).await {
        Ok((mut upstream, early)) => {
            let started = match &forward {
                Some(head) => upstream.write_all(head.as_bytes()).await,
                None => {
                    client
                        .get_mut()
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .await
                }
            };
            if started.is_err() {
                return;
            }
            // What the browser sent after the head (a request body).
            let buffered = client.buffer().to_vec();
            if !buffered.is_empty() && upstream.write_all(&buffered).await.is_err() {
                return;
            }
            let mut stream = client.into_inner();
            if !early.is_empty() && stream.write_all(&early).await.is_err() {
                return;
            }
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
        }
        Err(refusal) => {
            let _ = reply(&mut client, 502, "Bad Gateway", &refusal.message, "").await;
            lock(&route.events).push(RelayEvent::Refused {
                target,
                reason: refusal.reason,
                message: refusal.message,
            });
        }
    }
}

/// The route whose username and password `header` (`Basic …`) carries.
fn authorize(hub: &HubInner, header: Option<&str>) -> Option<Arc<Route>> {
    let encoded = header?.trim().strip_prefix("Basic ")?.trim();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (username, password) = decoded.split_once(':')?;
    let route = lock(&hub.sessions).get(username).cloned()?;
    super::relay::constant_time_eq(password.as_bytes(), route.password.as_bytes()).then_some(route)
}

/// A connection to `target` (`host:port`) by `route`'s way, and any bytes
/// the far side already sent.
async fn open(route: &Route, target: &str) -> Result<(TcpStream, Vec<u8>), TunnelRefusal> {
    match &route.way {
        Way::Worker { access, key } => {
            let key = lock(key).clone();
            let tunnel = tunnel_http::open(access, &key, target).await?;
            Ok((tunnel.stream, tunnel.early))
        }
        Way::Direct => connect_direct(target)
            .await
            .map(|stream| (stream, Vec::new())),
    }
}

/// A local session's connection, with the worker's words for what went
/// wrong, said of this computer.
async fn connect_direct(target: &str) -> Result<TcpStream, TunnelRefusal> {
    let refusal = |reason: &str, message: String| TunnelRefusal {
        reason: reason.to_string(),
        message,
    };
    let connect = async {
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host(target)
            .await
            .map(|addrs| addrs.collect())
            .unwrap_or_default();
        if addrs.is_empty() {
            return Err(refusal(
                "unresolved",
                format!("This computer could not find {target}."),
            ));
        }
        TcpStream::connect(&addrs[..])
            .await
            .map_err(|e| match e.kind() {
                io::ErrorKind::ConnectionRefused => refusal(
                    "refused",
                    format!("Nothing is listening on {target} on this computer."),
                ),
                _ => refusal(
                    "connect_failed",
                    format!("This computer could not reach {target}: {e}"),
                ),
            })
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(result) => result,
        Err(_) => Err(refusal("timed_out", format!("{target} did not answer."))),
    }
}

/// A request head, up to and including its blank line; `None` when the
/// connection closed first or the head is too long.
async fn read_head(client: &mut BufReader<TcpStream>) -> io::Result<Option<String>> {
    let mut head = Vec::new();
    loop {
        let before = head.len();
        if client.read_until(b'\n', &mut head).await? == 0 {
            return Ok(None);
        }
        let line = &head[before..];
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        if head.len() > MAX_HEAD {
            return Ok(None);
        }
    }
    Ok(String::from_utf8(head).ok())
}

/// A parsed request head.
struct Request {
    method: String,
    target: String,
    version: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn parse(head: &str) -> Option<Request> {
        let mut lines = head.lines();
        let mut first = lines.next()?.split_whitespace();
        let (method, target, version) = (first.next()?, first.next()?, first.next()?);
        if first.next().is_some() || !version.starts_with("HTTP/") {
            return None;
        }
        let headers = lines
            .take_while(|line| !line.is_empty())
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
            .collect();
        Some(Request {
            method: method.to_string(),
            target: target.to_string(),
            version: version.to_string(),
            headers,
        })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// This request as its server should see it: `path` instead of the
    /// absolute URL, without the proxy's headers, and closing after the
    /// answer (the next request may be for another host).
    fn origin_form(&self, path: &str) -> String {
        const HOP: [&str; 4] = [
            "proxy-authorization",
            "proxy-connection",
            "connection",
            "keep-alive",
        ];
        let mut out = format!("{} {path} {}\r\n", self.method, self.version);
        for (name, value) in &self.headers {
            if !HOP.iter().any(|hop| name.eq_ignore_ascii_case(hop)) {
                out.push_str(&format!("{name}: {value}\r\n"));
            }
        }
        out.push_str("Connection: close\r\n\r\n");
        out
    }
}

/// `http://authority/path?query` → (`authority`, `/path?query`); `None` for
/// anything else (another scheme, a login in the URL).
fn absolute_http(target: &str) -> Option<(String, String)> {
    let scheme = target.get(..7)?;
    if !scheme.eq_ignore_ascii_case("http://") {
        return None;
    }
    let rest = &target[7..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let path = path.split('#').next().unwrap_or_default();
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    Some((authority.to_string(), path))
}

/// `authority` as the worker's `parse_target` wants it: `host:port`, an
/// IPv6 address bracketed. `default_port` when it names none (`None`: a
/// port is required). An unbracketed IPv6 address (`::1:3000`, as SOCKS
/// names arrive) takes its last part as the port.
pub(crate) fn normalize_target(authority: &str, default_port: Option<u16>) -> Option<String> {
    let authority = authority.trim();
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(port) => Some(port),
            None if after.is_empty() => None,
            None => return None,
        };
        (host, port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    let port = match port {
        Some(port) => port.parse::<u16>().ok().filter(|p| *p != 0)?,
        None => default_port?,
    };
    if host.is_empty() || host.contains(['/', '@', '[', ']']) || host.contains(char::is_whitespace)
    {
        return None;
    }
    Some(if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    })
}

/// Write a short answer and close.
async fn reply(
    client: &mut BufReader<TcpStream>,
    code: u16,
    reason: &str,
    body: &str,
    extra: &str,
) -> io::Result<()> {
    let answer = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
        body.len()
    );
    let stream = client.get_mut();
    stream.write_all(answer.as_bytes()).await?;
    stream.shutdown().await
}

/// `Mutex::lock` that survives a poisoned lock.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests;
