//! Sending the pushes the bridge decided (remote-control-fglt, spec §5.5).
//!
//! [`crate::remote::RemoteBridge`] decides *when* the phone is notified and
//! seals each decision into a [`PushNotifyBody`] under the pairing's push key
//! ([`seal_push`], spec §7.2). This module decides *how* it travels:
//!
//! * **Over the relay WebSocket** ([`PushRoute::RelaySocket`]) when the relay
//!   client is connected, authenticated, and negotiated a protocol version of
//!   at least [`PUSH_NOTIFY_MIN_VERSION`]. The body goes onto the client's
//!   outbound channel as [`RemoteOutbound::PushNotify`] and the client writes a
//!   `push_notify` frame.
//! * **Otherwise over HTTPS** ([`PushRoute::Https`]): a signed
//!   [`PushRequest`] POSTed to [`push_endpoint_url`] of the configured relay URL,
//!   with the relay password header when one is configured. That covers a relay
//!   that is down or still reconnecting, a push-only relay the desktop holds no
//!   socket to, and a relay older than v4 (whose missing endpoint answers 404,
//!   which is logged and dropped; nothing retries or spins).
//!
//! The route is read from [`PushLink`], an atomic the relay client task
//! publishes on `auth_ok` and clears when the session ends, so choosing never
//! blocks the UI tick. The HTTPS request runs on the shared runtime
//! ([`crate::remote::runtime`]) behind the [`PushPoster`] seam, with a
//! [`PUSH_HTTP_TIMEOUT`] per attempt and one retry on a transient failure. Every
//! outcome is logged to [`crate::remote::debuglog`] as routing metadata only
//! (pairing, event id, class, route, status), never content.
//!
//! The desktop always pushes; it does not try to detect whether the phone app
//! is in the foreground. The phone dedups a push against the feed event that
//! carries the same `event_id`.
//!
//! A direct-mode pairing (spec §5.10) is pushed exactly the same way: pushes
//! always go via the relay, whatever carries the pairing's data plane.

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use flightdeck_remote_protocol::e2e::{truncate_push_text, MAX_PUSH_PLAINTEXT_BYTES};
use flightdeck_remote_protocol::relay::RELAY_PASSWORD_HEADER;
use flightdeck_remote_protocol::{
    push_endpoint_url, DeviceId, PushNotifyBody, PushPayload, PushRequest, PUSH_NOTIFY_MIN_VERSION,
};

use crate::contracts::Result;
use crate::remote::crypto::E2eChannel;
use crate::remote::notifier::PushIntent;
use crate::remote::{DeviceIdentity, RemoteOutbound};

/// Per-attempt bound on an HTTPS push (connect, TLS, request and response).
pub const PUSH_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Pause before the single retry of a transient HTTPS failure.
const PUSH_HTTP_RETRY_DELAY: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// Link state shared with the relay client task
// ---------------------------------------------------------------------------

/// Whether the relay WebSocket can carry a `push_notify` right now: the
/// negotiated protocol version of the live, authenticated session, or nothing.
///
/// Cloned between the relay client task (the only writer: it sets the version
/// on `auth_ok` and clears it when the session ends) and the push sender (the
/// reader). A plain atomic, so reading it on the UI tick never blocks.
#[derive(Debug, Clone, Default)]
pub struct PushLink(Arc<AtomicU16>);

impl PushLink {
    /// A link that starts down.
    pub fn new() -> Self {
        Self::default()
    }

    /// The negotiated version of the live authenticated session, or `None`
    /// when the relay WebSocket is not connected and authenticated.
    pub fn live_version(&self) -> Option<u16> {
        match self.0.load(Ordering::SeqCst) {
            0 => None,
            v => Some(v),
        }
    }

    /// The session authenticated at `version`.
    pub fn set_live(&self, version: u16) {
        self.0.store(version, Ordering::SeqCst);
    }

    /// The session ended (or never authenticated).
    pub fn set_down(&self) {
        self.0.store(0, Ordering::SeqCst);
    }
}

/// How a push travels to the relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushRoute {
    /// A `push_notify` frame on the live relay WebSocket.
    RelaySocket,
    /// `POST /v1/push` over HTTPS.
    Https,
}

/// The transport rule (spec §5.5.1): the relay WebSocket when it is connected
/// and authenticated at a negotiated version of at least
/// [`PUSH_NOTIFY_MIN_VERSION`], HTTPS otherwise.
pub fn choose_route(live_version: Option<u16>) -> PushRoute {
    match live_version {
        Some(v) if v >= PUSH_NOTIFY_MIN_VERSION => PushRoute::RelaySocket,
        _ => PushRoute::Https,
    }
}

// ---------------------------------------------------------------------------
// Sealing and signing
// ---------------------------------------------------------------------------

/// Seal `intent` for `channel`'s pairing (spec §7.2).
///
/// Title and body are already within their byte limits, but JSON escaping can
/// still push an unusual body (quotes, control characters) past the
/// [`MAX_PUSH_PLAINTEXT_BYTES`] cap on the serialized payload; such a body is
/// cut further, halving its budget until the payload fits, rather than the push
/// being lost.
pub fn seal_push(intent: &PushIntent, channel: &E2eChannel) -> Result<PushNotifyBody> {
    let payload = fit_payload(intent.to_payload());
    channel.seal_push_payload(&payload)
}

/// Shrink `payload.body` until the serialized payload fits the plaintext cap.
fn fit_payload(mut payload: PushPayload) -> PushPayload {
    let fits = |p: &PushPayload| {
        serde_json::to_vec(p)
            .map(|v| v.len() <= MAX_PUSH_PLAINTEXT_BYTES)
            .unwrap_or(false)
    };
    let mut budget = payload.body.len();
    while !fits(&payload) && !payload.body.is_empty() {
        budget /= 2;
        payload.body = truncate_push_text(&payload.body, budget);
    }
    payload
}

/// Build the signed HTTPS request for `push` (spec §5.5.3): ECDSA P-256 /
/// SHA-256 by the device identity key over
/// [`flightdeck_remote_protocol::push_request_signing_message`], raw `r ‖ s`,
/// base64. `timestamp_ms` is the desktop's wall clock.
pub fn sign_push_request(
    identity: &DeviceIdentity,
    timestamp_ms: i64,
    push: PushNotifyBody,
) -> PushRequest {
    let mut request = PushRequest {
        device_id: DeviceId::new(identity.device_id()),
        timestamp_ms,
        push,
        signature: String::new(),
    };
    let signature = identity.sign(request.signing_message().as_bytes());
    request.signature = STANDARD.encode(signature);
    request
}

// ---------------------------------------------------------------------------
// The HTTPS seam
// ---------------------------------------------------------------------------

/// One HTTPS push to deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpsPush {
    /// `https://host[:port]/v1/push` (or `http://` for a `ws://` relay).
    pub url: String,
    /// The relay password, sent as [`RELAY_PASSWORD_HEADER`] when set.
    pub relay_password: Option<String>,
    /// The signed request body.
    pub request: PushRequest,
}

/// Delivers HTTPS pushes. Fire-and-forget: [`Self::post`] must return at once
/// (the caller is the UI tick); the production [`RuntimePoster`] runs the
/// request on the shared runtime. Tests substitute a recorder.
pub trait PushPoster: Send {
    /// Start delivering `push`; never blocks.
    fn post(&self, push: HttpsPush);
}

/// The production [`PushPoster`]: spawns [`deliver`] on the shared runtime.
pub struct RuntimePoster;

impl PushPoster for RuntimePoster {
    fn post(&self, push: HttpsPush) {
        match crate::remote::runtime::try_shared() {
            Some(rt) => {
                rt.spawn(async move {
                    deliver(&push).await;
                });
            }
            None => log_outcome(&push.request.push, PushRoute::Https, "no runtime"),
        }
    }
}

/// What an HTTPS push attempt came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushHttpOutcome {
    /// 2xx: the relay accepted it (also when no push token is registered).
    Accepted(u16),
    /// A refusal retrying cannot fix (4xx, including 429 and an old relay's 404).
    Rejected(u16),
    /// Worth one retry: a connect/TLS/IO failure, a timeout, or a 5xx.
    Transient(String),
}

impl PushHttpOutcome {
    fn from_status(status: u16) -> Self {
        match status {
            200..=299 => PushHttpOutcome::Accepted(status),
            500..=599 => PushHttpOutcome::Transient(format!("status {status}")),
            _ => PushHttpOutcome::Rejected(status),
        }
    }

    fn label(&self) -> String {
        match self {
            PushHttpOutcome::Accepted(s) => format!("ok status={s}"),
            PushHttpOutcome::Rejected(s) => format!("rejected status={s}"),
            PushHttpOutcome::Transient(why) => format!("transient {why}"),
        }
    }
}

/// POST `push`, retrying once on a transient failure, and log the outcome.
pub async fn deliver(push: &HttpsPush) -> PushHttpOutcome {
    deliver_with(push, PUSH_HTTP_RETRY_DELAY).await
}

/// [`deliver`] with an explicit pause before the retry (tests pass zero).
async fn deliver_with(push: &HttpsPush, retry_delay: Duration) -> PushHttpOutcome {
    let body = match serde_json::to_vec(&push.request) {
        Ok(b) => b,
        Err(e) => {
            let outcome = PushHttpOutcome::Rejected(0);
            log_outcome(
                &push.request.push,
                PushRoute::Https,
                &format!("unserializable: {e}"),
            );
            return outcome;
        }
    };
    let mut attempt = 0;
    loop {
        attempt += 1;
        let outcome = match tokio::time::timeout(
            PUSH_HTTP_TIMEOUT,
            post_once(&push.url, push.relay_password.as_deref(), &body),
        )
        .await
        {
            Ok(Ok(status)) => PushHttpOutcome::from_status(status),
            Ok(Err(e)) => PushHttpOutcome::Transient(e),
            Err(_) => PushHttpOutcome::Transient("timed out".to_string()),
        };
        log_outcome(
            &push.request.push,
            PushRoute::Https,
            &format!("attempt={attempt} {}", outcome.label()),
        );
        match outcome {
            PushHttpOutcome::Transient(_) if attempt < 2 => {
                tokio::time::sleep(retry_delay).await;
            }
            done => return done,
        }
    }
}

/// Bytes both TLS backends and a plain socket hand to hyper.
trait PushIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> PushIo for T {}

/// One HTTP/1.1 POST of `json` to `url`; returns the response status.
async fn post_once(
    url: &str,
    relay_password: Option<&str>,
    json: &[u8],
) -> std::result::Result<u16, String> {
    use http_body_util::Full;
    use hyper::body::Bytes;
    use hyper::header::{CONTENT_TYPE, HOST};

    let uri: hyper::Uri = url.parse().map_err(|e| format!("bad push url: {e}"))?;
    let secure = uri.scheme_str() == Some("https");
    let host = uri
        .host()
        .ok_or_else(|| "push url has no host".to_string())?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| format!("tcp connect failed: {e}"))?;
    let io: Box<dyn PushIo> = if secure {
        tls_connect(&host, tcp).await?
    } else {
        Box::new(tcp)
    };
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(io))
            .await
            .map_err(|e| format!("http handshake failed: {e}"))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let authority = uri
        .authority()
        .map(|a| a.as_str().to_string())
        .unwrap_or(host);
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let mut request = hyper::Request::post(path)
        .header(HOST, authority)
        .header(CONTENT_TYPE, "application/json");
    if let Some(password) = relay_password {
        request = request.header(RELAY_PASSWORD_HEADER, password);
    }
    let request = request
        .body(Full::new(Bytes::copy_from_slice(json)))
        .map_err(|e| format!("bad push request: {e}"))?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|e| format!("http request failed: {e}"))?;
    Ok(response.status().as_u16())
}

/// TLS for the push POST off Windows: rustls with the webpki roots, the same
/// backend and trust store the relay WebSocket uses on this target.
#[cfg(not(windows))]
async fn tls_connect(
    host: &str,
    tcp: tokio::net::TcpStream,
) -> std::result::Result<Box<dyn PushIo>, String> {
    use tokio_rustls::rustls;

    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| format!("bad tls server name: {e}"))?;
    let stream = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map_err(|e| format!("tls handshake failed: {e}"))?;
    Ok(Box::new(stream))
}

/// TLS for the push POST on Windows: SChannel via native-tls, the same backend
/// the relay WebSocket uses on this target.
#[cfg(windows)]
async fn tls_connect(
    host: &str,
    tcp: tokio::net::TcpStream,
) -> std::result::Result<Box<dyn PushIo>, String> {
    let connector = tokio_native_tls::native_tls::TlsConnector::new()
        .map_err(|e| format!("tls setup failed: {e}"))?;
    let stream = tokio_native_tls::TlsConnector::from(connector)
        .connect(host, tcp)
        .await
        .map_err(|e| format!("tls handshake failed: {e}"))?;
    Ok(Box::new(stream))
}

// ---------------------------------------------------------------------------
// The sender the host drives
// ---------------------------------------------------------------------------

/// Routes sealed pushes to the relay (see the module docs). Owned by the host
/// for as long as the relay client runs.
pub struct PushSender {
    link: PushLink,
    identity: DeviceIdentity,
    endpoint: Option<String>,
    relay_password: Option<String>,
    poster: Box<dyn PushPoster>,
}

impl PushSender {
    /// A sender for the relay at `relay_url` (its `wss://`/`ws://` URL; the
    /// HTTPS endpoint is derived from it), signing HTTPS requests as `identity`
    /// and delivering them on the shared runtime.
    pub fn new(
        link: PushLink,
        identity: DeviceIdentity,
        relay_url: &str,
        relay_password: Option<String>,
    ) -> Self {
        Self::with_poster(
            link,
            identity,
            relay_url,
            relay_password,
            Box::new(RuntimePoster),
        )
    }

    /// [`Self::new`] with an explicit HTTPS [`PushPoster`] (a test seam).
    pub fn with_poster(
        link: PushLink,
        identity: DeviceIdentity,
        relay_url: &str,
        relay_password: Option<String>,
        poster: Box<dyn PushPoster>,
    ) -> Self {
        PushSender {
            link,
            identity,
            endpoint: push_endpoint_url(relay_url),
            relay_password,
            poster,
        }
    }

    /// Send one sealed push: onto the relay client's outbound channel when the
    /// WebSocket can carry it, otherwise as a signed HTTPS request stamped
    /// `now_unix_ms` (the desktop's wall clock). Never blocks. Returns the
    /// route taken.
    pub fn dispatch(
        &self,
        push: PushNotifyBody,
        now_unix_ms: i64,
        outbound: &mut dyn FnMut(RemoteOutbound),
    ) -> PushRoute {
        let route = choose_route(self.link.live_version());
        match route {
            PushRoute::RelaySocket => {
                log_outcome(&push, route, "queued");
                outbound(RemoteOutbound::PushNotify(push));
            }
            PushRoute::Https => match &self.endpoint {
                Some(url) => {
                    let request = sign_push_request(&self.identity, now_unix_ms, push);
                    self.poster.post(HttpsPush {
                        url: url.clone(),
                        relay_password: self.relay_password.clone(),
                        request,
                    });
                }
                None => log_outcome(&push, route, "dropped: relay url has no https endpoint"),
            },
        }
        route
    }
}

/// One metadata-only log line per push step (never title, body or ciphertext).
fn log_outcome(push: &PushNotifyBody, route: PushRoute, outcome: &str) {
    crate::remote::debuglog::log(&format!(
        "push pairing={} event={} class={} route={route:?} {outcome}",
        push.pairing_id, push.event_id, push.class
    ));
}

#[cfg(test)]
mod tests;
