//! End-to-end tests for the embedded web server (D15, `specs/WEB_INTERFACE.md`).
//!
//! These drive a **real `TcpListener` and a real WebSocket client** through the
//! whole stack: bind, credential exchange, cookie, upgrade, attach, seats,
//! takeover, graceful shutdown, and start/stop/start. Nothing is mocked except
//! the clock-and-filesystem seams the credential store already takes, and even
//! those are real here (a temp directory and the real clock) so bootstrap-code
//! expiry behaves as it does in production.
//!
//! Every server binds **port 0** and reads the assigned port back, so the suite
//! never collides with a developer's running FlightDeck or with itself.
//!
//! The HTTP client is hand-rolled over a `TcpStream` on purpose: the crate has
//! no HTTP client dependency, and the requests under test are two lines long.
//! The WebSocket client is `tokio-tungstenite`, which is already a dependency.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use flightdeck::contracts::domain::WebConfig;
use flightdeck::contracts::real::{RealClock, RealFs};
use flightdeck::contracts::traits::Clock;
use flightdeck::remote::runtime;
use flightdeck::web::credentials::CredentialStore;
use flightdeck::web::protocol::{
    Attach, ClientMsg, Delta, ErrorCode, Input, Seat, SeatRequest, ServerMsg, ShutdownReason,
    PROTOCOL_VERSION,
};
use flightdeck::web::server::{
    self, BindExposure, HostState, ShutdownNotice, WebInbound, WebServerHandle, COOKIE_NAME,
};

/// Long enough that a loaded CI box does not flake, short enough that a genuine
/// hang fails the test rather than the suite timeout.
const WAIT: Duration = Duration::from_secs(10);

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    handle: WebServerHandle,
    credentials: Arc<Mutex<CredentialStore>>,
    inbound: std::sync::mpsc::Receiver<WebInbound>,
    /// Kept alive: the credential store writes `web.json` into it.
    _dir: tempfile::TempDir,
}

impl Harness {
    fn start() -> Harness {
        Harness::start_on(0)
    }

    fn start_on(port: u16) -> Harness {
        let dir = tempfile::tempdir().expect("a temp dir");
        let clock = Arc::new(RealClock);
        let store =
            CredentialStore::open(Arc::new(RealFs), clock.clone(), dir.path().join("web.json"));
        let credentials = Arc::new(Mutex::new(store));
        let (tx, inbound) = std::sync::mpsc::channel::<WebInbound>();
        let config = WebConfig {
            enabled: true,
            port,
            // Explicitly the default, so the exposure assertion below is about
            // the shipped behaviour and not about the test's own choice.
            bind: WebConfig::default().bind,
            replay_bytes: 262_144,
        };
        let handle = server::start(
            &config,
            Arc::clone(&credentials),
            clock as Arc<dyn Clock + Send + Sync>,
            HostState {
                host_version: "test-host".to_string(),
                replay_capacity_bytes: 262_144,
                ..HostState::default()
            },
            tx,
        )
        .expect("the test host can bind a loopback port");
        Harness {
            handle,
            credentials,
            inbound,
            _dir: dir,
        }
    }

    fn addr(&self) -> String {
        self.handle.bound_addr().to_string()
    }

    /// Mint a bootstrap code, exchange it over HTTP, and return the `Cookie`
    /// header value a browser would then present.
    async fn authenticate(&self) -> String {
        let code = self
            .credentials
            .lock()
            .expect("the store lock is not poisoned")
            .mint_bootstrap_code()
            .reveal()
            .to_string();
        let response = post_json(
            &self.addr(),
            "/auth/exchange",
            &[],
            &serde_json::json!({ "code": code, "label": "Chrome on macOS" }).to_string(),
        )
        .await;
        assert_eq!(response.status, 200, "exchange failed: {}", response.body);
        response
            .cookie(COOKIE_NAME)
            .expect("the exchange sets the access cookie")
    }

    /// Drain what the server has told the TUI so far.
    fn inbound(&self) -> Vec<WebInbound> {
        self.inbound.try_iter().collect()
    }
}

/// Run `body` on the shared runtime from this synchronous test thread — exactly
/// the seam the TUI crosses (D6).
fn on_runtime<F: std::future::Future>(body: F) -> F::Output {
    runtime::shared().handle().block_on(body)
}

// ---------------------------------------------------------------------------
// A minimal HTTP/1.1 client
// ---------------------------------------------------------------------------

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The `name=value` pair from `Set-Cookie`, without its attributes.
    fn cookie(&self, name: &str) -> Option<String> {
        let raw = self.header("set-cookie")?;
        let pair = raw.split(';').next()?.trim();
        pair.starts_with(&format!("{name}="))
            .then(|| pair.to_string())
    }

    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

async fn request(
    addr: &str,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: Option<&str>,
) -> HttpResponse {
    let mut stream = TcpStream::connect(addr).await.expect("the server is up");
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (name, value) in extra_headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        head.push_str("Content-Type: application/json\r\n");
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .await
        .expect("the request head is writable");
    if let Some(body) = body {
        stream
            .write_all(body.as_bytes())
            .await
            .expect("the request body is writable");
    }

    // `Connection: close` means the server closes when it is done, so reading to
    // EOF is the whole response and needs no chunked/length bookkeeping.
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .await
        .expect("the response is readable");
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect();
    HttpResponse {
        status,
        headers,
        body: body.to_string(),
    }
}

async fn get(addr: &str, path: &str, headers: &[(&str, &str)]) -> HttpResponse {
    request(addr, "GET", path, headers, None).await
}

async fn post_json(addr: &str, path: &str, headers: &[(&str, &str)], body: &str) -> HttpResponse {
    request(addr, "POST", path, headers, Some(body)).await
}

// ---------------------------------------------------------------------------
// A WebSocket client
// ---------------------------------------------------------------------------

/// Attempt the `/ws` upgrade, optionally presenting a cookie.
async fn ws_connect(
    addr: &str,
    cookie: Option<&str>,
) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let mut request = format!("ws://{addr}/ws")
        .into_client_request()
        .expect("the ws url is well formed");
    if let Some(cookie) = cookie {
        request.headers_mut().insert(
            "cookie",
            cookie.parse().expect("the cookie is a valid header value"),
        );
    }
    request.headers_mut().insert(
        "user-agent",
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Chrome/131.0 Safari/537.36"
            .parse()
            .expect("a valid header value"),
    );
    connect_async(request).await.map(|(ws, _response)| ws)
}

async fn send(ws: &mut Ws, msg: &ClientMsg) {
    let json = serde_json::to_string(msg).expect("a client frame serializes");
    ws.send(Message::Text(json.into()))
        .await
        .expect("the socket accepts a frame");
}

/// The next protocol frame, or `None` when the socket closed.
async fn next_frame(ws: &mut Ws) -> Option<ServerMsg> {
    loop {
        let message = tokio::time::timeout(WAIT, ws.next())
            .await
            .expect("a frame arrives before the deadline")?;
        match message {
            Ok(Message::Text(text)) => {
                return Some(
                    serde_json::from_str(text.as_str()).expect("the host speaks protocol v1"),
                )
            }
            Ok(Message::Close(_)) | Err(_) => return None,
            // Ping/pong and anything else v1 does not use.
            Ok(_) => continue,
        }
    }
}

/// Read frames until one matches, so a test can assert about the frame it cares
/// about without asserting the exact interleaving of the others (a `Snapshot` is
/// followed by a `Delta::Seats` for the row that just appeared, and both are
/// correct).
async fn frame_matching<T>(ws: &mut Ws, mut pick: impl FnMut(ServerMsg) -> Option<T>) -> T {
    for _ in 0..16 {
        let Some(frame) = next_frame(ws).await else {
            panic!("the socket closed before the expected frame arrived");
        };
        if let Some(found) = pick(frame) {
            return found;
        }
    }
    panic!("the expected frame did not arrive within 16 frames");
}

async fn attach(ws: &mut Ws, seat: SeatRequest) -> ClientMsg {
    let msg = ClientMsg::Attach(Attach {
        protocol_version: PROTOCOL_VERSION,
        seat,
        cursors: Vec::new(),
        resume_viewer: None,
        viewport: None,
        client: None,
    });
    send(ws, &msg).await;
    msg
}

async fn await_snapshot(ws: &mut Ws) -> flightdeck::web::protocol::Snapshot {
    frame_matching(ws, |frame| match frame {
        ServerMsg::Snapshot(snapshot) => Some(snapshot),
        _ => None,
    })
    .await
}

// ===========================================================================
// Binding and lifecycle
// ===========================================================================

/// D5: the shipped default is loopback, and the caller learns the port the OS
/// actually gave it.
#[test]
fn the_default_bind_is_loopback_and_the_port_is_reported_back() {
    let harness = Harness::start();
    assert!(harness.handle.bound_addr().ip().is_loopback());
    assert_ne!(harness.handle.bound_addr().port(), 0);
    assert_eq!(harness.handle.exposure(), BindExposure::Loopback);
}

/// The acceptance criterion in the issue: start, stop, and start **again** — on
/// the very same port, which only works if the listener was really released and
/// the runtime thread was not leaked.
#[test]
fn start_stop_start_rebinds_the_same_port() {
    let first = Harness::start();
    let port = first.handle.bound_addr().port();
    let addr = first.addr();
    on_runtime(async {
        let response = get(&addr, "/", &[]).await;
        assert_eq!(response.status, 200);
    });
    let Harness {
        handle,
        _dir: dir_one,
        ..
    } = first;
    handle.stop(ShutdownNotice::server_stopped());
    drop(dir_one);

    // Same port, second listener.
    let second = Harness::start_on(port);
    assert_eq!(second.handle.bound_addr().port(), port);
    let addr = second.addr();
    on_runtime(async {
        let response = get(&addr, "/", &[]).await;
        assert_eq!(
            response.status, 200,
            "the second server must serve on the port the first released"
        );
    });
    let Harness {
        handle,
        _dir: dir_two,
        ..
    } = second;
    handle.stop(ShutdownNotice::server_stopped());
    drop(dir_two);

    // And a third time, to prove nothing accumulated.
    let third = Harness::start_on(port);
    assert_eq!(third.handle.bound_addr().port(), port);
}

/// A handle that goes out of scope without `stop` must still release the port,
/// or a crash path would leave FlightDeck unable to restart its own server.
#[test]
fn dropping_the_handle_releases_the_listener() {
    let first = Harness::start();
    let port = first.handle.bound_addr().port();
    drop(first);

    // Poll: `drop` signals without waiting, so allow the task a moment to unwind.
    let mut rebound = None;
    for _ in 0..100 {
        match std::panic::catch_unwind(|| Harness::start_on(port)) {
            Ok(harness) => {
                rebound = Some(harness);
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let rebound = rebound.expect("the dropped handle released its port");
    assert_eq!(rebound.handle.bound_addr().port(), port);
}

// ===========================================================================
// Assets (D9)
// ===========================================================================

/// `NotBuilt` must be an explanation, never a blank page — and either way the
/// root serves HTML, so this test does not depend on whether `npm run build`
/// happened before `cargo test` in this invocation.
#[test]
fn the_root_serves_html_whether_or_not_the_spa_was_built() {
    let harness = Harness::start();
    let addr = harness.addr();
    on_runtime(async {
        let response = get(&addr, "/", &[]).await;
        assert_eq!(response.status, 200);
        let content_type = response
            .header("content-type")
            .expect("an asset response has a content type");
        assert!(content_type.contains("text/html"), "{content_type}");
        assert!(
            !response.body.is_empty(),
            "a blank page is the one thing D9 rules out"
        );

        // An extensionless path is a client-side route and falls back to the
        // shell; a missing concrete asset is an honest 404.
        let route = get(&addr, "/session/42", &[]).await;
        assert_eq!(route.status, 200);
        let missing = get(&addr, "/assets/nope-abc123.js", &[]).await;
        assert!(
            missing.status == 404 || missing.status == 200,
            "a concrete asset either exists or 404s, never hangs: {}",
            missing.status
        );
    });
}

// ===========================================================================
// The credential exchange (Q4)
// ===========================================================================

#[test]
fn a_correct_code_is_exchanged_for_an_httponly_cookie() {
    let harness = Harness::start();
    let addr = harness.addr();
    let code = harness
        .credentials
        .lock()
        .expect("lock")
        .mint_bootstrap_code()
        .reveal()
        .to_string();

    on_runtime(async {
        let response = post_json(
            &addr,
            "/auth/exchange",
            &[],
            &serde_json::json!({ "code": code, "label": "Chrome on macOS" }).to_string(),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
        assert_eq!(response.json()["ok"], serde_json::json!(true));

        let set_cookie = response
            .header("set-cookie")
            .expect("the exchange sets a cookie");
        assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
        assert!(set_cookie.contains("SameSite=Lax"), "{set_cookie}");
        assert!(
            !set_cookie.contains("Secure"),
            "plain HTTP on loopback: a Secure cookie would never come back: {set_cookie}"
        );
        assert_eq!(response.header("cache-control"), Some("no-store"));
    });

    // The code was minted in a POST body, so it never appeared in a request
    // line — and the token is now persisted, so the cookie survives a restart.
    assert_eq!(
        harness
            .credentials
            .lock()
            .expect("lock")
            .active_tokens()
            .count(),
        1
    );
}

#[test]
fn a_wrong_code_is_refused_and_the_address_is_rate_limited() {
    let harness = Harness::start();
    let addr = harness.addr();
    harness
        .credentials
        .lock()
        .expect("lock")
        .mint_bootstrap_code();

    on_runtime(async {
        // `RATE_LIMIT_MAX_FAILURES` is 3, so three misses spend the budget.
        for attempt in 1..=3 {
            let response = post_json(
                &addr,
                "/auth/exchange",
                &[],
                &serde_json::json!({ "code": "0000" }).to_string(),
            )
            .await;
            assert_eq!(response.status, 401, "attempt {attempt}: {}", response.body);
            let body = response.json();
            assert_eq!(body["ok"], serde_json::json!(false));
            assert_eq!(body["screen"], serde_json::json!("rejected"));
            assert!(
                body["attempts_remaining"].as_u64().is_some(),
                "artboard 2b counts down: {body}"
            );
        }

        // The limiter inside `CredentialStore` is consulted on the HTTP path, so
        // the fourth attempt never reaches the digits at all.
        let limited = post_json(
            &addr,
            "/auth/exchange",
            &[],
            &serde_json::json!({ "code": "0000" }).to_string(),
        )
        .await;
        assert_eq!(limited.status, 429, "{}", limited.body);
        let body = limited.json();
        assert_eq!(body["screen"], serde_json::json!("rate_limited"));
        assert!(
            body["retry_after_ms"].as_u64().unwrap_or(0) > 0,
            "the browser needs a countdown: {body}"
        );
        assert!(limited.header("retry-after").is_some());
    });
}

/// A forwarding header must not mint a fresh attempt budget. If the server
/// trusted one, three guesses per forged address would make the limiter
/// decorative.
#[test]
fn a_forged_forwarding_header_cannot_reset_the_attempt_budget() {
    let harness = Harness::start();
    let addr = harness.addr();
    harness
        .credentials
        .lock()
        .expect("lock")
        .mint_bootstrap_code();

    on_runtime(async {
        for (index, forged) in ["203.0.113.1", "198.51.100.7", "192.0.2.9"]
            .into_iter()
            .enumerate()
        {
            let response = post_json(
                &addr,
                "/auth/exchange",
                &[
                    ("X-Forwarded-For", forged),
                    ("X-Real-IP", forged),
                    ("Forwarded", "for=203.0.113.1"),
                ],
                &serde_json::json!({ "code": "0000" }).to_string(),
            )
            .await;
            assert_eq!(response.status, 401, "attempt {index}: {}", response.body);
        }

        let limited = post_json(
            &addr,
            "/auth/exchange",
            &[("X-Forwarded-For", "203.0.113.99")],
            &serde_json::json!({ "code": "0000" }).to_string(),
        )
        .await;
        assert_eq!(
            limited.status, 429,
            "every attempt shares the socket's budget, whatever the headers claim: {}",
            limited.body
        );
    });
}

#[test]
fn a_code_is_single_use_and_expires_into_a_rejection() {
    let harness = Harness::start();
    let addr = harness.addr();
    let code = harness
        .credentials
        .lock()
        .expect("lock")
        .mint_bootstrap_code()
        .reveal()
        .to_string();

    on_runtime(async {
        let body = serde_json::json!({ "code": code }).to_string();
        let first = post_json(&addr, "/auth/exchange", &[], &body).await;
        assert_eq!(first.status, 200, "{}", first.body);

        let replay = post_json(&addr, "/auth/exchange", &[], &body).await;
        assert_eq!(replay.status, 401, "{}", replay.body);
        assert_eq!(
            replay.json()["reason"],
            serde_json::json!("code_already_used"),
            "a replayed code must be named as spent, not merely wrong: {}",
            replay.body
        );
    });
}

/// A malformed body is not a credential attempt, so it must not be able to lock
/// a browser out.
#[test]
fn a_malformed_exchange_body_does_not_spend_the_budget() {
    let harness = Harness::start();
    let addr = harness.addr();
    let code = harness
        .credentials
        .lock()
        .expect("lock")
        .mint_bootstrap_code()
        .reveal()
        .to_string();

    on_runtime(async {
        for _ in 0..5 {
            let junk = post_json(&addr, "/auth/exchange", &[], "not json").await;
            assert_eq!(junk.status, 400, "{}", junk.body);
        }
        // The real code still works, which it would not if the junk had counted.
        let good = post_json(
            &addr,
            "/auth/exchange",
            &[],
            &serde_json::json!({ "code": code }).to_string(),
        )
        .await;
        assert_eq!(good.status, 200, "{}", good.body);
    });
}

#[test]
fn the_session_probe_reports_whether_the_cookie_still_works() {
    let harness = Harness::start();
    let addr = harness.addr();
    on_runtime(async {
        let anonymous = get(&addr, "/auth/session", &[]).await;
        assert_eq!(anonymous.status, 401);
        assert_eq!(
            anonymous.json()["authenticated"],
            serde_json::json!(false),
            "{}",
            anonymous.body
        );
        assert_eq!(
            anonymous.json()["screen"],
            serde_json::json!("code_entry"),
            "no cookie is the plain code-entry case, not a rejection"
        );
    });

    let cookie = on_runtime(harness.authenticate());
    on_runtime(async {
        let signed_in = get(&addr, "/auth/session", &[("Cookie", &cookie)]).await;
        assert_eq!(signed_in.status, 200, "{}", signed_in.body);
        assert_eq!(signed_in.json()["authenticated"], serde_json::json!(true));
    });
}

// ===========================================================================
// The WebSocket upgrade
// ===========================================================================

/// The security property the whole surface rests on: **no cookie, no socket.**
#[test]
fn an_unauthenticated_ws_upgrade_is_refused() {
    let harness = Harness::start();
    let addr = harness.addr();
    on_runtime(async {
        let error = ws_connect(&addr, None)
            .await
            .expect_err("an unauthenticated upgrade must be refused");
        match error {
            tokio_tungstenite::tungstenite::Error::Http(response) => {
                assert_eq!(response.status().as_u16(), 401);
            }
            other => panic!("expected an HTTP refusal, got {other:?}"),
        }

        // A forged cookie is refused the same way — the absence of a cookie is
        // not a distinguishable state.
        let error = ws_connect(&addr, Some(&format!("{COOKIE_NAME}=made-up")))
            .await
            .expect_err("a forged cookie must be refused");
        assert!(matches!(
            error,
            tokio_tungstenite::tungstenite::Error::Http(_)
        ));
    });
    assert!(
        harness.inbound().is_empty(),
        "a refused upgrade must not produce a viewer"
    );
}

#[test]
fn a_revoked_cookie_is_refused_and_named_as_revoked() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    // Rotation (D5) revokes every token and mints a fresh code.
    let (_code, error) = harness.credentials.lock().expect("lock").rotate();
    assert!(error.is_none(), "the rotation persisted: {error:?}");

    on_runtime(async {
        let probe = get(&addr, "/auth/session", &[("Cookie", &cookie)]).await;
        assert_eq!(probe.status, 401, "{}", probe.body);
        assert_eq!(
            probe.json()["screen"],
            serde_json::json!("revoked"),
            "someone withdrew access; that is a decision, not a typo: {}",
            probe.body
        );

        let error = ws_connect(&addr, Some(&cookie))
            .await
            .expect_err("a revoked cookie must not open a socket");
        assert!(matches!(
            error,
            tokio_tungstenite::tungstenite::Error::Http(_)
        ));
    });
}

#[test]
fn an_authenticated_attach_is_answered_with_a_snapshot() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie))
            .await
            .expect("an authenticated upgrade succeeds");
        attach(&mut ws, SeatRequest::Control).await;

        let snapshot = await_snapshot(&mut ws).await;
        assert_eq!(snapshot.protocol_version, PROTOCOL_VERSION);
        assert_eq!(snapshot.host_version, "test-host");
        assert_eq!(snapshot.seat, Seat::Controlling);
        assert_eq!(snapshot.replay_capacity_bytes, 262_144);
        assert_eq!(snapshot.last_input_seq, 0, "a fresh tab has typed nothing");
        assert!(snapshot.server_time_ms > 0);

        // `desktop + this tab` (2f).
        assert_eq!(snapshot.seats.len(), 2, "{:?}", snapshot.seats);
        assert_eq!(snapshot.seats[0].viewer_id, None);
        assert_eq!(snapshot.seats[0].label, "desktop");
        let you = snapshot
            .seats
            .iter()
            .find(|s| s.is_you)
            .expect("a `you` row");
        assert_eq!(you.viewer_id.as_ref(), Some(&snapshot.viewer_id));
        assert!(
            you.label.starts_with("127.0.0.1"),
            "the label carries the address the host observed: {}",
            you.label
        );
        assert!(
            you.label.contains("Chrome on macOS"),
            "and a coarse user agent: {}",
            you.label
        );
    });

    // The TUI was told, and told enough to render the chip and answer a replay.
    let events = harness.inbound();
    let attached = events
        .iter()
        .find_map(|event| match event {
            WebInbound::ViewerAttached { seat, address, .. } => Some((*seat, *address)),
            _ => None,
        })
        .expect("the TUI is told a viewer attached");
    assert_eq!(attached.0, Seat::Controlling);
    assert!(attached.1.is_loopback());
    assert!(
        events
            .iter()
            .any(|event| matches!(event, WebInbound::SeatsChanged { .. })),
        "a seat change reaches the desktop too: {events:?}"
    );
}

/// A version this build cannot serve is answered "reload to update", not a
/// retry.
#[test]
fn an_unsupported_protocol_version_is_refused_with_the_numbers() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        send(
            &mut ws,
            &ClientMsg::Attach(Attach {
                protocol_version: PROTOCOL_VERSION + 7,
                seat: SeatRequest::Control,
                cursors: Vec::new(),
                resume_viewer: None,
                viewport: None,
                client: None,
            }),
        )
        .await;

        let error = frame_matching(&mut ws, |frame| match frame {
            ServerMsg::Error(error) => Some(error),
            _ => None,
        })
        .await;
        assert_eq!(error.code, ErrorCode::VersionMismatch);
        let version = error.version.expect("the numbers the reload prompt needs");
        assert_eq!(version.peer, PROTOCOL_VERSION + 7);
        assert_eq!(version.max_supported, PROTOCOL_VERSION);

        assert!(
            next_frame(&mut ws).await.is_none(),
            "nothing this build can serve, so the socket closes"
        );
    });
}

// ===========================================================================
// Seats and takeover (D14)
// ===========================================================================

#[test]
fn a_second_controller_is_refused_then_takes_over_and_the_first_becomes_an_observer() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut first = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut first, SeatRequest::Control).await;
        let first_snapshot = await_snapshot(&mut first).await;
        assert_eq!(first_snapshot.seat, Seat::Controlling);

        let mut second = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut second, SeatRequest::Control).await;

        // Refused, with the incumbent named so the takeover prompt can render.
        let refusal = frame_matching(&mut second, |frame| match frame {
            ServerMsg::Error(error) => Some(error),
            _ => None,
        })
        .await;
        assert_eq!(refusal.code, ErrorCode::SeatHeld);
        let incumbent = refusal.incumbent.expect("who holds the seat");
        assert_eq!(
            incumbent.viewer_id.as_ref(),
            Some(&first_snapshot.viewer_id)
        );
        assert_eq!(incumbent.seat, Seat::Controlling);

        // Takeover has no dedicated frame: the client re-sends `Attach`.
        attach(&mut second, SeatRequest::TakeOver).await;
        let second_snapshot = await_snapshot(&mut second).await;
        assert_eq!(second_snapshot.seat, Seat::Controlling);
        assert_ne!(second_snapshot.viewer_id, first_snapshot.viewer_id);

        // Eviction is a `Delta::Seats`, never a `Shutdown`: the evicted socket
        // stays open, watching read-only (2f).
        let (you, seats) = frame_matching(&mut first, |frame| match frame {
            ServerMsg::Delta(Delta::Seats { you, seats }) if you == Seat::Observing => {
                Some((you, seats))
            }
            _ => None,
        })
        .await;
        assert_eq!(you, Seat::Observing);
        assert_eq!(seats.len(), 3, "desktop + two tabs: {seats:?}");
        let web_controllers: Vec<_> = seats
            .iter()
            .filter(|row| row.viewer_id.is_some() && row.seat == Seat::Controlling)
            .collect();
        assert_eq!(web_controllers.len(), 1, "exactly one browser drives");
        assert_eq!(
            web_controllers[0].viewer_id.as_ref(),
            Some(&second_snapshot.viewer_id)
        );

        // The evicted socket is still usable — it just cannot type.
        send(
            &mut first,
            &ClientMsg::Input(Input {
                seq: 1,
                terminal_id: "t1".into(),
                data: b"ls\r".to_vec(),
            }),
        )
        .await;
        let ack = frame_matching(&mut first, |frame| match frame {
            ServerMsg::Ack(ack) => Some(ack),
            _ => None,
        })
        .await;
        assert_eq!(ack.seq, 1);
        assert_eq!(
            ack.outcome,
            flightdeck::web::protocol::AckOutcome::Ignored,
            "a keystroke is acked, never silently dropped (§5.1)"
        );
    });

    // No observer input ever reached the host's input seam.
    assert!(
        !harness
            .inbound()
            .iter()
            .any(|event| matches!(event, WebInbound::Input { .. })),
        "an observer's keystrokes must not reach the PTY seam"
    );
}

#[test]
fn an_observer_never_contends_for_the_seat() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut driver = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut driver, SeatRequest::Control).await;
        let driving = await_snapshot(&mut driver).await;

        let mut watchers = Vec::new();
        for _ in 0..3 {
            let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
            attach(&mut ws, SeatRequest::Observe).await;
            let snapshot = await_snapshot(&mut ws).await;
            assert_eq!(snapshot.seat, Seat::Observing);
            watchers.push(ws);
        }

        // The driver still drives: N observers cost nothing in arbitration.
        send(
            &mut driver,
            &ClientMsg::Command(flightdeck::web::protocol::Command {
                seq: 5,
                name: flightdeck::web::protocol::command::REQUEST_SNAPSHOT.to_string(),
                args: None,
            }),
        )
        .await;
        let again = await_snapshot(&mut driver).await;
        assert_eq!(again.seat, Seat::Controlling);
        assert_eq!(again.viewer_id, driving.viewer_id);
        assert_eq!(
            again.seats.len(),
            5,
            "desktop + four tabs: {:?}",
            again.seats
        );
    });
}

/// Read-only means read-only: D3 makes the selection shared with the desktop, so
/// letting an observer move it would be input by another name.
#[test]
fn an_observers_command_is_refused_as_read_only() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Observe).await;
        await_snapshot(&mut ws).await;

        send(
            &mut ws,
            &ClientMsg::Command(flightdeck::web::protocol::Command {
                seq: 9,
                name: flightdeck::web::protocol::command::SELECT_SESSION.to_string(),
                args: Some(serde_json::json!({ "session_id": "s1" })),
            }),
        )
        .await;
        let error = frame_matching(&mut ws, |frame| match frame {
            ServerMsg::Error(error) => Some(error),
            _ => None,
        })
        .await;
        assert_eq!(error.code, ErrorCode::ReadOnly);
        assert_eq!(
            error.seq,
            Some(9),
            "the browser must fail the right queued item"
        );
    });

    assert!(
        !harness
            .inbound()
            .iter()
            .any(|event| matches!(event, WebInbound::Command { .. })),
        "an observer's command must not reach the host"
    );
}

/// The M2 door's failure mode: an unknown command is refused clearly and the
/// socket survives.
#[test]
fn an_unknown_command_is_not_supported_rather_than_fatal() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Control).await;
        await_snapshot(&mut ws).await;

        send(
            &mut ws,
            &ClientMsg::Command(flightdeck::web::protocol::Command {
                seq: 3,
                name: "git_merge_back".to_string(),
                args: None,
            }),
        )
        .await;
        let error = frame_matching(&mut ws, |frame| match frame {
            ServerMsg::Error(error) => Some(error),
            _ => None,
        })
        .await;
        assert_eq!(error.code, ErrorCode::NotSupported);
        assert_eq!(error.seq, Some(3));

        // Still alive: `request_snapshot` still answers.
        send(
            &mut ws,
            &ClientMsg::Command(flightdeck::web::protocol::Command {
                seq: 4,
                name: flightdeck::web::protocol::command::REQUEST_SNAPSHOT.to_string(),
                args: None,
            }),
        )
        .await;
        await_snapshot(&mut ws).await;
    });
}

#[test]
fn releasing_the_seat_frees_it_for_the_next_browser() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut first = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut first, SeatRequest::Control).await;
        await_snapshot(&mut first).await;

        send(
            &mut first,
            &ClientMsg::Command(flightdeck::web::protocol::Command {
                seq: 1,
                name: flightdeck::web::protocol::command::RELEASE_SEAT.to_string(),
                args: None,
            }),
        )
        .await;
        // Skip the `Delta::Seats` that the attach itself produced (`you:
        // controlling`) and wait for the one the release produced.
        let (you, _) = frame_matching(&mut first, |frame| match frame {
            ServerMsg::Delta(Delta::Seats { you, seats }) if you == Seat::Observing => {
                Some((you, seats))
            }
            _ => None,
        })
        .await;
        assert_eq!(you, Seat::Observing);

        // The seat is free, so the next browser gets it without a takeover.
        let mut second = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut second, SeatRequest::Control).await;
        assert_eq!(await_snapshot(&mut second).await.seat, Seat::Controlling);
    });
}

/// The controlling viewer's keystrokes reach the host's input seam — the point
/// `src/web/stream.rs` plugs into — and the server does not ack on the
/// applier's behalf.
#[test]
fn the_controllers_input_reaches_the_host_seam() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Control).await;
        await_snapshot(&mut ws).await;
        send(
            &mut ws,
            &ClientMsg::Input(Input {
                seq: 11,
                terminal_id: "t-agent".into(),
                data: b"echo hi\r".to_vec(),
            }),
        )
        .await;
        send(
            &mut ws,
            &ClientMsg::Resize(flightdeck::web::protocol::Resize {
                viewport: flightdeck::web::protocol::Viewport {
                    cols: 200,
                    rows: 60,
                },
            }),
        )
        .await;
        // Give the server a moment to forward both before we drain.
        tokio::time::sleep(Duration::from_millis(150)).await;
    });

    let events = harness.inbound();
    let input = events
        .iter()
        .find_map(|event| match event {
            WebInbound::Input { input, .. } => Some(input),
            _ => None,
        })
        .expect("the controller's keystrokes reach the host");
    assert_eq!(input.seq, 11);
    assert_eq!(input.terminal_id.as_str(), "t-agent");
    assert_eq!(input.data, b"echo hi\r".to_vec());

    let viewport = events
        .iter()
        .find_map(|event| match event {
            WebInbound::Resize { viewport, .. } => Some(*viewport),
            _ => None,
        })
        .expect("a viewport report reaches the host, for display only");
    assert_eq!(viewport.cols, 200);
}

/// The other half of the `stream.rs` seam: the host pushes frames at viewers.
#[test]
fn the_host_can_push_frames_to_all_viewers_and_to_one() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut driver = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut driver, SeatRequest::Control).await;
        let driving = await_snapshot(&mut driver).await;
        let mut watcher = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut watcher, SeatRequest::Observe).await;
        let watching = await_snapshot(&mut watcher).await;

        // Terminal bytes fan out to everyone (an observer sees output, D14).
        harness
            .handle
            .send(server::WebOutbound::All(ServerMsg::TermBytes(
                flightdeck::web::protocol::TermBytes::live("t1".into(), 0, b"hello".to_vec()),
            )));
        for ws in [&mut driver, &mut watcher] {
            let bytes = frame_matching(ws, |frame| match frame {
                ServerMsg::TermBytes(bytes) => Some(bytes),
                _ => None,
            })
            .await;
            assert_eq!(bytes.data, b"hello".to_vec());
            assert_eq!(bytes.offset, 0);
            assert!(!bytes.truncated);
        }

        // An ack goes to exactly one viewer.
        harness.handle.send(server::WebOutbound::Viewer {
            viewer_id: driving.viewer_id.clone(),
            msg: ServerMsg::Ack(flightdeck::web::protocol::Ack {
                seq: 42,
                outcome: flightdeck::web::protocol::AckOutcome::Applied,
                detail: None,
            }),
        });
        let ack = frame_matching(&mut driver, |frame| match frame {
            ServerMsg::Ack(ack) => Some(ack),
            _ => None,
        })
        .await;
        assert_eq!(ack.seq, 42);

        // And `Controller` finds whoever holds the seat, which is not the
        // observer.
        harness
            .handle
            .send(server::WebOutbound::Controller(ServerMsg::Delta(
                Delta::Geometry {
                    terminal_id: "t1".into(),
                    geometry: flightdeck::web::protocol::Geometry {
                        cols: 100,
                        rows: 30,
                    },
                },
            )));
        let geometry = frame_matching(&mut driver, |frame| match frame {
            ServerMsg::Delta(Delta::Geometry { geometry, .. }) => Some(geometry),
            _ => None,
        })
        .await;
        assert_eq!(geometry.cols, 100);
        assert_ne!(watching.viewer_id, driving.viewer_id);
    });
}

#[test]
fn published_state_is_what_the_next_attach_sees() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    harness.handle.publish_state(HostState {
        host_version: "9.9.9-published".to_string(),
        geometry: flightdeck::web::protocol::Geometry {
            cols: 120,
            rows: 34,
        },
        replay_capacity_bytes: 4096,
        ..HostState::default()
    });

    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Control).await;
        let snapshot = await_snapshot(&mut ws).await;
        assert_eq!(snapshot.host_version, "9.9.9-published");
        assert_eq!(snapshot.geometry.cols, 120);
        assert_eq!(snapshot.replay_capacity_bytes, 4096);
    });
}

// ===========================================================================
// Graceful shutdown (Q5)
// ===========================================================================

/// The frame has to arrive **before** the socket closes, or the browser cannot
/// tell a deliberate quit from a network failure — which is Q5's entire point.
#[test]
fn a_graceful_stop_sends_shutdown_before_the_socket_closes() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    // Attach, then hand the socket to a task that reports every frame back to
    // this thread, so `stop` (which blocks) can run while the socket is live.
    let (frames_tx, frames_rx) = std::sync::mpsc::channel::<Option<ServerMsg>>();
    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Control).await;
        await_snapshot(&mut ws).await;
        tokio::spawn(async move {
            loop {
                let frame = next_frame(&mut ws).await;
                let closed = frame.is_none();
                if frames_tx.send(frame).is_err() || closed {
                    break;
                }
            }
        });
    });

    let Harness {
        handle, _dir: dir, ..
    } = harness;
    let port = handle.bound_addr().port();
    handle.stop(ShutdownNotice::host_quit(None));

    // The `Shutdown` frame, then the close — in that order.
    let mut seen_shutdown = false;
    loop {
        match frames_rx
            .recv_timeout(WAIT)
            .expect("the socket reports either a frame or its close")
        {
            Some(ServerMsg::Shutdown {
                reason,
                self_initiated,
                ..
            }) => {
                assert_eq!(reason, ShutdownReason::HostQuit);
                assert!(
                    !self_initiated,
                    "this browser did not ask for the quit, so it sees a failure screen"
                );
                assert!(
                    !reason.should_retry(),
                    "the browser must stop retrying against a host that is gone"
                );
                seen_shutdown = true;
            }
            Some(_) => continue,
            None => break,
        }
    }
    assert!(
        seen_shutdown,
        "the listener closed without telling the browser why"
    );

    // And the port is genuinely released, so `stop` really did finish.
    drop(dir);
    let restarted = Harness::start_on(port);
    assert_eq!(restarted.handle.bound_addr().port(), port);
}

/// Q5's two screens from one frame: the tab that pressed `Ctrl-q` sees an
/// acknowledgement of its own action, not a failure.
#[test]
fn the_viewer_that_asked_for_the_quit_is_told_it_was_its_own_doing() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    let (frames_tx, frames_rx) = std::sync::mpsc::channel::<Option<ServerMsg>>();
    let (id_tx, id_rx) = std::sync::mpsc::channel::<flightdeck::web::protocol::ViewerId>();
    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Control).await;
        let snapshot = await_snapshot(&mut ws).await;
        id_tx.send(snapshot.viewer_id).expect("the id is reported");
        tokio::spawn(async move {
            loop {
                let frame = next_frame(&mut ws).await;
                let closed = frame.is_none();
                if frames_tx.send(frame).is_err() || closed {
                    break;
                }
            }
        });
    });
    let viewer_id = id_rx.recv_timeout(WAIT).expect("the viewer id");

    let Harness { handle, .. } = harness;
    handle.stop(ShutdownNotice::host_quit(Some(viewer_id)));

    let mut self_initiated_seen = false;
    while let Ok(Some(frame)) = frames_rx.recv_timeout(WAIT) {
        if let ServerMsg::Shutdown {
            self_initiated,
            reason,
            ..
        } = frame
        {
            assert_eq!(reason, ShutdownReason::HostQuit);
            self_initiated_seen = self_initiated;
            break;
        }
    }
    assert!(
        self_initiated_seen,
        "the tab that pressed Ctrl-q must be told it was its own action"
    );
}

/// `Stop Web Interface` (D10) is a different reason from `Ctrl-q`, because the
/// agents are still alive and the desktop is still usable.
#[test]
fn stopping_only_the_web_interface_says_so() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    let (frames_tx, frames_rx) = std::sync::mpsc::channel::<Option<ServerMsg>>();
    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Observe).await;
        await_snapshot(&mut ws).await;
        tokio::spawn(async move {
            loop {
                let frame = next_frame(&mut ws).await;
                let closed = frame.is_none();
                if frames_tx.send(frame).is_err() || closed {
                    break;
                }
            }
        });
    });

    let Harness { handle, .. } = harness;
    handle.stop(ShutdownNotice::server_stopped());

    let mut reason_seen = None;
    while let Ok(Some(frame)) = frames_rx.recv_timeout(WAIT) {
        if let ServerMsg::Shutdown { reason, .. } = frame {
            reason_seen = Some(reason);
            break;
        }
    }
    assert_eq!(reason_seen, Some(ShutdownReason::ServerStopped));
}

/// Several observers must **all** be told, not just the first one drained.
#[test]
fn every_attached_viewer_is_told_about_the_shutdown() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    let (frames_tx, frames_rx) = std::sync::mpsc::channel::<ShutdownReason>();
    on_runtime(async {
        for index in 0..3 {
            let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
            let seat = if index == 0 {
                SeatRequest::Control
            } else {
                SeatRequest::Observe
            };
            attach(&mut ws, seat).await;
            await_snapshot(&mut ws).await;
            let tx = frames_tx.clone();
            tokio::spawn(async move {
                while let Some(frame) = next_frame(&mut ws).await {
                    if let ServerMsg::Shutdown { reason, .. } = frame {
                        let _ = tx.send(reason);
                        break;
                    }
                }
            });
        }
    });
    drop(frames_tx);

    let Harness { handle, .. } = harness;
    handle.stop(ShutdownNotice::host_quit(None));

    let mut told = 0;
    while let Ok(reason) = frames_rx.recv_timeout(WAIT) {
        assert_eq!(reason, ShutdownReason::HostQuit);
        told += 1;
        if told == 3 {
            break;
        }
    }
    assert_eq!(told, 3, "all three viewers must be told before the close");
}
