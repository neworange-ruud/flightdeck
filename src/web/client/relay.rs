//! The session browser's relay on this machine (`specs/SESSION_BROWSER_PRD.md`
//! B4, B22, B23; `remote-control-cefv.3`): everything the browser in a remote
//! session connects to, sent through the worker's tunnels
//! ([`super::tunnel_http`]).
//!
//! Two doors in, one way out:
//!
//! - **SOCKS5** on `127.0.0.1`, a random port, with a random username and
//!   password (RFC 1929) that only the session's web view is given, so no
//!   other program on this machine can use the worker's network through it.
//!   Every `CONNECT` — a name or an address, never resolved here — becomes one
//!   tunnel to that target. On macOS the web view's proxy points here.
//! - **Loopback forwards**: WKWebView never sends `localhost`, `127.0.0.1` or
//!   `[::1]` through a proxy (the spike), so for each loopback port the session
//!   uses the relay listens on `127.0.0.1:<port>` and `[::1]:<port>` itself and
//!   sends each connection through a tunnel to that port on the worker. The
//!   page still asks for `localhost:<port>`. A port something on this machine
//!   already uses is refused ([`ForwardError::InUse`]) and never shared: the
//!   listener is bound without `SO_REUSEADDR`, so it cannot quietly sit in
//!   front of a program listening on `0.0.0.0:<port>`.
//!
//! A tunnel the worker refuses is a SOCKS failure or a closed connection to
//! the web view, and a [`RelayEvent::Refused`] with the worker's words for
//! the browser's own error page.
//!
//! Everything runs on the shared runtime and stops with [`Relay::stop`] (or
//! drop): the listeners close and every open tunnel is cut.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use super::http::HttpAccess;
use super::tunnel_http::{self, TunnelRefusal};

/// The longest a SOCKS username or password may be (RFC 1929's own limit).
const MAX_CREDENTIAL: usize = 255;

/// What the relay saw that the browser tab should know about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayEvent {
    /// The worker would not make a connection: `target` as asked, and its
    /// reason and words.
    Refused {
        target: String,
        reason: String,
        message: String,
    },
}

/// Why a loopback port could not be forwarded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardError {
    /// Something on this machine already listens on it.
    InUse(u16),
    /// Any other failure to listen.
    Failed(String),
}

impl std::fmt::Display for ForwardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForwardError::InUse(port) => write!(
                f,
                "Port {port} is in use on this computer, so the worker's localhost:{port} \
                 cannot be shown here. Stop what uses it, or use another port on the worker."
            ),
            ForwardError::Failed(e) => write!(f, "Could not forward the port: {e}"),
        }
    }
}

/// Where the relay sends things, shared by its tasks.
struct Inner {
    access: HttpAccess,
    key: String,
    username: String,
    password: String,
    events: Mutex<Vec<RelayEvent>>,
    /// `true` once the relay stops; every task watches it.
    stopped: watch::Receiver<bool>,
}

/// See the module docs.
pub struct Relay {
    inner: Arc<Inner>,
    port: u16,
    stop: watch::Sender<bool>,
    /// Loopback ports forwarded, local port → the worker's port.
    forwards: Mutex<HashMap<u16, u16>>,
}

impl Relay {
    /// Start a relay sending through the worker at `access` with tunnel
    /// `key`, its SOCKS5 door on a free port on `127.0.0.1`.
    pub fn start(access: HttpAccess, key: String) -> io::Result<Relay> {
        let runtime = crate::remote::runtime::shared();
        let _enter = runtime.handle().enter();
        let listener = bind_strict(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))?;
        let port = listener.local_addr()?.port();
        let (stop, stopped) = watch::channel(false);
        let inner = Arc::new(Inner {
            access,
            key,
            username: crate::web::upload::new_ticket(),
            password: crate::web::upload::new_ticket(),
            events: Mutex::new(Vec::new()),
            stopped,
        });
        runtime
            .handle()
            .spawn(accept_loop(listener, inner.clone(), Door::Socks));
        Ok(Relay {
            inner,
            port,
            stop,
            forwards: Mutex::new(HashMap::new()),
        })
    }

    /// The SOCKS5 door's port on `127.0.0.1`.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The username and password the web view's proxy configuration carries.
    pub fn credentials(&self) -> (&str, &str) {
        (&self.inner.username, &self.inner.password)
    }

    /// Forward loopback `port` to the same port on the worker. Idempotent.
    pub fn forward(&self, port: u16) -> Result<(), ForwardError> {
        self.forward_to(port, port)
    }

    /// Forward local loopback `local` to the worker's `remote`. Only tests use
    /// two different ports (their "worker" is this machine, B24 never remaps).
    #[doc(hidden)]
    pub fn forward_to(&self, local: u16, remote: u16) -> Result<(), ForwardError> {
        let mut forwards = lock(&self.forwards);
        if forwards.contains_key(&local) {
            return Ok(());
        }
        if port_in_use(local) {
            return Err(ForwardError::InUse(local));
        }
        let runtime = crate::remote::runtime::shared();
        let _enter = runtime.handle().enter();
        let v4 = bind_strict(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), local))
            .map_err(|e| forward_error(local, e))?;
        // IPv6 loopback may be switched off; only a port in use refuses.
        let v6 = match bind_strict(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), local)) {
            Ok(listener) => Some(listener),
            Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
                return Err(ForwardError::InUse(local))
            }
            Err(_) => None,
        };
        let handle = runtime.handle();
        handle.spawn(accept_loop(
            v4,
            self.inner.clone(),
            Door::Loopback(format!("127.0.0.1:{remote}")),
        ));
        if let Some(v6) = v6 {
            handle.spawn(accept_loop(
                v6,
                self.inner.clone(),
                Door::Loopback(format!("[::1]:{remote}")),
            ));
        }
        forwards.insert(local, remote);
        Ok(())
    }

    /// The loopback ports forwarded, in order.
    pub fn forwarded_ports(&self) -> Vec<u16> {
        let mut ports: Vec<u16> = lock(&self.forwards).keys().copied().collect();
        ports.sort_unstable();
        ports
    }

    /// What happened since the last call, oldest first.
    pub fn take_events(&self) -> Vec<RelayEvent> {
        std::mem::take(&mut *lock(&self.inner.events))
    }

    /// Close every door and cut every tunnel.
    pub fn stop(self) {}
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

/// Which door a connection came in by.
#[derive(Clone)]
enum Door {
    Socks,
    /// A loopback forward, and the worker target it stands for.
    Loopback(String),
}

/// A listener bound **without** `SO_REUSEADDR`, so it fails while anything
/// else listens on the port, wildcard included. std's `bind` turns the option
/// on, and on macOS that lets a more specific bind (`127.0.0.1:3000`) sit in
/// front of a program listening on `0.0.0.0:3000`. An IPv6 listener is
/// IPv6-only, so the two loopback listeners of a forward never collide.
fn bind_strict(addr: SocketAddr) -> io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    socket.set_nonblocking(true)?;
    TcpListener::from_std(socket.into())
}

/// Whether something on this machine holds `port` on **any** address.
///
/// Binding the loopback address alone does not say so everywhere: Windows
/// lets `127.0.0.1:P` be bound while another program listens on
/// `0.0.0.0:P`, and the forward would then sit in front of it. So the
/// wildcard addresses are tried first — bound and closed again, never
/// listened on, so no firewall is asked anything. Only "in use" counts; a
/// machine without IPv6 is not a taken port.
fn port_in_use(port: u16) -> bool {
    use socket2::{Domain, Protocol, Socket, Type};
    let wildcards = [
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port),
    ];
    wildcards.iter().any(|addr| {
        let Ok(socket) = Socket::new(
            Domain::for_address(*addr),
            Type::STREAM,
            Some(Protocol::TCP),
        ) else {
            return false;
        };
        if addr.is_ipv6() && socket.set_only_v6(true).is_err() {
            return false;
        }
        matches!(socket.bind(&(*addr).into()), Err(e) if e.kind() == io::ErrorKind::AddrInUse)
    })
}

fn forward_error(port: u16, e: io::Error) -> ForwardError {
    if e.kind() == io::ErrorKind::AddrInUse {
        ForwardError::InUse(port)
    } else {
        ForwardError::Failed(e.to_string())
    }
}

async fn accept_loop(listener: TcpListener, inner: Arc<Inner>, door: Door) {
    let mut stopped = inner.stopped.clone();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let inner = inner.clone();
                let door = door.clone();
                tokio::spawn(async move {
                    let mut stopped = inner.stopped.clone();
                    tokio::select! {
                        _ = serve(stream, &inner, door) => {}
                        _ = stopped.wait_for(|s| *s) => {}
                    }
                });
            }
            _ = stopped.wait_for(|s| *s) => return,
        }
    }
}

async fn serve(mut stream: TcpStream, inner: &Inner, door: Door) {
    let socks = matches!(door, Door::Socks);
    let target = match door {
        Door::Loopback(target) => target,
        Door::Socks => match socks_handshake(&mut stream, inner).await {
            Ok(target) => target,
            Err(_) => return,
        },
    };
    match tunnel_http::open(&inner.access, &inner.key, &target).await {
        Ok(tunnel) => {
            // SOCKS: tell the web view it is connected. A forward is already
            // a plain connection.
            if socks && stream.write_all(&socks_reply(0)).await.is_err() {
                return;
            }
            let mut upstream = tunnel.stream;
            if !tunnel.early.is_empty() && stream.write_all(&tunnel.early).await.is_err() {
                return;
            }
            let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
        }
        Err(refusal) => {
            if socks {
                let _ = stream.write_all(&socks_reply(socks_code(&refusal))).await;
            }
            lock(&inner.events).push(RelayEvent::Refused {
                target,
                reason: refusal.reason,
                message: refusal.message,
            });
        }
    }
}

/// The SOCKS5 reply code for a worker's refusal (RFC 1928 §6).
fn socks_code(refusal: &TunnelRefusal) -> u8 {
    match refusal.reason.as_str() {
        "refused" => 0x05,                             // connection refused
        "unresolved" | "timed_out" => 0x04,            // host unreachable
        "invalid_tunnel" | "too_many_tunnels" => 0x02, // not allowed by ruleset
        "unreachable" => 0x03,                         // network unreachable
        _ => 0x01,                                     // general failure
    }
}

/// A SOCKS5 reply with code `code` and a zero IPv4 bound address.
fn socks_reply(code: u8) -> [u8; 10] {
    [5, code, 0, 1, 0, 0, 0, 0, 0, 0]
}

/// RFC 1928 greeting, RFC 1929 username/password, and a `CONNECT`: the
/// target as `host:port` (IPv6 bracketed), names left unresolved.
async fn socks_handshake(stream: &mut TcpStream, inner: &Inner) -> io::Result<String> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await?;
    if head[0] != 5 {
        return Err(bad("not SOCKS5"));
    }
    let mut methods = vec![0u8; head[1] as usize];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&2) {
        stream.write_all(&[5, 0xff]).await?;
        return Err(bad("no username/password method"));
    }
    stream.write_all(&[5, 2]).await?;

    let mut version = [0u8; 1];
    stream.read_exact(&mut version).await?;
    let user = read_counted(stream).await?;
    let pass = read_counted(stream).await?;
    let ok = version[0] == 1
        && constant_time_eq(&user, inner.username.as_bytes())
        && constant_time_eq(&pass, inner.password.as_bytes());
    stream.write_all(&[1, if ok { 0 } else { 1 }]).await?;
    if !ok {
        return Err(bad("wrong credentials"));
    }

    let mut request = [0u8; 4];
    stream.read_exact(&mut request).await?;
    if request[0] != 5 || request[1] != 1 {
        // Only CONNECT.
        stream.write_all(&socks_reply(0x07)).await?;
        return Err(bad("not CONNECT"));
    }
    let host = match request[3] {
        1 => {
            let mut a = [0u8; 4];
            stream.read_exact(&mut a).await?;
            Ipv4Addr::from(a).to_string()
        }
        3 => {
            let name = read_counted(stream).await?;
            String::from_utf8(name).map_err(|_| bad("name is not UTF-8"))?
        }
        4 => {
            let mut a = [0u8; 16];
            stream.read_exact(&mut a).await?;
            format!("[{}]", Ipv6Addr::from(a))
        }
        _ => {
            stream.write_all(&socks_reply(0x08)).await?;
            return Err(bad("unknown address type"));
        }
    };
    let mut port = [0u8; 2];
    stream.read_exact(&mut port).await?;
    Ok(format!("{host}:{}", u16::from_be_bytes(port)))
}

/// A length-prefixed field (one byte of length, then that many bytes).
async fn read_counted(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 1];
    stream.read_exact(&mut len).await?;
    let mut field = vec![0u8; (len[0] as usize).min(MAX_CREDENTIAL)];
    stream.read_exact(&mut field).await?;
    Ok(field)
}

/// `Mutex::lock` that survives a poisoned lock: the relay's state is a few
/// maps and a list, never left half-written by a panic.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Compare without stopping at the first difference.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::client::exchange::AccessToken;

    /// A relay pointed at a worker that is not there: enough for everything
    /// that happens before a tunnel is asked for.
    fn relay() -> Relay {
        Relay::start(
            HttpAccess {
                address: "127.0.0.1:9".to_string(),
                token: AccessToken::new("t".to_string()),
                user_agent: "test".to_string(),
            },
            "key".to_string(),
        )
        .expect("the relay starts")
    }

    fn refusal(reason: &str) -> TunnelRefusal {
        TunnelRefusal {
            reason: reason.to_string(),
            message: String::new(),
        }
    }

    #[test]
    fn worker_refusals_become_the_matching_socks_codes() {
        assert_eq!(socks_code(&refusal("refused")), 0x05);
        assert_eq!(socks_code(&refusal("unresolved")), 0x04);
        assert_eq!(socks_code(&refusal("timed_out")), 0x04);
        assert_eq!(socks_code(&refusal("invalid_tunnel")), 0x02);
        assert_eq!(socks_code(&refusal("unreachable")), 0x03);
        assert_eq!(socks_code(&refusal("anything else")), 0x01);
    }

    #[test]
    fn credentials_are_random_and_compared_whole() {
        let a = relay();
        let b = relay();
        assert_ne!(a.credentials(), b.credentials());
        assert_ne!(a.credentials().0, a.credentials().1);
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret!"));
        assert!(!constant_time_eq(b"", b"x"));
    }

    #[test]
    fn a_port_in_use_on_loopback_only_is_refused() {
        let relay = relay();
        let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = busy.local_addr().unwrap().port();
        assert_eq!(relay.forward(port), Err(ForwardError::InUse(port)));
    }

    #[test]
    fn a_port_in_use_here_is_refused_even_on_the_wildcard_address() {
        let relay = relay();
        // Something on this machine listens on every address of a port.
        let busy = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = busy.local_addr().unwrap().port();
        assert_eq!(relay.forward(port), Err(ForwardError::InUse(port)));
        assert!(relay.forwarded_ports().is_empty());
        assert!(ForwardError::InUse(port)
            .to_string()
            .contains(&port.to_string()));
    }

    #[test]
    fn a_free_port_is_forwarded_once_and_listens_on_loopback_only() {
        let relay = relay();
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        relay.forward(port).unwrap();
        relay.forward(port).unwrap();
        assert_eq!(relay.forwarded_ports(), vec![port]);
        // The forward holds the port now.
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_err());
        drop(relay);
        // Stopping gives it back.
        let mut free_again = false;
        for _ in 0..50 {
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                free_again = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(free_again, "the port is released when the relay stops");
    }

    #[test]
    fn the_socks_door_refuses_without_the_password() {
        use std::io::{Read, Write};
        let relay = relay();
        let mut client = std::net::TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        // No-auth only: refused outright.
        client.write_all(&[5, 1, 0]).unwrap();
        let mut answer = [0u8; 2];
        client.read_exact(&mut answer).unwrap();
        assert_eq!(answer, [5, 0xff]);

        // The wrong password: refused at the sub-negotiation.
        let mut client = std::net::TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        client.write_all(&[5, 1, 2]).unwrap();
        client.read_exact(&mut answer).unwrap();
        assert_eq!(answer, [5, 2]);
        let user = relay.credentials().0.as_bytes();
        let mut auth = vec![1, user.len() as u8];
        auth.extend_from_slice(user);
        auth.extend_from_slice(&[5]);
        auth.extend_from_slice(b"wrong");
        client.write_all(&auth).unwrap();
        client.read_exact(&mut answer).unwrap();
        assert_eq!(answer, [1, 1]);
    }
}
