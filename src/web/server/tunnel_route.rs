//! `GET /tunnel` — one TCP connection the worker makes for the session
//! browser in a Desktop window (v11, `specs/SESSION_BROWSER_PRD.md` B4;
//! `remote-control-cefv.2`). The key came first, over the WebSocket
//! ([`crate::web::tunnel`]); this is where the bytes flow.
//!
//! In order, and every refusal before the connection is upgraded:
//!
//! 1. **The method and the upgrade**: `GET` with `Upgrade:
//!    flightdeck-tunnel`. Anything else is `405` or `426`.
//! 2. **The cookie**, checked by the same [`authenticate`] as `/ws`,
//!    `/upload` and `/download`, refused in the same words.
//! 3. **The key**, for this cookie's credential: unknown, ended or someone
//!    else's are all one `403 invalid_tunnel`; one with
//!    [`MAX_TUNNELS_PER_KEY`] open is `429 too_many_tunnels`. The window it
//!    was granted to must still be attached.
//! 4. **The target** (`X-FlightDeck-Target: host:port`), read by
//!    [`parse_target`]: malformed is `400 bad_target`.
//! 5. **The connection**, made from the worker: a name that does not resolve
//!    is `502 unresolved`, nothing listening `502 refused`, no answer within
//!    [`CONNECT_TIMEOUT`] `504 timed_out`. Each names the target, so the
//!    browser's error page can say what the worker could not reach.
//!
//! Then `101 Switching Protocols`, and bytes both ways until either side
//! closes or the key ends (closed by the window, the window gone, the worker
//! stopping). The log has one line per tunnel with byte counts and never the
//! target: where a person browses is not the worker's to keep.
//!
//! [`MAX_TUNNELS_PER_KEY`]: crate::web::tunnel::MAX_TUNNELS_PER_KEY

use std::time::Duration;

use axum::extract::Request;
use axum::http::Method;
use hyper_util::rt::TokioIo;

use super::upload_route::Failure;
use super::*;
use crate::web::tunnel::{
    parse_target, Refusal, Target, TUNNEL_KEY_HEADER, TUNNEL_TARGET_HEADER, TUNNEL_UPGRADE,
};

/// How long the worker tries to reach a target.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// `GET /tunnel`. See the module docs.
pub(super) async fn tunnel_route(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(conn): ConnectInfo<PeerInfo>,
    mut request: Request,
) -> Response {
    if request.method() != Method::GET {
        let mut response = Failure::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "A tunnel is opened with GET.",
        )
        .response();
        response
            .headers_mut()
            .insert(header::ALLOW, header::HeaderValue::from_static("GET"));
        return response;
    }
    let headers = request.headers().clone();
    let upgrade = headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case(TUNNEL_UPGRADE));
    if !upgrade {
        let mut response = Failure::new(
            StatusCode::UPGRADE_REQUIRED,
            "upgrade_required",
            "A tunnel asks to upgrade to flightdeck-tunnel.",
        )
        .response();
        response.headers_mut().insert(
            header::UPGRADE,
            header::HeaderValue::from_static(TUNNEL_UPGRADE),
        );
        return response;
    }

    let address = rate_limit_address(conn.addr);
    let token = match authenticate(&shared, conn.addr, &headers) {
        Ok(token) => token,
        Err(failure) => {
            debuglog::log(&format!(
                "web TUNNEL refused address={address} reason={}",
                failure.as_str()
            ));
            let attempts = shared
                .credentials
                .unwrap_or_recover()
                .attempts_remaining(&address);
            return refusal_response(failure, attempts, shared.now_ms());
        }
    };

    let key = headers
        .get(TUNNEL_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let opened = shared.tunnels.unwrap_or_recover().open(&key, &token);
    let (viewer, mut ended) = match opened {
        Ok(opened) => opened,
        Err(refusal) => {
            debuglog::log(&format!(
                "web TUNNEL refused address={address} reason={refusal:?}"
            ));
            return refused(refusal).response();
        }
    };
    // From here on every way out says the tunnel ended.
    let guard = OpenTunnel {
        shared: shared.clone(),
        key,
    };
    if shared.registry().viewer(&viewer).is_none() {
        return refused(Refusal::InvalidKey).response();
    }

    let Some(target) = headers
        .get(TUNNEL_TARGET_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_target)
    else {
        return Failure::new(
            StatusCode::BAD_REQUEST,
            "bad_target",
            "A tunnel names one host:port to reach.",
        )
        .response();
    };
    let upstream = match reach(&target).await {
        Ok(stream) => stream,
        Err(failure) => {
            debuglog::log(&format!("web TUNNEL failed reason={}", failure.reason()));
            return failure.response();
        }
    };

    let on_upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        let _guard = guard;
        let Ok(upgraded) = on_upgrade.await else {
            return;
        };
        let mut client = TokioIo::new(upgraded);
        let mut upstream = upstream;
        let piped = tokio::select! {
            piped = tokio::io::copy_bidirectional(&mut client, &mut upstream) => piped.ok(),
            _ = ended.wait_for(|ended| *ended) => None,
        };
        match piped {
            Some((up, down)) => debuglog::log(&format!("web TUNNEL closed up={up} down={down}")),
            None => debuglog::log("web TUNNEL ended"),
        }
    });

    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    response.headers_mut().insert(
        header::CONNECTION,
        header::HeaderValue::from_static("upgrade"),
    );
    response.headers_mut().insert(
        header::UPGRADE,
        header::HeaderValue::from_static(TUNNEL_UPGRADE),
    );
    response
}

/// One open tunnel's place under its key, given back when it ends however
/// it ends.
struct OpenTunnel {
    shared: Arc<Shared>,
    key: String,
}

impl Drop for OpenTunnel {
    fn drop(&mut self) {
        self.shared.tunnels.unwrap_or_recover().closed(&self.key);
    }
}

fn refused(refusal: Refusal) -> Failure {
    match refusal {
        Refusal::InvalidKey => Failure::new(
            StatusCode::FORBIDDEN,
            "invalid_tunnel",
            "This browser is not (or no longer) allowed to use the worker's network. \
             Open it again.",
        ),
        Refusal::TooMany => Failure::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_tunnels",
            "This browser has too many connections open through the worker.",
        ),
    }
}

/// Resolve and connect to `target` from the worker.
async fn reach(target: &Target) -> Result<tokio::net::TcpStream, Failure> {
    let shown = target.address();
    let connect = async {
        let addrs: Vec<SocketAddr> = match tokio::net::lookup_host(&shown).await {
            Ok(addrs) => addrs.collect(),
            Err(_) => Vec::new(),
        };
        if addrs.is_empty() {
            return Err(Failure::new(
                StatusCode::BAD_GATEWAY,
                "unresolved",
                format!("The worker could not find {}.", target.host),
            ));
        }
        tokio::net::TcpStream::connect(&addrs[..])
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::ConnectionRefused => Failure::new(
                    StatusCode::BAD_GATEWAY,
                    "refused",
                    format!("Nothing is listening on {shown} on the worker."),
                ),
                _ => Failure::new(
                    StatusCode::BAD_GATEWAY,
                    "connect_failed",
                    format!("The worker could not reach {shown}: {e}"),
                ),
            })
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(result) => result,
        Err(_) => Err(Failure::new(
            StatusCode::GATEWAY_TIMEOUT,
            "timed_out",
            format!("{shown} did not answer the worker."),
        )),
    }
}
