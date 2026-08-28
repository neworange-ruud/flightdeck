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

use flightdeck::agents::status::DisplayStatus;
use flightdeck::contracts::domain::WebConfig;
use flightdeck::contracts::real::{RealClock, RealFs};
use flightdeck::contracts::traits::Clock;
use flightdeck::contracts::{InterpretedStatus, ProcessState, TabId};
use flightdeck::remote::runtime;
use flightdeck::web::access::{AccessKey, AccessOutcome, WebAccess};
use flightdeck::web::activity::{apply_mark_read, ActivityStore, Transition};
use flightdeck::web::credentials::CredentialStore;
use flightdeck::web::interfaces::FakeInterfaceEnumerator;
use flightdeck::web::protocol::{
    AckOutcome, ActivityTier, Attach, ClientMsg, Command as WireCommand, Delta, ErrorCode, Input,
    ProjectId, Seat, SeatRequest, ServerMsg, ShutdownReason, PROTOCOL_VERSION,
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
                    serde_json::from_str(text.as_str()).expect("the host speaks the web protocol"),
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
        ServerMsg::Snapshot(snapshot) => Some(*snapshot),
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
            // 2b's footer — "3 attempts left before this address is
            // rate-limited **for 60s**" — needs the lockout length *before* the
            // limiter has ever fired, which `retry_after_ms` cannot supply. The
            // browser used to mirror both numbers as TypeScript constants that
            // would drift; they are host-sent now, like `attempts_remaining`.
            assert_eq!(
                body["lockout_seconds"].as_u64(),
                Some(60),
                "the lockout length, before the limiter fires: {body}"
            );
            assert_eq!(
                body["code_ttl_seconds"].as_u64(),
                Some(120),
                "2b: \"Codes last 120 seconds and only work once\": {body}"
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
        let body = probe.json();
        assert_eq!(
            body["screen"],
            serde_json::json!("revoked"),
            "someone withdrew access; that is a decision, not a typo: {}",
            probe.body
        );
        // 2b: "withdrew this browser's access **12s ago**". The host knows when,
        // so it says when — paired with its own clock, so the browser subtracts
        // two host timestamps rather than measuring a host instant with its own.
        let revoked_at_ms = body["revoked_at_ms"]
            .as_i64()
            .unwrap_or_else(|| panic!("2b needs the revocation time: {}", probe.body));
        let server_time_ms = body["server_time_ms"]
            .as_i64()
            .expect("paired with the host's own clock");
        assert!(
            revoked_at_ms > 0,
            "never a fabricated 1970: {revoked_at_ms}"
        );
        assert!(
            server_time_ms >= revoked_at_ms,
            "the revocation cannot be in the host's future: {body}"
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
        attach(&mut ws, SeatRequest::Write).await;

        let snapshot = await_snapshot(&mut ws).await;
        assert_eq!(snapshot.protocol_version, PROTOCOL_VERSION);
        assert_eq!(snapshot.host_version, "test-host");
        assert_eq!(snapshot.seat, Seat::Writing);
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

        // Artboard 2f's arriving-viewer panel lists address / browser /
        // connected as three rows, so each arrives in its own field. The
        // browser never splits the one-line label on its separator: a
        // user-agent string is attacker-supplied and can contain one.
        assert_eq!(
            you.address.as_deref(),
            Some("127.0.0.1"),
            "the address the host observed on the socket"
        );
        assert_eq!(
            you.user_agent_label.as_deref(),
            Some("Chrome on macOS"),
            "the browser's own claim, in its own field"
        );
        assert_eq!(
            snapshot.seats[0].address, None,
            "the desktop arrived over no socket, so it has no address to report"
        );
        assert_eq!(snapshot.seats[0].user_agent_label, None);
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
    assert_eq!(attached.0, Seat::Writing);
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
                seat: SeatRequest::Write,
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

/// D14 as revised, end to end over two real sockets: **both browsers are
/// seated**, the one that is mid-burst holds the turn, the other is refused by
/// name, and `TakeOver` is the one thing that cuts in.
#[test]
fn a_second_writer_is_seated_refused_by_name_and_can_take_the_turn() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut first = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut first, SeatRequest::Write).await;
        let first_snapshot = await_snapshot(&mut first).await;
        assert_eq!(first_snapshot.seat, Seat::Writing);

        let mut second = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut second, SeatRequest::Write).await;
        let second_snapshot = await_snapshot(&mut second).await;
        assert_eq!(
            second_snapshot.seat,
            Seat::Writing,
            "a writer's seat is a role now, and asking for it is never refused"
        );
        assert_ne!(second_snapshot.viewer_id, first_snapshot.viewer_id);
        assert_eq!(
            second_snapshot
                .seats
                .iter()
                .filter(|row| row.viewer_id.is_some() && row.seat == Seat::Writing)
                .count(),
            2,
            "two browsers seated as writers: {:?}",
            second_snapshot.seats
        );
        assert!(
            second_snapshot.seats.iter().all(|row| !row.holds_input),
            "and nobody has typed yet, so the turn is free: {:?}",
            second_snapshot.seats
        );

        // The first one types, which is how the lock is claimed.
        send(
            &mut first,
            &ClientMsg::Input(Input {
                seq: 1,
                terminal_id: "t1".into(),
                data: b"hel".to_vec(),
            }),
        )
        .await;
        settle().await;

        // The second one types into that live burst and is refused — by name,
        // and without losing its seat.
        send(
            &mut second,
            &ClientMsg::Input(Input {
                seq: 1,
                terminal_id: "t1".into(),
                data: b"wor".to_vec(),
            }),
        )
        .await;
        let ack = next_ack(&mut second).await;
        assert_eq!(ack.seq, 1);
        assert_eq!(
            ack.outcome,
            flightdeck::web::protocol::AckOutcome::Rejected,
            "refused, never interleaved and never silently dropped (§5.1)"
        );
        let refusal = frame_matching(&mut second, |frame| match frame {
            ServerMsg::Error(error) => Some(error),
            _ => None,
        })
        .await;
        assert_eq!(refusal.code, ErrorCode::SeatHeld);
        let holder = refusal.incumbent.expect("who is typing");
        assert_eq!(holder.viewer_id.as_ref(), Some(&first_snapshot.viewer_id));
        assert!(holder.holds_input);
        assert!(
            refusal.message.contains("typing"),
            "the refusal says who is typing, not merely that something failed: {}",
            refusal.message
        );

        // Takeover has no dedicated frame: the client re-sends `Attach`. It
        // takes the *turn*, and demotes nobody.
        attach(&mut second, SeatRequest::TakeOver).await;
        let after = await_snapshot(&mut second).await;
        assert_eq!(after.seat, Seat::Writing);

        let (you, seats, server_time_ms, preempted) =
            frame_matching(&mut first, |frame| match frame {
                ServerMsg::Delta(Delta::Seats {
                    you,
                    seats,
                    server_time_ms,
                    you_were_preempted,
                }) if seats.iter().any(|row| {
                    row.holds_input && row.viewer_id.as_ref() == Some(&after.viewer_id)
                }) =>
                {
                    Some((you, seats, server_time_ms, you_were_preempted))
                }
                _ => None,
            })
            .await;
        assert_eq!(
            you,
            Seat::Writing,
            "the interrupted writer keeps its seat — only the turn moved"
        );
        // The one fact the rows cannot carry: this movement was *deliberate*.
        // Without it the browser cannot tell a confirmed override from the
        // ordinary hand-off that happens every time the other person starts a
        // sentence, and 2f's evicted panel would be a modal on every hand-off.
        assert!(
            preempted,
            "the writer that was cut into is told the interruption was confirmed"
        );
        // The frame carries its own reference clock, so the rows it delivers are
        // as datable as the ones inside a snapshot. Artboard 2f's `connected`
        // fact must not depend on which frame the seat news arrived in.
        assert!(
            server_time_ms > 0,
            "a seat list with no clock to date it against: {server_time_ms}"
        );
        for row in &seats {
            assert!(
                server_time_ms >= row.since_ms,
                "a seat cannot have been taken in the host's future: {row:?}"
            );
        }
        assert_eq!(seats.len(), 3, "desktop + two tabs: {seats:?}");
        assert_eq!(
            seats.iter().filter(|row| row.holds_input).count(),
            1,
            "exactly one surface has the turn: {seats:?}"
        );

        // The other half of the flag: the surface that *did* the interrupting
        // gets the same roster and is told nothing was done to it. A browser
        // shown 2f's evicted panel about its own confirmed `Take over` would be
        // the panel accusing the reader of what the reader just chose.
        let (holder_view, holder_preempted) = frame_matching(&mut second, |frame| match frame {
            ServerMsg::Delta(Delta::Seats {
                seats,
                you_were_preempted,
                you: _,
                server_time_ms: _,
            }) => Some((seats, you_were_preempted)),
            _ => None,
        })
        .await;
        assert!(
            holder_view
                .iter()
                .any(|row| row.holds_input && row.viewer_id.as_ref() == Some(&after.viewer_id)),
            "the interrupter's own frame shows it holding the turn: {holder_view:?}"
        );
        assert!(
            !holder_preempted,
            "the writer that pressed the button was interrupted by nobody"
        );

        // And now the roles are exactly reversed, on the same symmetric rule.
        send(
            &mut first,
            &ClientMsg::Input(Input {
                seq: 2,
                terminal_id: "t1".into(),
                data: b"lo".to_vec(),
            }),
        )
        .await;
        let ack = next_ack(&mut first).await;
        assert_eq!(
            ack.outcome,
            flightdeck::web::protocol::AckOutcome::Rejected,
            "the writer that was interrupted is refused on the same terms"
        );
    });

    // Only the holder's bytes ever reached the host's input seam. The refused
    // ones were never forwarded at all — arbitration happens before the channel,
    // which is what makes draining it in order safe.
    let typed: Vec<Vec<u8>> = harness
        .inbound()
        .iter()
        .filter_map(|event| match event {
            WebInbound::Input { input, .. } => Some(input.data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        typed,
        vec![b"hel".to_vec()],
        "a refused keystroke must not reach the PTY seam"
    );
}

#[test]
fn an_observer_never_contends_for_the_seat() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut driver = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut driver, SeatRequest::Write).await;
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
        assert_eq!(again.seat, Seat::Writing);
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
        attach(&mut ws, SeatRequest::Write).await;
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
        attach(&mut first, SeatRequest::Write).await;
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
        let (you, _, _, preempted) = frame_matching(&mut first, |frame| match frame {
            ServerMsg::Delta(Delta::Seats {
                you,
                seats,
                server_time_ms,
                you_were_preempted,
            }) if you == Seat::Observing => Some((you, seats, server_time_ms, you_were_preempted)),
            _ => None,
        })
        .await;
        assert_eq!(you, Seat::Observing);
        // Giving a turn up is not having one taken away: nothing here is worth
        // 2f's evicted panel.
        assert!(!preempted, "release_seat interrupted nobody");

        // The seat is free, so the next browser gets it without a takeover.
        let mut second = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut second, SeatRequest::Write).await;
        assert_eq!(await_snapshot(&mut second).await.seat, Seat::Writing);
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
        attach(&mut ws, SeatRequest::Write).await;
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
        attach(&mut driver, SeatRequest::Write).await;
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

        // And `Writers` reaches everyone seated as a writer, which is not the
        // observer.
        harness
            .handle
            .send(server::WebOutbound::Writers(ServerMsg::Delta(
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
        attach(&mut ws, SeatRequest::Write).await;
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
        attach(&mut ws, SeatRequest::Write).await;
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
        attach(&mut ws, SeatRequest::Write).await;
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
                SeatRequest::Write
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

// ===========================================================================
// Terminals: bytes out, keystrokes in (D2, D4, D8, Q3, §5.1)
// ===========================================================================
//
// These drive the real socket into **real
// `flightdeck::terminal::session::Terminal`s**, each backed by a
// `flightdeck::testing::FakePtySession` that records every `write_input` and
// every `resize` it is handed. That recording is the point: D4's guarantee is
// not "the host politely declines a viewer's geometry", it is "no such call is
// ever made", and only a counting seam can tell those two apart.

use flightdeck::contracts::domain::PtySize;
use flightdeck::terminal::session::Session;
use flightdeck::testing::{FakePty, FakePtyHandle};
use flightdeck::web::protocol::{Ack, ServerMsg as SM, TermBytes, TermCursor};
use flightdeck::web::stream::{
    child_terminal_id, primary_terminal_id, write_into_session, TerminalHost, TerminalStreams,
    Written,
};

/// The tabs the host has open — the `AppState` half of the TUI's
/// [`TerminalHost`], reduced to what a terminal write needs.
///
/// It resolves a wire id through the *same* `write_into_session` the TUI uses,
/// so a divergence between this test's idea of a stale id and production's
/// would be a compile-level impossibility rather than a review question.
#[derive(Default)]
struct Tabs {
    tabs: Vec<(String, Session)>,
}

impl TerminalHost for Tabs {
    fn write_terminal_input(
        &mut self,
        terminal_id: &flightdeck::web::protocol::TerminalId,
        bytes: &[u8],
    ) -> Written {
        for (tab_id, session) in self.tabs.iter_mut() {
            let tab_id = tab_id.clone();
            if let Some(written) = write_into_session(session, &tab_id, terminal_id, bytes) {
                return written;
            }
        }
        Written::NoSuchTerminal
    }
}

/// The host side of the terminal stream, assembled the way `src/lib.rs`
/// assembles it: one [`TerminalStreams`] registry plus the tabs it streams.
struct Fleet {
    streams: TerminalStreams,
    tabs: Tabs,
    backend: FakePty,
}

impl Fleet {
    fn new(replay_bytes: usize) -> Fleet {
        Fleet {
            streams: TerminalStreams::new(replay_bytes),
            tabs: Tabs::default(),
            backend: FakePty::new(),
        }
    }

    /// Open a tab with a primary agent terminal, returning the handle that
    /// records what its PTY was asked to do.
    fn tab(&mut self, tab_id: &str) -> FakePtyHandle {
        let handle = self.backend.queue_session();
        let mut session = Session::new();
        session
            .spawn_primary(
                &self.backend,
                "agent",
                &[],
                std::path::Path::new("."),
                PtySize {
                    rows: 34,
                    cols: 120,
                },
            )
            .expect("the fake backend spawns");
        self.streams.open(primary_terminal_id(tab_id));
        self.tabs.tabs.push((tab_id.to_string(), session));
        handle
    }

    /// Add a child shell to an existing tab, returning its handle and its
    /// **mint-keyed** id (never its index — see `TerminalId`'s own doc).
    fn child(&mut self, tab_id: &str) -> (FakePtyHandle, flightdeck::web::protocol::TerminalId) {
        let handle = self.backend.queue_session();
        let (_, session) = self
            .tabs
            .tabs
            .iter_mut()
            .find(|(id, _)| id == tab_id)
            .expect("that tab is open");
        let index = session
            .spawn_child(
                &self.backend,
                "bash",
                &[],
                std::path::Path::new("."),
                PtySize {
                    rows: 34,
                    cols: 120,
                },
            )
            .expect("the fake backend spawns");
        let id = child_terminal_id(
            tab_id,
            session
                .child(index)
                .expect("the child is there")
                .stream_id(),
        );
        self.streams.open(id.clone());
        (handle, id)
    }

    /// One tick of `drain_pty_output`: read every live PTY, feed the desktop's
    /// own `vt100` parser, **and** tee the same bytes to the browser (D2).
    fn pump(&mut self, handle: &WebServerHandle) {
        for (tab_id, session) in self.tabs.tabs.iter_mut() {
            if let Some(primary) = session.primary_mut() {
                let bytes = primary.session_mut().try_read_output().unwrap_or_default();
                if !bytes.is_empty() {
                    primary.process_output(&bytes);
                    if let Some(frame) = self
                        .streams
                        .pty_output(&primary_terminal_id(tab_id), &bytes)
                    {
                        handle.send(server::WebOutbound::All(SM::TermBytes(frame)));
                    }
                }
            }
            for c in 0..session.child_count() {
                let Some(child) = session.child_mut(c) else {
                    continue;
                };
                let stream_id = child.stream_id();
                let bytes = child.session_mut().try_read_output().unwrap_or_default();
                if bytes.is_empty() {
                    continue;
                }
                child.process_output(&bytes);
                if let Some(frame) = self
                    .streams
                    .pty_output(&child_terminal_id(tab_id, stream_id), &bytes)
                {
                    handle.send(server::WebOutbound::All(SM::TermBytes(frame)));
                }
            }
        }
    }

    /// One tick of the inbound drain, exactly as the TUI runs it.
    fn drain(&mut self, events: Vec<WebInbound>, handle: &WebServerHandle) {
        for event in events {
            for out in self.streams.apply_inbound(&event, &mut self.tabs) {
                handle.send(out);
            }
        }
    }
}

/// Let the server's async side catch up with what the synchronous test just did.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(150)).await;
}

async fn next_term_bytes(ws: &mut Ws) -> TermBytes {
    frame_matching(ws, |frame| match frame {
        SM::TermBytes(bytes) => Some(bytes),
        _ => None,
    })
    .await
}

async fn next_ack(ws: &mut Ws) -> Ack {
    frame_matching(ws, |frame| match frame {
        SM::Ack(ack) => Some(ack),
        _ => None,
    })
    .await
}

// ---------------------------------------------------------------------------
// D2: bytes out
// ---------------------------------------------------------------------------

/// D2 + Q3: raw PTY bytes reach an attached viewer, each frame carrying the
/// offset of its own first byte, contiguous across chunks — and the desktop's
/// own `vt100` parse still sees the same bytes.
#[test]
fn pty_bytes_reach_an_attached_viewer_with_monotonic_offsets() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut fleet = Fleet::new(65_536);
    let agent = fleet.tab("tab-1");

    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        await_snapshot(&mut ws).await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        agent.push_output(b"hello ".to_vec());
        fleet.pump(&harness.handle);
        agent.push_output(b"world".to_vec());
        fleet.pump(&harness.handle);

        let first = next_term_bytes(&mut ws).await;
        assert_eq!(first.terminal_id.as_str(), "tab-1:primary");
        assert_eq!(first.data, b"hello ".to_vec());
        assert_eq!(first.offset, 0);
        assert!(!first.truncated);

        let second = next_term_bytes(&mut ws).await;
        assert_eq!(second.data, b"world".to_vec());
        assert_eq!(
            second.offset,
            first.next_offset(),
            "offsets must be contiguous, or a resuming viewer's cursor lies"
        );
    });

    // D2's other half: the desktop's parse was not disturbed by the tee.
    let (_, session) = &fleet.tabs.tabs[0];
    let screen = session.primary().expect("a primary").screen();
    assert!(
        screen.contents().contains("hello world"),
        "the desktop's own vt100 parse must still have the bytes: {:?}",
        screen.contents()
    );
}

/// D14 as revised: a controller and a concurrent observer both receive every
/// byte, and only the controller's keystrokes reach a PTY. Both halves in one
/// test, because it is the *combination* that D14 permits.
#[test]
fn a_controller_and_an_observer_both_see_bytes_but_only_one_types() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut fleet = Fleet::new(65_536);
    let agent = fleet.tab("tab-1");

    on_runtime(async {
        let mut driver = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut driver, SeatRequest::Write).await;
        await_snapshot(&mut driver).await;
        let mut watcher = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut watcher, SeatRequest::Observe).await;
        await_snapshot(&mut watcher).await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        agent.push_output(b"shared output".to_vec());
        fleet.pump(&harness.handle);

        for ws in [&mut driver, &mut watcher] {
            let frame = next_term_bytes(ws).await;
            assert_eq!(
                frame.data,
                b"shared output".to_vec(),
                "an observer is a first-class reader (D14)"
            );
        }

        // The observer types. The server refuses it before it ever reaches the
        // host, so the PTY sees nothing — and the observer is *told*, never
        // silently dropped (§5.1).
        send(
            &mut watcher,
            &ClientMsg::Input(Input {
                seq: 1,
                terminal_id: primary_terminal_id("tab-1"),
                data: b"rm -rf /\r".to_vec(),
            }),
        )
        .await;
        let ack = next_ack(&mut watcher).await;
        assert_eq!(ack.outcome, AckOutcome::Ignored);
        assert!(ack.detail.is_some(), "the observer is told why");
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);
        assert!(
            agent.input().is_empty(),
            "an observer's keystrokes must never reach a PTY, got {:?}",
            String::from_utf8_lossy(&agent.input())
        );

        // The controller types, and it lands.
        send(
            &mut driver,
            &ClientMsg::Input(Input {
                seq: 1,
                terminal_id: primary_terminal_id("tab-1"),
                data: b"ls\r".to_vec(),
            }),
        )
        .await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);
        let ack = next_ack(&mut driver).await;
        assert_eq!(ack.outcome, AckOutcome::Applied);
        assert_eq!(agent.input(), b"ls\r".to_vec());
    });
}

// ---------------------------------------------------------------------------
// Q3: reconnect and resume
// ---------------------------------------------------------------------------

/// Q3's Tail case, over a real socket: the viewer sends the cursor it saved,
/// and is handed the exact continuation — nothing re-sent, nothing skipped.
#[test]
fn a_reconnecting_viewer_resumes_from_its_byte_cursor() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut fleet = Fleet::new(65_536);
    let agent = fleet.tab("tab-1");
    let terminal = primary_terminal_id("tab-1");

    on_runtime(async {
        // First connection: sees the first chunk, then the link dies.
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        await_snapshot(&mut ws).await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        agent.push_output(b"first-half ".to_vec());
        fleet.pump(&harness.handle);
        let seen = next_term_bytes(&mut ws).await;
        let cursor = seen.next_offset();
        drop(ws);

        // Output continues while nobody is watching.
        agent.push_output(b"second-half".to_vec());
        fleet.pump(&harness.handle);

        // Reconnect, presenting the cursor.
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        send(
            &mut ws,
            &ClientMsg::Attach(Attach {
                protocol_version: PROTOCOL_VERSION,
                seat: SeatRequest::TakeOver,
                cursors: vec![TermCursor {
                    terminal_id: terminal.clone(),
                    next_offset: cursor,
                }],
                resume_viewer: None,
                viewport: None,
                client: None,
            }),
        )
        .await;
        await_snapshot(&mut ws).await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        let resumed = next_term_bytes(&mut ws).await;
        assert_eq!(resumed.offset, cursor, "a tail begins where the viewer was");
        assert_eq!(
            resumed.data,
            b"second-half".to_vec(),
            "exactly the bytes it missed, and no others"
        );
        assert!(
            !resumed.truncated,
            "nothing aged out, so nothing may be claimed lost"
        );
    });
}

/// Q3's Truncated case: the ring wrapped while the viewer was away, so it is
/// handed everything retained, flagged, starting *ahead* of where it asked.
/// That inequality is the gap, and the flag is what lets the browser say it
/// missed output instead of pretending continuity.
#[test]
fn a_reconnecting_viewer_whose_cursor_aged_out_is_told_it_missed_output() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    // A ring far smaller than what the terminal is about to print.
    let mut fleet = Fleet::new(8);
    let agent = fleet.tab("tab-1");
    let terminal = primary_terminal_id("tab-1");

    on_runtime(async {
        agent.push_output(b"0123456789ABCDEF".to_vec());
        fleet.pump(&harness.handle);

        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        send(
            &mut ws,
            &ClientMsg::Attach(Attach {
                protocol_version: PROTOCOL_VERSION,
                seat: SeatRequest::Write,
                cursors: vec![TermCursor {
                    terminal_id: terminal.clone(),
                    next_offset: 2,
                }],
                resume_viewer: None,
                viewport: None,
                client: None,
            }),
        )
        .await;
        await_snapshot(&mut ws).await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        let resumed = next_term_bytes(&mut ws).await;
        assert!(
            resumed.truncated,
            "the viewer must be told output was discarded before it could see it"
        );
        assert_eq!(resumed.data, b"89ABCDEF".to_vec());
        assert_eq!(resumed.offset, 8, "the oldest byte still retained");
        assert!(
            resumed.offset > 2,
            "the frame starting ahead of the cursor *is* the gap"
        );
    });
}

// ---------------------------------------------------------------------------
// §5.1: queued, in order, exactly once
// ---------------------------------------------------------------------------

/// Input reaches the terminal it names, and only that one. The child is
/// addressed by its mint, so this also proves the id is not positional.
#[test]
fn input_reaches_the_terminal_it_names_and_no_other() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut fleet = Fleet::new(65_536);
    let agent = fleet.tab("tab-1");
    let (shell, shell_id) = fleet.child("tab-1");
    let other_agent = fleet.tab("tab-2");

    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        await_snapshot(&mut ws).await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        send(
            &mut ws,
            &ClientMsg::Input(Input {
                seq: 1,
                terminal_id: shell_id.clone(),
                data: b"pwd\r".to_vec(),
            }),
        )
        .await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        let ack = next_ack(&mut ws).await;
        assert_eq!(ack.seq, 1);
        assert_eq!(ack.outcome, AckOutcome::Applied);
        assert_eq!(shell.input(), b"pwd\r".to_vec());
        assert!(
            agent.input().is_empty() && other_agent.input().is_empty(),
            "a keystroke must not spray across terminals"
        );
    });
}

/// Type one writer's whole token at human speed, one `Input` frame per byte,
/// after an optional stagger.
///
/// **One frame per byte, spaced out in time, on purpose.** A token sent as a
/// single frame would be written atomically by `write_into_session` whatever the
/// lock did, and a burst fired in microseconds would finish before the other
/// writer's first byte arrived — either way the test would pass without the lock
/// and prove nothing. A `KEY_GAP_MS` gap makes the two bursts genuinely overlap
/// in time, so an unarbitrated host really does splice them, while staying well
/// inside `INPUT_LOCK_IDLE_MS` so an arbitrated one never breaks a burst.
async fn type_token(
    ws: &mut Ws,
    terminal: &flightdeck::web::protocol::TerminalId,
    seq_base: u64,
    token: &[u8],
    stagger_ms: u64,
    key_gap_ms: u64,
) {
    if stagger_ms > 0 {
        tokio::time::sleep(Duration::from_millis(stagger_ms)).await;
    }
    for (offset, byte) in token.iter().enumerate() {
        if offset > 0 {
            tokio::time::sleep(Duration::from_millis(key_gap_ms)).await;
        }
        send(
            ws,
            &ClientMsg::Input(Input {
                seq: seq_base + offset as u64,
                terminal_id: terminal.clone(),
                data: vec![*byte],
            }),
        )
        .await;
    }
}

/// **The criterion D14's revision exists for: two writers typing at the same
/// time never produce interleaved-and-corrupted bytes at the PTY.**
///
/// Two real sockets, both seated as writers, both typing a five-byte token one
/// frame per byte at human speed, *concurrently* — the two bursts genuinely
/// overlap: one writer's keys fall in the gaps between the other's, which is
/// precisely the arrangement that produces `1212121212…` on a host that does not
/// arbitrate. The 20 ms stagger is half a keystroke gap, so it decides the
/// round's winner without ever separating the bursts in time; which side gets it
/// alternates, so both writers really reach the terminal and the result is not
/// an artifact of one of them never having tried.
///
/// The assertion is on the bytes the fake PTY actually received. Every five-byte
/// token must be whole and belong to one writer. **Half the tokens never arrive
/// at all**, and that is the cost the decision log states rather than a defect:
/// a keystroke typed into somebody else's live burst is refused, not queued for
/// later delivery, because delivering it later would splice it into the middle
/// of whatever they had typed.
#[test]
fn two_writers_typing_at_once_never_interleave_at_the_pty() {
    const ROUNDS: u64 = 4;
    const TOKEN_A: &[u8] = b"1111.";
    const TOKEN_B: &[u8] = b"2222.";
    /// A relaxed typing speed. Five of these span 160 ms — comfortably inside
    /// `INPUT_LOCK_IDLE_MS`, so an arbitrated host never breaks a burst.
    const KEY_GAP_MS: u64 = 40;
    /// Half a keystroke gap: enough to decide who asked first (the host answers
    /// a frame in microseconds over loopback), far too little to stop the bursts
    /// overlapping.
    const STAGGER_MS: u64 = 20;
    /// Long enough after the winner's last byte that the lock has idled out
    /// before the next round, so each round starts from a free lock.
    const SETTLE_MS: u64 = 450;

    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut fleet = Fleet::new(65_536);
    let agent = fleet.tab("tab-1");
    let terminal = primary_terminal_id("tab-1");

    on_runtime(async {
        let mut alice = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        let mut bob = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut alice, SeatRequest::Write).await;
        let alice_snapshot = await_snapshot(&mut alice).await;
        attach(&mut bob, SeatRequest::Write).await;
        let bob_snapshot = await_snapshot(&mut bob).await;
        assert_eq!(alice_snapshot.seat, Seat::Writing);
        assert_eq!(
            bob_snapshot.seat,
            Seat::Writing,
            "both are writers — that is what lifting the single-controller \
             restriction means"
        );
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        for round in 0..ROUNDS {
            let seq = 1 + round * TOKEN_A.len() as u64;
            // Alternate who asks first, so the transcript has to contain both
            // writers' tokens for the test to pass.
            let (alice_stagger, bob_stagger) = if round % 2 == 0 {
                (0, STAGGER_MS)
            } else {
                (STAGGER_MS, 0)
            };
            // Both bursts are genuinely in flight together: two tasks on two
            // sockets, joined rather than sequenced, and interleaved in time.
            tokio::join!(
                type_token(
                    &mut alice,
                    &terminal,
                    seq,
                    TOKEN_A,
                    alice_stagger,
                    KEY_GAP_MS
                ),
                type_token(&mut bob, &terminal, seq, TOKEN_B, bob_stagger, KEY_GAP_MS),
            );
            tokio::time::sleep(Duration::from_millis(SETTLE_MS)).await;
            settle().await;
            fleet.drain(harness.inbound(), &harness.handle);
        }
    });

    let typed = agent.input();
    let transcript = String::from_utf8_lossy(&typed).into_owned();
    let tokens: Vec<&[u8]> = typed.chunks(TOKEN_A.len()).collect();

    // The criterion. An unarbitrated host writes `12121212..21212121..`, and
    // every chunk of it is a token neither writer typed.
    for (chunk, token) in tokens.iter().enumerate() {
        assert!(
            *token == TOKEN_A || *token == TOKEN_B,
            "the PTY was written `{}` at chunk {chunk}, which is neither \
             writer's token — the two bursts were spliced together, which is \
             exactly the corruption the input lock exists to prevent. Whole \
             transcript: `{transcript}`",
            String::from_utf8_lossy(token)
        );
    }
    assert!(
        tokens.contains(&TOKEN_A) && tokens.contains(&TOKEN_B),
        "both writers must actually reach the PTY, or this proves only that one \
         of them never typed: `{transcript}`"
    );
    assert_eq!(
        typed.len(),
        (ROUNDS as usize) * TOKEN_A.len(),
        "exactly one writer's token landed per round — the other was refused, \
         which is the cost D14's revision states rather than a defect: \
         `{transcript}`"
    );
}

/// §5.1 end to end: keystrokes typed while the link is down are queued by the
/// browser, replayed **in order** when it returns, and the ones the host
/// already applied are not applied a second time.
///
/// The browser's queue is modelled here the way turn 2 specifies it: replay in
/// seq order, dropping nothing, and let the host's watermark decide what was
/// already taken.
#[test]
fn input_held_across_a_reconnect_arrives_in_order_exactly_once() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut fleet = Fleet::new(65_536);
    let agent = fleet.tab("tab-1");
    let terminal = primary_terminal_id("tab-1");

    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        let first = await_snapshot(&mut ws).await;
        assert_eq!(
            first.last_input_seq, 0,
            "a fresh viewer has applied nothing"
        );
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        // Two keystrokes land, and the browser is told about both.
        for (seq, data) in [(1u64, &b"a"[..]), (2, &b"b"[..])] {
            send(
                &mut ws,
                &ClientMsg::Input(Input {
                    seq,
                    terminal_id: terminal.clone(),
                    data: data.to_vec(),
                }),
            )
            .await;
        }
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);
        for expected in [1u64, 2] {
            let ack = next_ack(&mut ws).await;
            assert_eq!(ack.seq, expected);
            assert_eq!(ack.outcome, AckOutcome::Applied);
        }
        let previous_viewer = first.viewer_id.clone();
        drop(ws);
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        // The link is down. The user keeps typing; the browser holds 3, 4, 5.
        let held: Vec<(u64, Vec<u8>)> =
            vec![(3, b"c".to_vec()), (4, b"d".to_vec()), (5, b"e".to_vec())];

        // Back. The host carries the previous connection's watermark onto the
        // new viewer id, which is what makes `last_input_seq` honest.
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        send(
            &mut ws,
            &ClientMsg::Attach(Attach {
                protocol_version: PROTOCOL_VERSION,
                seat: SeatRequest::TakeOver,
                cursors: Vec::new(),
                resume_viewer: Some(previous_viewer.clone()),
                viewport: None,
                client: None,
            }),
        )
        .await;
        let resumed = await_snapshot(&mut ws).await;
        settle().await;
        // Nothing bespoke here: the ordinary drain carries the watermark onto
        // the new viewer id, because `Attach { resume_viewer }` told it to.
        fleet.drain(harness.inbound(), &harness.handle);
        assert_eq!(
            resumed.last_input_seq, 2,
            "the returning browser is told exactly what already landed"
        );

        // The browser replays its whole queue in seq order — including the two
        // it has now learned were applied, to prove they are not typed twice.
        for (seq, data) in [(1u64, b"a".to_vec()), (2, b"b".to_vec())]
            .into_iter()
            .chain(held)
        {
            send(
                &mut ws,
                &ClientMsg::Input(Input {
                    seq,
                    terminal_id: terminal.clone(),
                    data,
                }),
            )
            .await;
        }
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        let mut outcomes = Vec::new();
        for _ in 0..5 {
            let ack = next_ack(&mut ws).await;
            outcomes.push((ack.seq, ack.outcome));
        }
        assert_eq!(
            outcomes,
            vec![
                (1, AckOutcome::Ignored),
                (2, AckOutcome::Ignored),
                (3, AckOutcome::Applied),
                (4, AckOutcome::Applied),
                (5, AckOutcome::Applied),
            ],
            "every frame is answered: the replayed ones ignored, the held ones applied"
        );
        assert_eq!(
            agent.input(),
            b"abcde".to_vec(),
            "in order, once each: no loss, no duplication, no reordering"
        );
    });
}

/// A refusal path (SPECS §26): a keystroke aimed at a terminal the host no
/// longer has is **rejected with a reason** rather than vanishing, and the
/// watermark does not move, so the same seq stays re-sendable.
#[test]
fn input_for_a_terminal_the_host_does_not_have_is_refused_out_loud() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut fleet = Fleet::new(65_536);
    let agent = fleet.tab("tab-1");

    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        await_snapshot(&mut ws).await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        send(
            &mut ws,
            &ClientMsg::Input(Input {
                seq: 1,
                terminal_id: primary_terminal_id("tab-that-was-closed"),
                data: b"still here?\r".to_vec(),
            }),
        )
        .await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        let ack = next_ack(&mut ws).await;
        assert_eq!(ack.seq, 1);
        assert_eq!(ack.outcome, AckOutcome::Rejected);
        assert!(
            ack.detail.is_some(),
            "a rejection with no reason is a silent drop with extra steps"
        );
        assert!(agent.input().is_empty());

        // The refusal did not consume the seq: a retry at the same number, once
        // the terminal is back, still applies.
        send(
            &mut ws,
            &ClientMsg::Input(Input {
                seq: 1,
                terminal_id: primary_terminal_id("tab-1"),
                data: b"retry\r".to_vec(),
            }),
        )
        .await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);
        let ack = next_ack(&mut ws).await;
        assert_eq!(ack.outcome, AckOutcome::Applied);
        assert_eq!(agent.input(), b"retry\r".to_vec());
    });
}

/// The other refusal path: the terminal is still listed, but its process has
/// exited. The browser can see it, so it gets the accurate reason rather than
/// "no such terminal".
#[test]
fn input_for_an_exited_terminal_is_refused_with_the_accurate_reason() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut fleet = Fleet::new(65_536);
    let agent = fleet.tab("tab-1");
    agent.set_state(ProcessState::Exited(0));

    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        await_snapshot(&mut ws).await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        send(
            &mut ws,
            &ClientMsg::Input(Input {
                seq: 1,
                terminal_id: primary_terminal_id("tab-1"),
                data: b"anyone there?\r".to_vec(),
            }),
        )
        .await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        let ack = next_ack(&mut ws).await;
        assert_eq!(ack.outcome, AckOutcome::Rejected);
        let detail = ack.detail.unwrap_or_default();
        assert!(
            detail.contains("exited"),
            "the browser is told the process is gone, not that the terminal is: {detail}"
        );
        assert!(agent.input().is_empty());
    });
}

// ---------------------------------------------------------------------------
// D4: the browser can never resize a PTY
// ---------------------------------------------------------------------------

/// **D4, proved rather than asserted about types.**
///
/// A viewer sends `Resize { viewport: 240×80 }` — a viewport nothing like the
/// host's 120×34 grid — over a real socket. The frame reaches the host
/// (asserted, so this is not passing because the frame went missing), is fed
/// through the very same inbound drain the TUI runs, and lands on real
/// `Terminal`s whose `PtySession`s **count every `resize` call they receive**.
/// The count must still be zero.
///
/// The counter is then proved non-vacuous on the *same* fake session: the
/// desktop's own resize path (`Terminal::resize`, which is what
/// `resize_if_changed` calls every frame) is invoked and the count becomes one.
/// Without that second half, "zero resizes" could mean "the fake never counts",
/// which would prove nothing at all.
#[test]
fn a_resize_frame_never_resizes_a_pty() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut fleet = Fleet::new(65_536);
    let agent = fleet.tab("tab-1");
    let (shell, _shell_id) = fleet.child("tab-1");

    // Spawning sizes a PTY once, through the backend rather than through
    // `resize`, so both counters start empty. Baseline it explicitly: the whole
    // test is about a count, and a count needs a known zero.
    assert!(agent.resizes().is_empty(), "baseline: no resizes yet");
    assert!(shell.resizes().is_empty(), "baseline: no resizes yet");

    let saw_resize = on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        await_snapshot(&mut ws).await;
        settle().await;
        fleet.drain(harness.inbound(), &harness.handle);

        // A viewport wildly unlike the host's 120x34 grid, sent twice, plus a
        // keystroke behind it so the host is demonstrably still processing this
        // viewer's frames after the resizes.
        for (cols, rows) in [(240u16, 80u16), (37, 11)] {
            send(
                &mut ws,
                &ClientMsg::Resize(flightdeck::web::protocol::Resize {
                    viewport: flightdeck::web::protocol::Viewport { cols, rows },
                }),
            )
            .await;
        }
        send(
            &mut ws,
            &ClientMsg::Input(Input {
                seq: 1,
                terminal_id: primary_terminal_id("tab-1"),
                data: b"x".to_vec(),
            }),
        )
        .await;
        settle().await;

        let events = harness.inbound();
        let resizes = events
            .iter()
            .filter(|e| matches!(e, WebInbound::Resize { .. }))
            .count();
        fleet.drain(events, &harness.handle);
        let ack = next_ack(&mut ws).await;
        assert_eq!(
            ack.outcome,
            AckOutcome::Applied,
            "the host kept processing this viewer's frames after the resizes"
        );
        resizes
    });

    assert_eq!(
        saw_resize, 2,
        "the Resize frames must really have reached the host, or this test proves nothing"
    );
    assert_eq!(
        agent.resizes(),
        Vec::new(),
        "D4: a viewer's viewport must never reach portable_pty"
    );
    assert_eq!(
        shell.resizes(),
        Vec::new(),
        "D4: not for the selected terminal, and not for any other either"
    );

    // The PTY grid is still the host's, untouched.
    let (_, session) = &fleet.tabs.tabs[0];
    let (rows, cols) = session.primary().expect("a primary").screen().size();
    assert_eq!(
        (cols, rows),
        (120, 34),
        "the host's grid is unchanged by anything the browser said"
    );

    // ...and the counter is not vacuous: the *desktop's* resize path, the one
    // `sync_terminal_sizes` drives every frame, does register.
    let (_, session) = &mut fleet.tabs.tabs[0];
    session
        .primary_mut()
        .expect("a primary")
        .resize(PtySize {
            rows: 40,
            cols: 100,
        })
        .expect("the desktop may resize its own PTY");
    assert_eq!(
        agent.resizes(),
        vec![PtySize {
            rows: 40,
            cols: 100
        }],
        "the seam does count resizes — so the zero above is a real zero"
    );
}

// ===========================================================================
// The activity feed (D11, turn 2 §5.1)
// ===========================================================================
//
// D11 is the browser's *entire* substitute for OS notifications — Web Push is
// structurally blocked under D1 — so these drive the two halves that make it
// worth having: a tab already open must learn a transition without reloading,
// and a tab opened afterwards must land on history rather than silence.
//
// They deliberately go through the same `web::activity` store and the same
// `web::stream::deltas` the event loop uses. A test that hand-rolled a
// `Delta::Activity` would pass happily while the loop shipped nothing.

/// The event loop's publish step in miniature (see `build_web_host_state` and
/// the `web_surface.running()` block in `src/lib.rs`): enforce both retention
/// bounds, carry the retained feed into the state, publish, then send the deltas
/// that describe the difference.
fn publish_activity(
    handle: &WebServerHandle,
    published: &mut HostState,
    store: &mut ActivityStore,
    clock: &dyn Clock,
) {
    store.evict(clock);
    let next = HostState {
        activity: store.events().cloned().collect(),
        ..published.clone()
    };
    let frames = flightdeck::web::stream::deltas(published, &next);
    handle.publish_state(next.clone());
    for delta in frames {
        handle.send(server::WebOutbound::All(ServerMsg::Delta(delta)));
    }
    *published = next;
}

/// Record one transition the way `WebSurface::record_transition` does — through
/// `activity::observe`, so the reason string in these assertions is the one the
/// host would really produce rather than one the test made up.
fn record(
    store: &mut ActivityStore,
    clock: &dyn Clock,
    session: &str,
    was: (ProcessState, InterpretedStatus),
    now: (ProcessState, InterpretedStatus),
    lifecycle_reporting: bool,
) {
    let observed = flightdeck::web::activity::observe(
        DisplayStatus {
            process: was.0,
            interpreted: was.1,
            manual: None,
        },
        DisplayStatus {
            process: now.0,
            interpreted: now.1,
            manual: None,
        },
        "Claude Code",
        lifecycle_reporting,
    );
    store.record(
        clock,
        Transition {
            project_id: ProjectId::new("/repo/flightdeck"),
            project_name: "flightdeck".to_string(),
            session_id: TabId(session.to_string()),
            session_name: session.to_string(),
            from: observed.from,
            to: observed.to,
            manual: observed.manual,
            reason: observed.reason,
        },
    );
}

/// Block until the host is handed an inbound command, the way the TUI's tick
/// loop finds one in its channel. Returns the seat label too: D13's origin line
/// is built from it, so a test that asserts on an origin needs the same value
/// the real host would have used.
fn wait_for_command(
    harness: &Harness,
) -> (flightdeck::web::protocol::ViewerId, String, WireCommand) {
    let deadline = std::time::Instant::now() + WAIT;
    while std::time::Instant::now() < deadline {
        for event in harness.inbound() {
            if let WebInbound::Command {
                viewer_id,
                label,
                command,
            } = event
            {
                return (viewer_id, label, command);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the host was never told about the command");
}

/// The live half: a tab that is already looking at the app learns about a
/// transition as a `Delta::Activity`, with no reload and no re-snapshot.
#[test]
fn an_attached_viewer_learns_a_new_transition_without_reloading() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let clock = RealClock;
    let mut store = ActivityStore::new();
    let mut published = HostState::default();

    let mut ws = on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        let snapshot = await_snapshot(&mut ws).await;
        assert!(
            snapshot.activity.is_empty(),
            "nothing has happened yet, and the host does not invent history"
        );
        ws
    });

    record(
        &mut store,
        &clock,
        "flaky-e2e-runner",
        (ProcessState::Running, InterpretedStatus::Working),
        (ProcessState::Exited(1), InterpretedStatus::Failed),
        true,
    );
    publish_activity(&harness.handle, &mut published, &mut store, &clock);

    let event = on_runtime(frame_matching(&mut ws, |frame| match frame {
        ServerMsg::Delta(Delta::Activity(event)) => Some(event),
        _ => None,
    }));
    assert_eq!(event.session_name, "flaky-e2e-runner");
    assert_eq!(event.project_name, "flightdeck");
    assert_eq!(event.from, InterpretedStatus::Working);
    assert_eq!(event.to, InterpretedStatus::Failed);
    assert_eq!(event.reason, "agent exited (code 1)");
    assert_eq!(event.tier, ActivityTier::Attention);
    assert!(!event.read);
}

/// A publish that changes nothing about the feed must not replay it: the
/// backfill list is resent whole on every tick, and a browser that appended it
/// again would show every row twice.
#[test]
fn republishing_the_same_feed_sends_no_further_activity_deltas() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let clock = RealClock;
    let mut store = ActivityStore::new();
    let mut published = HostState::default();

    record(
        &mut store,
        &clock,
        "add-tests-api",
        (ProcessState::Running, InterpretedStatus::Working),
        (ProcessState::Running, InterpretedStatus::Idle),
        true,
    );
    publish_activity(&harness.handle, &mut published, &mut store, &clock);

    let mut ws = on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        await_snapshot(&mut ws).await;
        ws
    });

    // Two more ticks with nothing new, then one genuinely new event. The first
    // `Delta::Activity` to arrive must be the new one.
    publish_activity(&harness.handle, &mut published, &mut store, &clock);
    publish_activity(&harness.handle, &mut published, &mut store, &clock);
    record(
        &mut store,
        &clock,
        "migrate-schema-v4",
        (ProcessState::Running, InterpretedStatus::Working),
        (ProcessState::Running, InterpretedStatus::WaitingForInput),
        true,
    );
    publish_activity(&harness.handle, &mut published, &mut store, &clock);

    let event = on_runtime(frame_matching(&mut ws, |frame| match frame {
        ServerMsg::Delta(Delta::Activity(event)) => Some(event),
        _ => None,
    }));
    assert_eq!(
        event.session_name, "migrate-schema-v4",
        "the already-delivered row must not have been replayed"
    );
    assert_eq!(
        event.reason, "",
        "`asked a question` is not a fact any hook reports, so the row carries \
         no reason rather than a plausible one"
    );
}

/// The reason D11's retention exists: a tab opened *after* the fact must land on
/// history. This is the bug `remote-control-5yy.1` was filed for — the store was
/// correct and nothing fed it, so a fresh tab opened on silence.
#[test]
fn a_freshly_attached_viewer_backfills_the_retained_feed() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let clock = RealClock;
    let mut store = ActivityStore::new();
    let mut published = HostState::default();

    // Two transitions with nobody watching, including one from an agent that
    // reports no lifecycle at all (§5.1: `unknown → unknown`, never a guess).
    record(
        &mut store,
        &clock,
        "add-tests-api",
        (ProcessState::Running, InterpretedStatus::Working),
        (ProcessState::Running, InterpretedStatus::Idle),
        true,
    );
    record(
        &mut store,
        &clock,
        "hotfix-csp-header",
        (ProcessState::Running, InterpretedStatus::Unknown),
        (ProcessState::Exited(0), InterpretedStatus::Completed),
        false,
    );
    publish_activity(&harness.handle, &mut published, &mut store, &clock);

    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        let snapshot = await_snapshot(&mut ws).await;
        assert_eq!(
            snapshot.activity.len(),
            2,
            "a fresh tab opens on history, not silence"
        );
        // Oldest first, as `Snapshot::activity` documents.
        assert_eq!(snapshot.activity[0].session_name, "add-tests-api");
        assert_eq!(snapshot.activity[1].session_name, "hotfix-csp-header");
        assert!(snapshot.activity.iter().all(|event| !event.read));

        let unknown = &snapshot.activity[1];
        assert_eq!(unknown.from, InterpretedStatus::Unknown);
        assert_eq!(
            unknown.to,
            InterpretedStatus::Unknown,
            "the process exited 0, but this agent never said what that meant"
        );
        assert_eq!(unknown.reason, "Claude Code reports no lifecycle");
        assert_eq!(unknown.tier, ActivityTier::Quiet);
    });
}

/// Read-marking end to end, and why it is host state: one tab opening the feed
/// must not leave a second tab facing the same wall of unread.
#[test]
fn marking_the_feed_read_is_host_state_a_second_tab_sees() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let clock = RealClock;
    let mut store = ActivityStore::new();
    let mut published = HostState::default();

    record(
        &mut store,
        &clock,
        "add-tests-api",
        (ProcessState::Running, InterpretedStatus::Working),
        (ProcessState::Running, InterpretedStatus::Idle),
        true,
    );
    record(
        &mut store,
        &clock,
        "flaky-e2e-runner",
        (ProcessState::Running, InterpretedStatus::Working),
        (ProcessState::Exited(1), InterpretedStatus::Failed),
        true,
    );
    publish_activity(&harness.handle, &mut published, &mut store, &clock);

    let (mut ws, ids) = on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        let snapshot = await_snapshot(&mut ws).await;
        let ids: Vec<String> = snapshot
            .activity
            .iter()
            .map(|event| event.event_id.as_str().to_string())
            .collect();
        (ws, ids)
    });
    assert_eq!(ids.len(), 2);

    // What the SPA sends (`webui/src/main.ts`): the ids it has just shown.
    on_runtime(send(
        &mut ws,
        &ClientMsg::Command(WireCommand {
            seq: 11,
            name: flightdeck::web::protocol::command::MARK_ACTIVITY_READ.to_string(),
            args: Some(serde_json::json!({ "event_ids": ids })),
        }),
    ));

    // The tick loop's half: apply it to the store and ack the sender.
    let (viewer_id, _label, command) = wait_for_command(&harness);
    let ack = apply_mark_read(&mut store, &command);
    assert_eq!(ack.outcome, AckOutcome::Applied);
    harness.handle.send(server::WebOutbound::Viewer {
        viewer_id,
        msg: ServerMsg::Ack(ack),
    });
    let delivered = on_runtime(frame_matching(&mut ws, |frame| match frame {
        ServerMsg::Ack(ack) => Some(ack),
        _ => None,
    }));
    assert_eq!(delivered.seq, 11);
    assert_eq!(delivered.outcome, AckOutcome::Applied);

    // Next tick republishes, and a second tab attaches to a feed that agrees.
    publish_activity(&harness.handle, &mut published, &mut store, &clock);
    on_runtime(async {
        let mut second = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut second, SeatRequest::Observe).await;
        let snapshot = await_snapshot(&mut second).await;
        assert_eq!(snapshot.activity.len(), 2);
        assert!(
            snapshot.activity.iter().all(|event| event.read),
            "the second tab must not be shown a wall of unread the first already cleared"
        );
    });
}

/// Failure path: a malformed command is refused with a stated reason rather than
/// silently succeeding, and nothing is half-applied.
#[test]
fn a_malformed_mark_activity_read_is_rejected_and_changes_nothing() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let clock = RealClock;
    let mut store = ActivityStore::new();
    let mut published = HostState::default();

    record(
        &mut store,
        &clock,
        "add-tests-api",
        (ProcessState::Running, InterpretedStatus::Working),
        (ProcessState::Running, InterpretedStatus::Idle),
        true,
    );
    publish_activity(&harness.handle, &mut published, &mut store, &clock);

    let mut ws = on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        await_snapshot(&mut ws).await;
        ws
    });
    on_runtime(send(
        &mut ws,
        &ClientMsg::Command(WireCommand {
            seq: 12,
            name: flightdeck::web::protocol::command::MARK_ACTIVITY_READ.to_string(),
            args: Some(serde_json::json!({ "event_ids": "evt-1" })),
        }),
    ));

    let (viewer_id, _label, command) = wait_for_command(&harness);
    let ack = apply_mark_read(&mut store, &command);
    assert_eq!(ack.outcome, AckOutcome::Rejected);
    assert!(ack.detail.is_some(), "a rejection has to state why");
    harness.handle.send(server::WebOutbound::Viewer {
        viewer_id,
        msg: ServerMsg::Ack(ack),
    });

    let delivered = on_runtime(frame_matching(&mut ws, |frame| match frame {
        ServerMsg::Ack(ack) => Some(ack),
        _ => None,
    }));
    assert_eq!(delivered.seq, 12);
    assert_eq!(delivered.outcome, AckOutcome::Rejected);
    assert!(
        store.events().all(|event| !event.read),
        "a rejected frame must not half-apply"
    );
}

/// Failure path: D14's read-only observation is real. The feed's read flag is
/// shared host state, so an observer flipping it would change what the
/// controller's next snapshot says — the server refuses the frame before the
/// host ever hears about it.
#[test]
fn an_observer_cannot_mark_the_feed_read() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let clock = RealClock;
    let mut store = ActivityStore::new();
    let mut published = HostState::default();

    record(
        &mut store,
        &clock,
        "add-tests-api",
        (ProcessState::Running, InterpretedStatus::Working),
        (ProcessState::Running, InterpretedStatus::Idle),
        true,
    );
    publish_activity(&harness.handle, &mut published, &mut store, &clock);

    on_runtime(async {
        // A controller first, so the second socket really is an observer.
        let mut driver = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut driver, SeatRequest::Write).await;
        await_snapshot(&mut driver).await;

        let mut watcher = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut watcher, SeatRequest::Observe).await;
        await_snapshot(&mut watcher).await;

        send(
            &mut watcher,
            &ClientMsg::Command(WireCommand {
                seq: 13,
                name: flightdeck::web::protocol::command::MARK_ACTIVITY_READ.to_string(),
                args: Some(serde_json::json!({ "event_ids": ["evt-1"] })),
            }),
        )
        .await;

        let error = frame_matching(&mut watcher, |frame| match frame {
            ServerMsg::Error(error) => Some(error),
            _ => None,
        })
        .await;
        assert_eq!(error.code, ErrorCode::ReadOnly);
        assert_eq!(error.seq, Some(13));
    });

    assert!(
        !harness
            .inbound()
            .iter()
            .any(|event| matches!(event, WebInbound::Command { .. })),
        "the refusal happens at the socket; the host is never asked to apply it"
    );
    assert!(store.events().all(|event| !event.read));
}

// ===========================================================================
// The command surface (D3, D13, D16; SPECS §5, §22)
// ===========================================================================
//
// These drive **real `Command` frames over the real socket** and then run the
// host's half exactly as the tick loop does: look the name up in
// `flightdeck::web::commands::INVENTORY`, and hand the action it carries to the
// same dispatcher the TUI's palette calls. Nothing here writes a `Command`
// value of its own — a test that hand-rolled the effect would pass while the
// wire name pointed somewhere else entirely, which is the failure the inventory
// exists to prevent.

use flightdeck::app::commands::{Command as AppCommand, Effect};
use flightdeck::app::state::{AppState, Services};
use flightdeck::contracts::domain::{Config, ProjectState, STATE_VERSION};
use flightdeck::testing::{FakeClock, FakeCommandRunner, FakeContainerRuntime, FakeFs, FakeGit};
use flightdeck::web::commands::{self, Confirmation, Route, HOST_ONLY_REFUSAL, PULL_BASE_REFUSAL};
use flightdeck::web::protocol::{command as names, CommandTarget};

/// Send one command frame from a controlling browser, and hand back the socket
/// so the answer can be read off it.
async fn control(addr: &str, cookie: &str) -> Ws {
    let mut ws = ws_connect(addr, Some(cookie)).await.expect("upgrade");
    attach(&mut ws, SeatRequest::Write).await;
    await_snapshot(&mut ws).await;
    ws
}

async fn command(ws: &mut Ws, seq: u64, name: &str) {
    send(
        ws,
        &ClientMsg::Command(WireCommand {
            seq,
            name: name.to_string(),
            args: None,
        }),
    )
    .await;
}

async fn next_error(ws: &mut Ws) -> flightdeck::web::protocol::WireError {
    frame_matching(ws, |frame| match frame {
        ServerMsg::Error(error) => Some(error),
        _ => None,
    })
    .await
}

/// Nothing reached the host: the refusal happened at the socket, so no code path
/// existed that could have applied it.
fn assert_nothing_forwarded(harness: &Harness) {
    // Give the server a moment to have forwarded it, if it were going to.
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !harness
            .inbound()
            .iter()
            .any(|event| matches!(event, WebInbound::Command { .. })),
        "the frame must be refused at the socket, never forwarded to the host"
    );
}

/// The browser must not have to guess what this build can run: the inventory
/// rides on the snapshot, and every row it names is a row the host accepts.
#[test]
fn the_snapshot_carries_the_hosts_command_inventory() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    let snapshot = on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        await_snapshot(&mut ws).await
    });

    assert!(
        snapshot.commands.len() > 30,
        "the whole §22 surface, not a handful: {}",
        snapshot.commands.len()
    );
    for view in &snapshot.commands {
        assert!(
            commands::lookup(&view.run.name).is_some(),
            "the host offered `{}`, which it does not accept",
            view.run.name
        );
        assert!(!view.label.is_empty() && !view.group.is_empty());
    }

    // D16: both desktop-only actions are present, badged, and say why they will
    // be refused — visible and honest rather than hidden.
    for name in [names::OPEN_WORKTREE_IN_FILE_MANAGER, names::EDIT_IN_EDITOR] {
        let view = snapshot
            .commands
            .iter()
            .find(|view| view.run.name == name)
            .unwrap_or_else(|| panic!("`{name}` must be offered, not hidden"));
        assert!(view.host_only, "`{name}` must carry the host-only badge");
        assert_eq!(view.refusal.as_deref(), Some(HOST_ONLY_REFUSAL));
    }

    // The three D3 selection rows are templates: the browser fills the id.
    let select_session = snapshot
        .commands
        .iter()
        .find(|view| view.run.name == names::SELECT_SESSION)
        .expect("the selection rows are on the wire");
    assert_eq!(select_session.target, Some(CommandTarget::Session));
    assert!(select_session.run.args.is_none());

    // The names the SPA had to invent before this existed are now real, and
    // nothing in the inventory is a name the host cannot resolve.
    assert!(snapshot
        .commands
        .iter()
        .any(|view| view.run.name == names::RESTART_AGENT));
    assert!(snapshot
        .commands
        .iter()
        .any(|view| view.run.name == names::TOGGLE_SPLIT_VIEW));
}

/// SPECS §23's help and the About screen ride on the snapshot, in the host's
/// own words (`remote-control-ll5.8`, `specs/WEB_INTERFACE.md` §6.5 R16).
///
/// This is the same argument the inventory test above makes, applied to the
/// other thing both surfaces must agree about: a browser that authored its own
/// keybinding list would be documenting a FlightDeck it is not attached to, and
/// the drift would be invisible until somebody changed a binding. Asserting the
/// content *matches `crate::tui::help`* is the whole point — it is what makes
/// the desktop's overlay and the browser's one source rather than two.
#[test]
fn the_snapshot_carries_the_hosts_help_and_about() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    let snapshot = on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Write).await;
        await_snapshot(&mut ws).await
    });

    let help = snapshot.help.expect("this build sends its help screen");
    assert_eq!(help, flightdeck::tui::help::help_doc(false, false));
    assert!(
        help.sections.iter().any(|s| s.title == "Global"),
        "the browser must get the whole list, not a summary"
    );
    // Every row is both halves. A key with no description is nothing a reader
    // can act on, and the browser renders these verbatim.
    for section in &help.sections {
        assert!(!section.rows.is_empty());
        for row in &section.rows {
            assert!(!row.keys.is_empty() && !row.description.is_empty());
        }
    }
    // An ordinary run has no notes; SPECS §32's is the only one, and it is a
    // fact about *this* run rather than a constant either surface holds.
    assert!(help.notes.is_empty());

    // One source, again: the panel the browser draws is the panel the desktop
    // draws. (`snapshot.host_version` is deliberately *not* compared here —
    // this harness fabricates a `HostState` with a stand-in version string,
    // whereas a real one is built by `build_web_host_state` from the same
    // `CARGO_PKG_VERSION` this is.)
    let about = snapshot.about.expect("this build sends its About screen");
    assert_eq!(about, flightdeck::tui::help::about_doc());
    assert_eq!(about.version, env!("CARGO_PKG_VERSION"));
    assert!(!about.credits.is_empty());
    assert!(about.url.starts_with("https://"));
}

/// A real frame drives a real effect: `toggle_split_view` travels over the
/// socket, is looked up in the inventory, and the action it carries — the very
/// value the TUI's palette row holds — flips real `AppState`.
#[test]
fn a_real_command_frame_drives_a_real_effect() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut ws = on_runtime(control(&addr, &cookie));

    on_runtime(command(&mut ws, 31, names::TOGGLE_SPLIT_VIEW));
    let (viewer_id, _label, forwarded) = wait_for_command(&harness);
    assert_eq!(forwarded.name, names::TOGGLE_SPLIT_VIEW);

    // The host's half, as the tick loop runs it: the inventory hands over the
    // palette action, and the app core applies it. The test never names the
    // command itself.
    let spec = commands::lookup(&forwarded.name).expect("a forwarded name is a known name");
    let action = match &spec.route {
        Route::Palette(action) => action.clone(),
        other => panic!("expected a palette route, got {other:?}"),
    };
    let cmd = match &action {
        flightdeck::tui::palette::PaletteAction::Dispatch(cmd) => cmd.clone(),
        other => panic!("expected a direct dispatch, got {other:?}"),
    };
    assert_eq!(
        cmd,
        AppCommand::ToggleSplitView,
        "the wire name must reach the palette's own action"
    );

    let mut app = app_state();
    let git = FakeGit::new();
    let fs = FakeFs::new();
    let pty = FakePty::new();
    let clock = FakeClock::new("2026-08-28T10:00:00Z");
    let container = FakeContainerRuntime::new();
    let runner = FakeCommandRunner::new();
    let services = Services {
        git: &git,
        fs: &fs,
        pty: &pty,
        clock: &clock,
        container: &container,
        command: &runner,
    };
    assert!(!app.split_view);
    let effect = app.dispatch(cmd, &services).expect("the dispatch succeeds");
    assert!(app.split_view, "the frame changed real host state");
    let detail = match effect {
        Effect::Message(m) => Some(m),
        other => panic!("expected a message, got {other:?}"),
    };

    // And the browser is told what happened, by seq, rather than guessing.
    harness.handle.send(server::WebOutbound::Viewer {
        viewer_id,
        msg: ServerMsg::Ack(Ack {
            seq: forwarded.seq,
            outcome: AckOutcome::Applied,
            detail,
        }),
    });
    let ack = on_runtime(next_ack(&mut ws));
    assert_eq!(ack.seq, 31);
    assert_eq!(ack.outcome, AckOutcome::Applied);
    assert!(
        ack.detail.is_some(),
        "the ack carries what the desktop showed"
    );
}

/// D16: a desktop-only action is answered with its host-only outcome. Not a
/// fake success (the window would open on a machine the user is not at) and not
/// silence (indistinguishable from a success).
#[test]
fn a_desktop_only_command_is_acked_with_its_host_only_outcome() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut ws = on_runtime(control(&addr, &cookie));

    for (seq, name) in [
        (41, names::OPEN_WORKTREE_IN_FILE_MANAGER),
        (42, names::EDIT_IN_EDITOR),
    ] {
        on_runtime(command(&mut ws, seq, name));
        let ack = on_runtime(next_ack(&mut ws));
        assert_eq!(ack.seq, seq);
        assert_eq!(
            ack.outcome,
            AckOutcome::Rejected,
            "`{name}` must not report a success that happened on another machine"
        );
        assert_eq!(ack.detail.as_deref(), Some(HOST_ONLY_REFUSAL));
    }
    assert_nothing_forwarded(&harness);
}

/// **D16: `quit` stops FlightDeck and every agent in it, and a bare frame naming
/// it cannot do that.**
///
/// The mechanism changed in `remote-control-ll5.4` and the property did not.
/// Until then the socket refused the name outright; now the frame is forwarded,
/// and what stops it is the *payload* — `INVENTORY` carries `Quit { confirm:
/// false }` and a forwarding row never reads the frame's `args`, so the value
/// reaching `AppState::dispatch` can only ask. The dispatch below is the real
/// one, so this test cannot pass by describing a table that lies.
#[test]
fn a_bare_quit_frame_cannot_kill_flightdeck() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut ws = on_runtime(control(&addr, &cookie));

    // Even a frame that spells out `confirm: true` — the smuggling attempt.
    on_runtime(async {
        send(
            &mut ws,
            &ClientMsg::Command(WireCommand {
                seq: 51,
                name: names::QUIT.to_string(),
                args: Some(serde_json::json!({ "confirm": true })),
            }),
        )
        .await;
    });
    let (_, _, forwarded) = wait_for_command(&harness);
    assert_eq!(forwarded.name, names::QUIT);

    let spec = commands::lookup(&forwarded.name).expect("a forwarded name is a known name");
    let cmd = match &spec.route {
        Route::Palette(flightdeck::tui::palette::PaletteAction::Dispatch(cmd)) => cmd.clone(),
        other => panic!("expected a direct dispatch, got {other:?}"),
    };
    assert_eq!(
        cmd,
        AppCommand::Quit { confirm: false },
        "the args the frame carried are not read: the table's value is what runs"
    );
    assert_eq!(commands::confirmation_of(&cmd), Confirmation::Pending);

    let mut app = app_state();
    let git = FakeGit::new();
    let fs = FakeFs::new();
    let pty = FakePty::new();
    let clock = FakeClock::new("2026-08-28T10:00:00Z");
    let container = FakeContainerRuntime::new();
    let runner = FakeCommandRunner::new();
    let services = Services {
        git: &git,
        fs: &fs,
        pty: &pty,
        clock: &clock,
        container: &container,
        command: &runner,
    };
    let effect = app.dispatch(cmd, &services).expect("the dispatch succeeds");
    assert_eq!(
        effect,
        Effect::QuitConfirm,
        "the frame opened D13's shared question; it did not quit"
    );

    // And the socket keeps working, as it did when this was a flat refusal.
    on_runtime(command(&mut ws, 52, names::REQUEST_SNAPSHOT));
    on_runtime(await_snapshot(&mut ws));
}

/// **A confirmation from a browser that has gone read-only does not slip
/// through** (D14 + artboard 1g, `remote-control-ll5.4`).
///
/// The seat that typed the name must be the seat that confirms, and here that is
/// true *by construction rather than by comparison*: 1g's typed name rides on
/// the deciding frame itself, so the host keeps no "this viewer is armed" state
/// for a second browser to inherit or race. What is left is the ordinary seat
/// check — which runs before a command's own route is even considered.
///
/// **D14's revision changes what puts a browser on the wrong side of that
/// check.** A takeover no longer demotes anyone: it takes the input *lock*, and
/// the interrupted browser keeps its writer's seat and may still answer a
/// dialog. What still refuses is the seat itself, and 2f's `Watch read-only` is
/// how a browser chooses it — which is exactly the flow here, mid-dialog.
#[test]
fn a_confirm_from_a_browser_watching_read_only_never_reaches_the_host() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut first = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut first, SeatRequest::Write).await;
        await_snapshot(&mut first).await;

        // A second browser takes the turn while the first is typing the session
        // name into artboard 1g's step 2 — which, under the revision, leaves the
        // first browser still seated and still able to answer.
        let mut second = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut second, SeatRequest::TakeOver).await;
        await_snapshot(&mut second).await;

        // So the first browser takes 2f's other offer instead of fighting.
        attach(&mut first, SeatRequest::Observe).await;
        await_snapshot(&mut first).await;

        // It finishes typing and answers anyway. The name is right; the seat is
        // not one that may decide anything.
        send(
            &mut first,
            &ClientMsg::Command(WireCommand {
                seq: 71,
                name: names::DIALOG_CONFIRM.to_string(),
                args: Some(serde_json::json!({
                    "dialog_id": "dialog-1",
                    "confirm_name": "Task",
                })),
            }),
        )
        .await;
        let error = next_error(&mut first).await;
        assert_eq!(error.code, ErrorCode::ReadOnly);
        assert_eq!(error.seq, Some(71));

        // Cancelling is refused for the same reason, and that is not a
        // contradiction of "cancelling is never gated": the gate is about the
        // *name*, the seat is about D14. An observer answering a question that
        // is not theirs is input by another name, in either direction.
        send(
            &mut first,
            &ClientMsg::Command(WireCommand {
                seq: 72,
                name: names::DIALOG_CANCEL.to_string(),
                args: Some(serde_json::json!({ "dialog_id": "dialog-1" })),
            }),
        )
        .await;
        let error = next_error(&mut first).await;
        assert_eq!(error.code, ErrorCode::ReadOnly);
    });

    assert_nothing_forwarded(&harness);
}

/// D14 as revised: `take_input_lock` is the browser's explicit override, and it
/// is the *same* act as `Attach { seat: take_over }` — the palette door for a
/// tab that is already a writer and does not want to re-attach to interrupt.
///
/// The desktop reaches the same act through its own palette row, which is what
/// keeps the rule symmetric: neither surface can cut into a live burst any other
/// way, and both can.
#[test]
fn a_writer_can_take_the_input_lock_by_name() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut first = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut first, SeatRequest::Write).await;
        let first_snapshot = await_snapshot(&mut first).await;
        let mut second = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut second, SeatRequest::Write).await;
        let second_snapshot = await_snapshot(&mut second).await;

        // The first claims the lock by typing.
        send(
            &mut first,
            &ClientMsg::Input(Input {
                seq: 1,
                terminal_id: "t1".into(),
                data: b"a".to_vec(),
            }),
        )
        .await;
        settle().await;

        command(&mut second, 90, names::TAKE_INPUT_LOCK).await;
        let ack = next_ack(&mut second).await;
        assert_eq!(ack.seq, 90);
        assert_eq!(ack.outcome, AckOutcome::Applied);

        // The turn moved, and the seat map says so to everyone.
        let seats = frame_matching(&mut first, |frame| match frame {
            ServerMsg::Delta(Delta::Seats { seats, .. })
                if seats.iter().any(|row| {
                    row.holds_input && row.viewer_id.as_ref() == Some(&second_snapshot.viewer_id)
                }) =>
            {
                Some(seats)
            }
            _ => None,
        })
        .await;
        assert_eq!(seats.iter().filter(|row| row.holds_input).count(), 1);

        // And the first is now the one being refused, mid-burst, on the same rule.
        send(
            &mut first,
            &ClientMsg::Input(Input {
                seq: 2,
                terminal_id: "t1".into(),
                data: b"b".to_vec(),
            }),
        )
        .await;
        let refusal = frame_matching(&mut first, |frame| match frame {
            ServerMsg::Error(error) => Some(error),
            _ => None,
        })
        .await;
        assert_eq!(refusal.code, ErrorCode::SeatHeld);
        assert_eq!(
            refusal.incumbent.and_then(|row| row.viewer_id).as_ref(),
            Some(&second_snapshot.viewer_id)
        );
        assert_ne!(first_snapshot.viewer_id, second_snapshot.viewer_id);
    });
}

/// An observer taking the input lock would stop everyone else typing in order to
/// type nothing, so it is the one server-answered row that is not open to a
/// read-only tab.
#[test]
fn an_observer_cannot_take_the_input_lock() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut watcher = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut watcher, SeatRequest::Observe).await;
        await_snapshot(&mut watcher).await;

        command(&mut watcher, 91, names::TAKE_INPUT_LOCK).await;
        let error = next_error(&mut watcher).await;
        assert_eq!(error.code, ErrorCode::ReadOnly);
        assert_eq!(error.seq, Some(91));

        // The two rows that *are* open to an observer still are: this is one
        // row's exception, not a new rule about `Route::Server`.
        command(&mut watcher, 92, names::REQUEST_SNAPSHOT).await;
        await_snapshot(&mut watcher).await;
    });
}

/// D14: read-only means read-only, for the whole surface and not just for the
/// four M1 names — the seat check runs before the command's own route is even
/// considered, so nothing added later can slip past it.
#[test]
fn an_observers_palette_command_is_refused_as_read_only() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());

    on_runtime(async {
        let mut ws = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut ws, SeatRequest::Observe).await;
        await_snapshot(&mut ws).await;

        // One that would run, one that would be refused anyway, and one that is
        // desktop-only: an observer gets `read_only` for all three, and learns
        // nothing about which would have worked.
        for (seq, name) in [
            (61, names::RESTART_AGENT),
            (62, names::QUIT),
            (63, names::OPEN_WORKTREE_IN_FILE_MANAGER),
            // D13/D14: a dialog is shared with every viewer, but answering it is
            // input by another name. An observer sees the modal and cannot
            // decide it — for either half, so it cannot cancel out from under
            // the controller either.
            (64, names::DIALOG_CONFIRM),
            (65, names::DIALOG_CANCEL),
        ] {
            command(&mut ws, seq, name).await;
            let error = next_error(&mut ws).await;
            assert_eq!(error.code, ErrorCode::ReadOnly, "for `{name}`");
            assert_eq!(error.seq, Some(seq));
        }
    });
    assert_nothing_forwarded(&harness);
}

/// **D13 over a real socket.** A browser row whose desktop behaviour is "ask
/// something" reaches the host rather than being refused; the host publishes the
/// dialog and the browser learns about it as a `Delta::DialogOpened` carrying the
/// origin *it* is named by; answering it closes it with a `Delta::DialogClosed`
/// whose outcome is the decision, not the diff's fallback.
///
/// The host half here is the real one: `crate::web::stream::deltas` computes the
/// frames from two published `HostState`s, exactly as the TUI's tick does, so
/// this test cannot pass by sending frames the host would never send.
#[test]
fn a_dialog_opened_from_a_browser_round_trips_over_the_socket() {
    use flightdeck::web::protocol::{
        DialogOrigin, DialogOutcome, DialogView, ViewerId as WireViewerId,
    };

    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut ws = on_runtime(control(&addr, &cookie));

    // 1. The row is forwarded, not refused: before D13 landed this answered
    //    `ErrorCode::NotSupported`.
    on_runtime(command(&mut ws, 91, names::NEW_AGENT_SESSION_TAB));
    let (viewer_id, label, forwarded) = wait_for_command(&harness);
    assert_eq!(forwarded.name, names::NEW_AGENT_SESSION_TAB);
    assert!(
        !label.is_empty(),
        "the host needs a label to word D13's origin line"
    );

    // 2. The host does what the TUI's tick does: publish the dialog, then send
    //    the deltas the diff derived from the change.
    let mut published = HostState::default();
    let opened = DialogView {
        dialog_id: flightdeck::web::protocol::DialogId::new("dialog-1"),
        kind: "new_agent".to_string(),
        title: "New Agent Session Tab".to_string(),
        origin: DialogOrigin::Browser {
            viewer_id: Some(WireViewerId::new(viewer_id.as_str())),
            label: label.clone(),
        },
        body: None,
    };
    let next = HostState {
        dialog: Some(opened.clone()),
        ..published.clone()
    };
    publish_dialog(&harness.handle, &mut published, next);

    let arrived = on_runtime(frame_matching(&mut ws, |frame| match frame {
        ServerMsg::Delta(Delta::DialogOpened(view)) => Some(view),
        _ => None,
    }));
    assert_eq!(arrived, opened);
    match arrived.origin {
        DialogOrigin::Browser { label: seen, .. } => assert_eq!(seen, label),
        DialogOrigin::Desktop => panic!("this dialog was opened from a browser"),
    }

    // 3. A late joiner paints the dialog from the snapshot, not from a delta it
    //    was not attached for — the dialog is state, which is D13's premise.
    let second = on_runtime(async {
        let mut second = ws_connect(&addr, Some(&cookie)).await.expect("upgrade");
        attach(&mut second, SeatRequest::Observe).await;
        await_snapshot(&mut second).await
    });
    assert_eq!(second.dialog.as_ref(), Some(&opened));

    // 4. The browser confirms. The frame reaches the host, which closes the
    //    dialog and reports the *decision* — `Superseded` here would mean
    //    "somebody replaced it", which is not what happened.
    on_runtime(async {
        send(
            &mut ws,
            &ClientMsg::Command(WireCommand {
                seq: 92,
                name: names::DIALOG_CONFIRM.to_string(),
                args: Some(serde_json::json!({ "dialog_id": "dialog-1" })),
            }),
        )
        .await;
    });
    let (_, _, answered) = wait_for_command(&harness);
    assert_eq!(answered.name, names::DIALOG_CONFIRM);

    let closed = HostState {
        dialog: None,
        ..published.clone()
    };
    let mut frames = flightdeck::web::stream::deltas(&published, &closed);
    for frame in frames.iter_mut() {
        if let Delta::DialogClosed { outcome, .. } = frame {
            *outcome = DialogOutcome::Confirmed;
        }
    }
    harness.handle.publish_state(closed);
    for frame in frames {
        harness
            .handle
            .send(flightdeck::web::server::WebOutbound::All(ServerMsg::Delta(
                frame,
            )));
    }

    let (dialog_id, outcome) = on_runtime(frame_matching(&mut ws, |frame| match frame {
        ServerMsg::Delta(Delta::DialogClosed { dialog_id, outcome }) => Some((dialog_id, outcome)),
        _ => None,
    }));
    assert_eq!(dialog_id.as_str(), "dialog-1");
    assert_eq!(outcome, DialogOutcome::Confirmed);
}

/// Publish one state and send the deltas the diff derived, the way the TUI's
/// tick does. `published` is advanced so a caller can chain ticks.
fn publish_dialog(
    handle: &flightdeck::web::server::WebServerHandle,
    published: &mut HostState,
    next: HostState,
) {
    let frames = flightdeck::web::stream::deltas(published, &next);
    handle.publish_state(next.clone());
    for frame in frames {
        handle.send(flightdeck::web::server::WebOutbound::All(ServerMsg::Delta(
            frame,
        )));
    }
    *published = next;
}

/// The rows that still refuse at the socket refuse because of **where the panel
/// would land**, not because nothing was built.
///
/// `show_git_status` left this test in `remote-control-ll5.8`: it is a palette
/// row now, forwarding `ShowGitStatus`, and the host answers the asking browser
/// with `ServerMsg::GitStatus` rather than opening a panel on the desktop
/// (`specs/WEB_INTERFACE.md` §6.5 R16). What took its place here is
/// `show_help`: the browser draws its own help from `Snapshot::help`, so
/// forwarding the row would put a panel on the desktop that the person who
/// asked cannot read. Refused at the socket, so no code path exists that could
/// half-open it.
///
/// `abandon_worktree` left this test in `remote-control-ll5.4`: it is a palette
/// row now, forwarding `AbandonWorktree { confirm: false }`, which can only open
/// SPECS §5/§15's question. What stops a browser *answering* that question with
/// one press is artboard 1g's typed name, which lives on the confirm rather than
/// on the row — so it is checked where the effect is, not at the door.
#[test]
fn the_dialog_row_that_still_refuses_says_which_task_owns_it() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut ws = on_runtime(control(&addr, &cookie));

    on_runtime(command(&mut ws, 72, names::SHOW_HELP));
    let error = on_runtime(next_error(&mut ws));
    assert_eq!(error.code, ErrorCode::NotSupported);
    assert_eq!(error.seq, Some(72));
    assert!(
        error.message.len() > 30,
        "it must say why: {}",
        error.message
    );
    assert_nothing_forwarded(&harness);

    // And the read-only overlay that *is* forwarded, because its facts are a
    // fresh `git` read the snapshot cannot hold (SPECS §21).
    on_runtime(command(&mut ws, 75, names::SHOW_GIT_STATUS));
    let (_, _, forwarded) = wait_for_command(&harness);
    assert_eq!(forwarded.name, names::SHOW_GIT_STATUS);
    let spec = commands::lookup(&forwarded.name).expect("a forwarded name is a known name");
    assert_eq!(
        commands::dispatched_command(&spec.route),
        Some(&AppCommand::ShowGitStatus)
    );

    // The destructive row is forwarded now — and carries the value that asks.
    on_runtime(command(&mut ws, 73, names::ABANDON_WORKTREE));
    let (_, _, forwarded) = wait_for_command(&harness);
    assert_eq!(forwarded.name, names::ABANDON_WORKTREE);
    let spec = commands::lookup(&forwarded.name).expect("a forwarded name is a known name");
    assert_eq!(
        commands::dispatched_command(&spec.route),
        Some(&AppCommand::AbandonWorktree { confirm: false }),
        "the first dispatch asks; 1g's second step guards the answer"
    );

    // Still alive, like any other refusal.
    on_runtime(command(&mut ws, 74, names::REQUEST_SNAPSHOT));
    on_runtime(await_snapshot(&mut ws));
}

/// **SPECS §5 over a real socket, restated for a browser that runs git**
/// (`remote-control-ll5.5`).
///
/// Three git rows now reach the host, so the old assertion — "every git name is
/// refused at the socket" — would be a rubber stamp if it were merely relaxed.
/// What is proven here instead is the invariant that replaced it:
///
/// 1. The rows that run are **forwarded** rather than refused, and what travels
///    is the frame the browser sent — the host then takes its payload from
///    `INVENTORY`, never from the frame's `args`.
/// 2. `pull_base` is still refused at the socket (SPECS §5.2), with the
///    boundary decision as its reason rather than a placeholder.
/// 3. Every command the table would forward either does not rewrite history, or
///    is unconfirmed and therefore lands on §5.1's confirmation prompt — and
///    none of them opens a pull request, with no exception.
#[test]
fn no_browser_reachable_command_rewrites_unconfirmed_history_or_opens_a_pr() {
    let harness = Harness::start();
    let addr = harness.addr();
    let cookie = on_runtime(harness.authenticate());
    let mut ws = on_runtime(control(&addr, &cookie));

    // 1. The git family reaches the host. Sent one at a time and drained one at
    //    a time, so a row that was silently dropped shows up as a timeout here
    //    rather than as a passing test.
    for (seq, name) in [
        (81, names::REBASE_WORKTREE),
        (82, names::PUSH_BRANCH),
        (83, names::FINISH_LOCAL_MERGE),
    ] {
        on_runtime(command(&mut ws, seq, name));
        let (_, _, forwarded) = wait_for_command(&harness);
        assert_eq!(forwarded.name, name);
        assert_eq!(forwarded.seq, seq);
        assert!(
            forwarded.args.is_none(),
            "`{name}` carries no payload on the wire; the host takes its own"
        );
    }

    // 2. SPECS §5.2's row is the exception, and it says so in its own words.
    on_runtime(command(&mut ws, 84, names::PULL_BASE));
    let error = on_runtime(next_error(&mut ws));
    assert_eq!(error.code, ErrorCode::NotSupported);
    assert_eq!(error.seq, Some(84));
    assert_eq!(error.message, PULL_BASE_REFUSAL);

    // 3. The construction behind all of it, checked against the live table.
    let mut rewriting: Vec<&str> = Vec::new();
    for spec in commands::INVENTORY {
        if let Some(cmd) = commands::dispatched_command(&spec.route) {
            if commands::rewrites_history(cmd) {
                rewriting.push(spec.name);
                assert_eq!(
                    commands::confirmation_of(cmd),
                    Confirmation::Pending,
                    "`{}` would rewrite history from a browser without asking",
                    spec.name
                );
            }
            assert_ne!(
                commands::confirmation_of(cmd),
                Confirmation::Given,
                "`{}` carries a pre-confirmed {cmd:?}",
                spec.name
            );
            assert!(
                !commands::creates_pull_request(cmd),
                "`{}` would open a PR from a browser",
                spec.name
            );
        }
    }
    assert_eq!(
        rewriting,
        vec![names::REBASE_WORKTREE],
        "SPECS §5.1 sanctions exactly one, and it is confirmation-gated"
    );

    // The socket is unharmed by the refusal, like any other.
    on_runtime(command(&mut ws, 85, names::REQUEST_SNAPSHOT));
    on_runtime(await_snapshot(&mut ws));
}

/// An `AppState` with no tabs — enough for the global view commands, which is
/// what these tests drive.
fn app_state() -> AppState {
    AppState::new(
        Config::default(),
        ProjectState {
            version: STATE_VERSION,
            project_root_relative: ".".to_string(),
            base_branch: "main".to_string(),
            tabs: Vec::new(),
        },
        std::path::Path::new("."),
        std::path::Path::new("state.json"),
    )
}

// ===========================================================================
// The production path (§6.5 R18)
// ===========================================================================
//
// Every other credential test in this file mints by calling
// `mint_bootstrap_code` itself, and the Playwright suite authenticates through
// `FLIGHTDECK_WEB_TEST_CODE` — a `#[cfg(debug_assertions)]` seam. Both are
// legitimate, and between them they left the one thing nobody was testing: that
// something in a *shipped* binary ever mints a code and shows it. For months
// nothing did, and the whole suite stayed green while the web interface could
// not be authenticated at all.
//
// These two tests close that. They drive `web::access::WebAccess` — the type
// the TUI's event loop drives, the only production caller of
// `mint_bootstrap_code` — and exchange what it produces over the real HTTP
// endpoint. Nothing debug-only is compiled into the path they take, and the
// first thing each asserts is that the debug seam's environment variable is not
// set, so a developer running the suite with it exported cannot make them pass
// for the wrong reason.

/// The debug seam must be nowhere near these two tests. Asserted rather than
/// assumed: `FLIGHTDECK_WEB_TEST_CODE` is exported in some local Playwright
/// workflows, and a test that silently benefits from it would be back to
/// proving nothing.
fn assert_no_debug_seam() {
    assert!(
        std::env::var_os("FLIGHTDECK_WEB_TEST_CODE").is_none(),
        "these tests prove the production mint path; unset FLIGHTDECK_WEB_TEST_CODE to run them"
    );
}

/// The loopback default (D5, artboard 2a State A): `Enter` on the access
/// overlay produces a URL, and the code in its fragment authenticates a real
/// browser through the real endpoint.
///
/// This is the acceptance criterion of the bug, end to end and in one test: a
/// release-shaped run mints a code, builds the URL it hands the browser, and
/// that URL gets in.
#[test]
fn the_access_overlay_mints_a_code_a_browser_can_exchange() {
    assert_no_debug_seam();
    let harness = Harness::start();
    let addr = harness.addr();

    // Exactly what `open_web_access_overlay` does in the event loop: build the
    // overlay against the running listener's own address, which mints.
    let mut access = {
        let mut store = harness.credentials.lock().expect("lock");
        WebAccess::open(
            &mut store,
            &FakeInterfaceEnumerator::new(),
            harness.handle.bound_addr(),
            harness.handle.exposure(),
        )
    };

    // And exactly what pressing `Enter` does.
    let outcome = {
        let mut store = harness.credentials.lock().expect("lock");
        access.handle_key(AccessKey::Enter, &mut store)
    };
    let AccessOutcome::OpenBrowser(url) = outcome else {
        panic!("Enter on the loopback state must hand a URL to the browser, got {outcome:?}");
    };

    // The credential is in the fragment and nowhere else (Q4): everything the
    // server will ever see of this URL is the part before the `#`.
    let (visible, code) = url.split_once('#').expect("the URL carries a fragment");
    assert_eq!(
        visible,
        format!("http://{}/", harness.handle.bound_addr()),
        "the request line the server sees carries no credential"
    );
    assert_eq!(code.len(), 4);
    assert!(code.bytes().all(|b| b.is_ascii_digit()));

    on_runtime(async {
        // What the SPA does with `location.hash`: POST it in a body.
        let response = post_json(
            &addr,
            "/auth/exchange",
            &[],
            &serde_json::json!({ "code": code, "label": "Firefox on Linux" }).to_string(),
        )
        .await;
        assert_eq!(
            response.status, 200,
            "the overlay's own code must be exchangeable: {}",
            response.body
        );
        let cookie = response
            .cookie(COOKIE_NAME)
            .expect("the exchange sets the access cookie");

        // And the cookie actually works, so "authenticated" is not just a 200.
        let probe = get(&addr, "/auth/session", &[("Cookie", cookie.as_str())]).await;
        assert_eq!(probe.status, 200, "{}", probe.body);
        assert_eq!(probe.json()["authenticated"], serde_json::json!(true));
    });

    assert_eq!(
        harness
            .credentials
            .lock()
            .expect("lock")
            .active_tokens()
            .count(),
        1,
        "one browser now holds access, minted by the production path"
    );
}

/// The network state (artboard 2a State B, Q1 addition 1): the string the QR
/// encodes is a URL whose fragment is the same live code, so a phone that scans
/// it lands authenticated rather than on a code prompt.
#[test]
fn the_qr_payload_carries_a_code_the_server_accepts() {
    assert_no_debug_seam();
    let harness = Harness::start();
    let addr = harness.addr();
    let port = harness.handle.bound_addr().port();

    // A routable binding, so the overlay opens in its network state and the
    // picker has an address to publish. The interface enumerator is faked (a CI
    // box's real NICs are nobody's business), but everything downstream of it —
    // the URL, the code, the exchange — is the shipped path.
    let mut access = {
        let mut store = harness.credentials.lock().expect("lock");
        WebAccess::open(
            &mut store,
            &FakeInterfaceEnumerator::new()
                .with_interface("en0", std::net::Ipv4Addr::new(192, 168, 2, 14)),
            std::net::SocketAddr::from(([0, 0, 0, 0], port)),
            BindExposure::Routable,
        )
    };

    // The payload the QR is built from, read out of the view the renderer gets
    // — so this is the string a phone camera would actually decode.
    let payload = {
        let store = harness.credentials.lock().expect("lock");
        let view = access.view(&store, |payload| Some((vec![payload.to_string()], 0)));
        assert!(view.code.is_some(), "State B shows the code beside the QR");
        view.qr_rows.first().cloned().expect("a QR payload")
    };
    assert!(
        payload.starts_with(&format!("http://192.168.2.14:{port}/#")),
        "the QR encodes the published address and the code: {payload}"
    );
    let code = payload.rsplit('#').next().expect("a fragment").to_string();

    on_runtime(async {
        let response = post_json(
            &addr,
            "/auth/exchange",
            &[],
            &serde_json::json!({ "code": code, "label": "Safari/iOS" }).to_string(),
        )
        .await;
        assert_eq!(response.status, 200, "{}", response.body);
    });

    // D5's "rotates on one command", over the wire this time: `x` revokes the
    // browser that just got in and issues a code the previous one is not.
    let spent = code.clone();
    {
        let mut store = harness.credentials.lock().expect("lock");
        assert_eq!(
            access.handle_key(AccessKey::Char('x'), &mut store),
            AccessOutcome::Handled
        );
        assert_eq!(
            store.active_tokens().count(),
            0,
            "rotate revoked the browser"
        );
    }
    on_runtime(async {
        let response = post_json(
            &addr,
            "/auth/exchange",
            &[],
            &serde_json::json!({ "code": spent }).to_string(),
        )
        .await;
        assert_ne!(
            response.status, 200,
            "the rotated-away code must not still work: {}",
            response.body
        );
    });
}
