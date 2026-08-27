//! The embedded HTTP + WebSocket server (D6, `specs/WEB_INTERFACE.md`).
//!
//! One axum app, running as **one task on the process's one shared runtime**
//! ([`crate::remote::runtime`]) alongside the relay client. The TUI event loop
//! stays synchronous and never awaits: it starts the server, publishes state
//! into it, pushes frames at it, and drains inbound frames from it — all over
//! channels.
//!
//! ```text
//!   TUI thread (sync)                         shared tokio runtime
//!   ─────────────────                         ────────────────────
//!   start(config, …)  ──── rendezvous ──────▶  bind TcpListener        ┐
//!        │            ◀─── bound SocketAddr ─  axum::serve             │ 1 task
//!        │                                         │                   │
//!   publish_state(HostState) ─ watch ───────────▶  │ GET /   assets    │
//!   send(WebOutbound)        ─ per-viewer mpsc ─▶  │ POST /auth/…      │
//!   inbound.try_recv()       ◀ std::mpsc ────────  │ GET /ws  ──┐      │
//!   stop(ShutdownNotice)     ─ watch ───────────▶  │            ├ 1 task per viewer
//!                                                  ┘            ┘
//! ```
//!
//! ## What lives here, and what deliberately does not
//!
//! **Here:** the listener and its lifecycle, the three routes, credential
//! checking on every entry point, and **seat arbitration** (D14) — one
//! controlling viewer plus N observers, takeover, eviction, and the
//! [`Delta::Seats`] fan-out that follows any change.
//!
//! **Not here:** PTY bytes and keystrokes. `src/web/stream.rs` owns those
//! (D2/D8), and the seam it plugs into is exactly two enums:
//! [`WebOutbound`] (host → viewers: [`ServerMsg::TermBytes`], [`Delta`],
//! [`Ack`]) and [`WebInbound`] (viewers → host: [`WebInbound::Input`], and the
//! [`WebInbound::ViewerAttached`] that carries the returning viewer's
//! [`TermCursor`]s so a replay can be answered). This module never inspects a
//! terminal byte; it decides *who* may send one.
//!
//! ## Security posture
//!
//! * **Two credentials (Q4).** A ~120 s bootstrap code, read by the browser from
//!   its **URL fragment** (so it never appears in a request line, a query
//!   string, a referrer or a log), is POSTed once to
//!   [`AUTH_EXCHANGE_PATH`] and exchanged for a long-lived `HttpOnly` cookie.
//!   Every later request — including the `/ws` upgrade — carries only the
//!   cookie.
//! * **The upgrade is gated before it happens.** `/ws` verifies the cookie and
//!   answers `401` *without upgrading* when it fails, so an unauthenticated peer
//!   never gets a WebSocket at all.
//! * **The peer address is the TCP peer address.** No proxy header is trusted;
//!   see [`rate_limit_address`].
//! * **Nothing secret is logged.** The code, the token and the `Cookie` /
//!   `Set-Cookie` headers never reach [`debuglog`]; refusals are logged as
//!   [`AuthFailure::as_str`] plus the peer IP.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, watch, Notify};

use crate::contracts::domain::WebConfig;
use crate::contracts::traits::Clock;
use crate::remote::debuglog;
use crate::remote::runtime;
use crate::web::assets::{self, Lookup};
use crate::web::credentials::{AccessScreen, AuthFailure, CredentialStore, TokenId};
use crate::web::protocol::{
    check_version, Ack, AckOutcome, ActivityEvent, Attach, ClientMsg, Delta, DialogView, ErrorCode,
    Geometry, ProjectView, Seat, SeatInfo, SeatRequest, Selection, ServerMsg, ShutdownReason,
    Snapshot, TermCursor, ViewerId, Viewport, WireError,
};

// ===========================================================================
// Wire-visible constants
// ===========================================================================

/// The cookie the browser presents on every request after the one-time exchange
/// (Q4).
pub const COOKIE_NAME: &str = "flightdeck_web";

/// How long the cookie is offered for: **400 days**, the longest lifetime
/// browsers will actually honour (Chrome clamps anything larger; Safari and
/// Firefox accept it).
///
/// "Long-lived" is Q4's own word, and the reason it is safe is that expiry is
/// not the revocation mechanism: the cookie is worthless the moment the desktop
/// revokes or rotates its token, because [`CredentialStore::verify_token`] is
/// consulted on every connection. A short `Max-Age` would only mean the user
/// re-enters a code for no security gain.
pub const COOKIE_MAX_AGE_SECS: u64 = 400 * 24 * 60 * 60;

/// Where the browser POSTs its bootstrap code (Q4).
pub const AUTH_EXCHANGE_PATH: &str = "/auth/exchange";

/// A cheap "does my cookie still work?" probe, so the SPA can decide between
/// the app and the code-entry screen without opening a WebSocket it is about to
/// be refused.
pub const AUTH_SESSION_PATH: &str = "/auth/session";

/// The WebSocket endpoint protocol v1 rides on (D12).
pub const WS_PATH: &str = "/ws";

// ===========================================================================
// Tuning
// ===========================================================================

/// Frames a viewer may fall behind by before the host stops holding them.
///
/// A viewer that cannot drain a thousand frames from a LAN socket is not a
/// viewer any more, and growing the host's heap on its behalf is the wrong
/// trade. It is dropped instead, and comes back through the reconnect path,
/// which resumes from its byte cursor (Q3) — the machinery that exists for
/// exactly this.
const VIEWER_QUEUE_FRAMES: usize = 1024;

/// How long [`WebServerHandle::stop`] waits for the server task to finish.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// How long the shutdown path waits for every viewer to write its
/// [`ServerMsg::Shutdown`] frame and close, before the listener goes away
/// anyway (Q5).
const DRAIN_GRACE: Duration = Duration::from_secs(3);

/// How many disconnected viewers' input cursors are remembered, so a reconnect
/// can be told what already landed ([`Snapshot::last_input_seq`]).
const REMEMBERED_INPUT_CURSORS: usize = 64;

/// The label the desktop's own seat row carries (2f's `desktop + this tab`).
const DESKTOP_SEAT_LABEL: &str = "desktop";

// ===========================================================================
// The state the TUI publishes
// ===========================================================================

/// Everything a [`Snapshot`] needs that is **not** per-viewer.
///
/// The TUI owns `AppState` and cannot be interrogated from an async task, so it
/// pushes this in instead: [`WebServerHandle::publish_state`] replaces the
/// server's copy wholesale, and every later attach is answered from it.
///
/// Publishing is **not** a broadcast. It changes what the *next* attach sees and
/// nothing else, deliberately: only the host knows which [`Delta`] honestly
/// describes a change it just made, and a server-side diff would invent one.
/// The TUI therefore does both — publish the new state, then
/// [`WebServerHandle::send`] the delta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostState {
    /// FlightDeck's version string, for the browser's "reload to update" prompt.
    pub host_version: String,
    /// Every open project and its sessions.
    pub projects: Vec<ProjectView>,
    /// The shared selection (D3).
    pub selection: Selection,
    /// The host's authoritative grid for the selected terminal (D4).
    pub geometry: Geometry,
    /// `[web] replay_bytes` (Q2), so a viewer can explain a truncated resume.
    pub replay_capacity_bytes: u64,
    /// Activity-feed backfill, oldest first (D11).
    pub activity: Vec<ActivityEvent>,
    /// The open dialog, if any (D13).
    pub dialog: Option<DialogView>,
}

impl Default for HostState {
    fn default() -> Self {
        HostState {
            host_version: env!("CARGO_PKG_VERSION").to_string(),
            projects: Vec::new(),
            selection: Selection::default(),
            geometry: Geometry { cols: 80, rows: 24 },
            replay_capacity_bytes: 0,
            activity: Vec::new(),
            dialog: None,
        }
    }
}

// ===========================================================================
// The channel API the TUI uses
// ===========================================================================

/// Host → viewers. Handed to [`WebServerHandle::send`].
///
/// This is one half of the seam `src/web/stream.rs` plugs into: it pushes
/// [`ServerMsg::TermBytes`] as [`WebOutbound::All`] for live output and as
/// [`WebOutbound::Viewer`] for a targeted resume, and [`ServerMsg::Ack`] as
/// [`WebOutbound::Viewer`] once a keystroke has actually reached the PTY.
#[derive(Clone, Debug)]
pub enum WebOutbound {
    /// To every attached viewer, controller and observers alike.
    All(ServerMsg),
    /// To exactly one viewer — a resume replay, an ack, a targeted refusal.
    /// Silently dropped if that viewer has gone.
    Viewer {
        /// Who to send it to.
        viewer_id: ViewerId,
        /// What to send.
        msg: ServerMsg,
    },
    /// To whichever viewer currently holds the controlling seat, if any.
    Controller(ServerMsg),
}

/// Viewers → host. The TUI drains these non-blockingly each render tick, the
/// same way it drains [`crate::remote::client`]'s inbound channel.
#[derive(Clone, Debug)]
pub enum WebInbound {
    /// A browser attached (or re-attached after a takeover). `cursors` is what
    /// `src/web/stream.rs` needs to answer a resume (Q3).
    ViewerAttached {
        /// The viewer's id for the life of this socket.
        viewer_id: ViewerId,
        /// The peer's IP, as observed on the socket.
        address: IpAddr,
        /// The chip label the host will show (`192.168.2.20 · Chrome on macOS`).
        label: String,
        /// What it ended up holding.
        seat: Seat,
        /// Per-terminal byte cursors it wants to resume from.
        cursors: Vec<TermCursor>,
    },
    /// A browser's socket closed.
    ViewerDetached {
        /// The viewer that went away.
        viewer_id: ViewerId,
    },
    /// The seat map changed — someone attached, left, took over, was evicted,
    /// or released the seat. Carries the same rows the viewers were just told
    /// about, so the desktop's viewer chip can render without asking.
    SeatsChanged {
        /// Everyone attached, desktop row first.
        seats: Vec<SeatInfo>,
    },
    /// Keystrokes from the **controlling** viewer. An observer's input never
    /// reaches here: it is answered [`AckOutcome::Ignored`] by the server.
    ///
    /// The other half of the `stream.rs` seam. Whoever applies these owns the
    /// [`Ack`]: this module has forwarded a frame, which is not the same claim
    /// as "the PTY accepted it", so it does not ack on the applier's behalf.
    Input {
        /// Who typed.
        viewer_id: ViewerId,
        /// What they typed.
        input: crate::web::protocol::Input,
    },
    /// A named command from the controlling viewer (the M2 door, D13). The
    /// server answers `release_seat` and `request_snapshot` itself and refuses
    /// unknown names with [`ErrorCode::NotSupported`], so only M1's remaining
    /// commands arrive here.
    Command {
        /// Who asked.
        viewer_id: ViewerId,
        /// What they asked for.
        command: crate::web::protocol::Command,
    },
    /// A viewer reported its viewport. **Display only** — D4 means this can
    /// never reach `portable_pty`, and [`Viewport`] structurally cannot name a
    /// terminal to resize.
    Resize {
        /// Who reported.
        viewer_id: ViewerId,
        /// What it can show.
        viewport: Viewport,
    },
}

/// Why the socket is closing, as [`WebServerHandle::stop`] is told it (Q5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShutdownNotice {
    /// The reason the browser branches on.
    pub reason: ShutdownReason,
    /// Extra human-readable detail, rendered verbatim.
    pub detail: Option<String>,
    /// The viewer that caused this, when a browser did (a `Ctrl-q` sent from a
    /// tab). That tab is told `self_initiated: true` and shows an
    /// acknowledgement of its own action; everyone else sees a failure. Q5 is
    /// explicit that the difference is not derivable from the reason alone.
    pub initiator: Option<ViewerId>,
}

impl ShutdownNotice {
    /// `Stop Web Interface` (D10): FlightDeck keeps running, the server does not.
    pub fn server_stopped() -> Self {
        ShutdownNotice {
            reason: ShutdownReason::ServerStopped,
            detail: None,
            initiator: None,
        }
    }

    /// FlightDeck itself is quitting.
    pub fn host_quit(initiator: Option<ViewerId>) -> Self {
        ShutdownNotice {
            reason: ShutdownReason::HostQuit,
            detail: None,
            initiator,
        }
    }
}

/// Why the server could not start.
#[derive(Debug)]
pub enum StartError {
    /// The shared runtime could not be created. The TUI runs on exactly as
    /// before, minus the web interface — the same degradation the relay client
    /// takes.
    NoRuntime,
    /// `bind`/`port` could not be listened on: the address does not exist on
    /// this host, the port is in use, or it needs privileges.
    Bind {
        /// What was attempted, e.g. `127.0.0.1:8477`.
        address: String,
        /// The OS's reason.
        reason: String,
    },
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::NoRuntime => f.write_str("the shared async runtime could not be started"),
            StartError::Bind { address, reason } => {
                write!(f, "could not bind {address}: {reason}")
            }
        }
    }
}

impl std::error::Error for StartError {}

/// Whether the bound address is reachable from other machines (D5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindExposure {
    /// `127.0.0.1` / `::1` — this machine only. The default.
    Loopback,
    /// Anything else. Only ever reached because the user typed a non-loopback
    /// `[web] bind` themselves, and the UI warns when the server actually
    /// starts.
    Routable,
}

/// Classify a configured `[web] bind` string **before** binding, so the access
/// overlay can warn the user about what they are about to do.
///
/// Anything that is not recognisably loopback is treated as [`Routable`]. The
/// conservative direction is deliberate: a hostname this function cannot parse
/// must produce a warning, never silence.
///
/// [`Routable`]: BindExposure::Routable
pub fn bind_exposure(bind: &str) -> BindExposure {
    let trimmed = bind.trim();
    if trimmed.eq_ignore_ascii_case("localhost") {
        return BindExposure::Loopback;
    }
    // `[::1]` as well as `::1`, since a user may copy the bracketed form.
    let bare = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    match bare.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => BindExposure::Loopback,
        _ => BindExposure::Routable,
    }
}

// ===========================================================================
// The handle
// ===========================================================================

/// The TUI's end of the server. Dropping it stops the server.
pub struct WebServerHandle {
    shared: Arc<Shared>,
    bound: SocketAddr,
    exposure: BindExposure,
    /// `None` once [`WebServerHandle::stop`] has taken it.
    shutdown: Option<watch::Sender<Option<ShutdownNotice>>>,
    /// Reports `Disconnected` when the server task has finished, which is how
    /// `stop` waits for the listener to be released without needing a runtime
    /// context.
    done: Option<std::sync::mpsc::Receiver<()>>,
}

impl WebServerHandle {
    /// The address actually bound. With `[web] port = 0` (how the tests avoid
    /// colliding) this is where the OS put the listener, so the caller can print
    /// or QR-encode a URL that works.
    pub fn bound_addr(&self) -> SocketAddr {
        self.bound
    }

    /// Whether the bound address is reachable off this machine (D5), derived
    /// from the address the listener really got rather than from the config
    /// string.
    pub fn exposure(&self) -> BindExposure {
        self.exposure
    }

    /// Replace the state that answers the next attach. Cheap to call every
    /// frame; see [`HostState`] on why it does not notify anyone.
    pub fn publish_state(&self, state: HostState) {
        self.shared.state.send_replace(Arc::new(state));
    }

    /// Push a frame at one, some, or all viewers. Never blocks and never fails:
    /// a viewer that has gone, or that has fallen [`VIEWER_QUEUE_FRAMES`]
    /// behind, simply stops receiving.
    pub fn send(&self, out: WebOutbound) {
        self.shared.dispatch(out);
    }

    /// Everyone attached, desktop row first — what the desktop's viewer chip
    /// renders. `is_you` is false on every row, because the desktop is not a
    /// viewer.
    pub fn seats(&self) -> Vec<SeatInfo> {
        self.shared.registry().seat_rows(None)
    }

    /// How many browsers are attached (observers included).
    pub fn viewer_count(&self) -> usize {
        self.shared.registry().viewers.len()
    }

    /// Tell every viewer why the socket is closing, then close the listener and
    /// end the task (Q5).
    ///
    /// The ordering is the point: the [`ServerMsg::Shutdown`] frames are written
    /// **before** the listener goes away, so a browser can tell a deliberate
    /// quit from a network failure instead of spinning in "reconnecting…". The
    /// shared runtime is untouched — this stops a task, never the runtime the
    /// relay client is also using.
    ///
    /// Callable from any thread, including from inside an async context: it
    /// waits on a plain channel rather than `block_on`.
    pub fn stop(mut self, notice: ShutdownNotice) {
        self.signal(Some(notice));
        if let Some(done) = self.done.take() {
            // `Err` means the task already finished, or the grace elapsed;
            // either way there is nothing left to wait for.
            let _ = done.recv_timeout(STOP_GRACE);
        }
    }

    fn signal(&mut self, notice: Option<ShutdownNotice>) {
        if let Some(tx) = self.shutdown.take() {
            // A failed send means the task already ended; dropping `tx` is
            // itself a shutdown signal, so there is nothing to recover.
            let _ = tx.send(notice.or_else(|| Some(ShutdownNotice::server_stopped())));
        }
    }
}

impl Drop for WebServerHandle {
    fn drop(&mut self) {
        // A handle that goes out of scope without `stop` must still release the
        // listener, or "start, stop, start again" would fail on the second bind.
        // Best effort and non-blocking: `stop` is the path that waits.
        self.signal(None);
    }
}

impl std::fmt::Debug for WebServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebServerHandle")
            .field("bound", &self.bound)
            .field("exposure", &self.exposure)
            .field("viewers", &self.viewer_count())
            .finish()
    }
}

// ===========================================================================
// Starting
// ===========================================================================

/// Start the server on the shared runtime.
///
/// Binds `config.bind:config.port` — loopback by default (D5); a routable
/// address is only ever reached because the user typed one into `config.toml`,
/// and [`WebServerHandle::exposure`] reports which happened so the caller can
/// warn.
///
/// `credentials` is shared with the TUI (which mints bootstrap codes, revokes
/// and rotates) rather than owned here, so a revocation takes effect on the very
/// next connection.
///
/// Returns once the listener is bound, so `bound_addr` is immediately usable.
/// The `bind` itself happens **inside** the spawned task, because tokio I/O
/// construction needs a runtime context.
pub fn start(
    config: &WebConfig,
    credentials: Arc<Mutex<CredentialStore>>,
    clock: Arc<dyn Clock + Send + Sync>,
    initial_state: HostState,
    inbound: std::sync::mpsc::Sender<WebInbound>,
) -> Result<WebServerHandle, StartError> {
    let Some(rt) = runtime::try_shared() else {
        debuglog::log("web START skipped — no async runtime");
        return Err(StartError::NoRuntime);
    };

    let address = listen_address(&config.bind, config.port);
    let (shutdown_tx, shutdown_rx) = watch::channel::<Option<ShutdownNotice>>(None);
    let (state_tx, state_rx) = watch::channel(Arc::new(initial_state));
    let started_ms = clock.now_millis() as i64;

    let shared = Arc::new(Shared {
        credentials,
        clock,
        inbound: Mutex::new(inbound),
        state: state_tx,
        state_rx,
        registry: Mutex::new(SeatRegistry::new(started_ms)),
        shutdown: shutdown_rx,
        drain: Arc::new(Drain::default()),
    });

    // Rendezvous: the task reports the bind result, then serves. Blocking the
    // caller here is bounded by one `bind` syscall and is what lets the palette
    // command say "listening on …" or "could not bind" synchronously.
    let (bound_tx, bound_rx) = std::sync::mpsc::channel::<Result<SocketAddr, String>>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

    let task_shared = Arc::clone(&shared);
    let task_address = address.clone();
    rt.spawn(async move {
        // Owned by the task: dropped when it returns, which is what unblocks
        // `stop`. Never sent on — its drop is the signal.
        let _done = done_tx;
        let listener = match tokio::net::TcpListener::bind(task_address.as_str()).await {
            Ok(listener) => listener,
            Err(e) => {
                let _ = bound_tx.send(Err(e.to_string()));
                return;
            }
        };
        let bound = match listener.local_addr() {
            Ok(addr) => addr,
            Err(e) => {
                let _ = bound_tx.send(Err(e.to_string()));
                return;
            }
        };
        if bound_tx.send(Ok(bound)).is_err() {
            // Nobody is waiting for this server; do not hold the port.
            return;
        }
        serve(task_shared, listener).await;
    });

    match bound_rx.recv() {
        Ok(Ok(bound)) => {
            let exposure = if bound.ip().is_loopback() {
                BindExposure::Loopback
            } else {
                BindExposure::Routable
            };
            debuglog::log(&format!(
                "web START bound={bound} exposure={}",
                match exposure {
                    BindExposure::Loopback => "loopback",
                    BindExposure::Routable => "routable",
                }
            ));
            Ok(WebServerHandle {
                shared,
                bound,
                exposure,
                shutdown: Some(shutdown_tx),
                done: Some(done_rx),
            })
        }
        Ok(Err(reason)) => {
            debuglog::log(&format!("web START failed address={address}"));
            Err(StartError::Bind { address, reason })
        }
        // The task died before reporting — treat as a runtime failure.
        Err(_) => Err(StartError::NoRuntime),
    }
}

/// Build the `host:port` string to bind, bracketing a bare IPv6 literal.
fn listen_address(bind: &str, port: u16) -> String {
    let host = bind.trim();
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Run axum until shutdown, then release the listener.
async fn serve(shared: Arc<Shared>, listener: tokio::net::TcpListener) {
    let app = router(Arc::clone(&shared));
    let graceful = {
        let mut shutdown = shared.shutdown.clone();
        let drain = Arc::clone(&shared.drain);
        async move {
            let notice = await_shutdown(&mut shutdown).await;
            debuglog::log(&format!("web SHUTDOWN reason={}", notice.reason.as_str()));
            // Q5: every viewer must have written its `Shutdown` frame before the
            // listener closes. Each connection drops its drain guard only after
            // that write, so waiting for the count to reach zero *is* waiting
            // for the frames to be out. Bounded, so one wedged socket cannot
            // hold the port.
            let _ = tokio::time::timeout(DRAIN_GRACE, drain.wait_for_idle()).await;
        }
    };
    let served = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(graceful)
    .await;
    if let Err(e) = served {
        debuglog::log(&format!("web SERVE ended err={e}"));
    }
    // The runtime is `&'static` and shared with the relay client (D6/D7): this
    // task ends, the runtime keeps running.
}

fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route(WS_PATH, get(ws_route))
        .route(AUTH_EXCHANGE_PATH, post(exchange_route))
        .route(AUTH_SESSION_PATH, get(session_route))
        // Everything else is the SPA: `/`, its hashed assets, and its
        // client-side routes (D9). `assets::lookup` owns the fallback rules.
        .fallback(asset_route)
        .with_state(shared)
}

// ===========================================================================
// Shared server state
// ===========================================================================

/// Everything the routes and the per-viewer tasks share.
struct Shared {
    credentials: Arc<Mutex<CredentialStore>>,
    clock: Arc<dyn Clock + Send + Sync>,
    /// The TUI's inbound channel. `std::sync::mpsc::Sender` is `Send` but not
    /// `Sync`, so it lives behind a mutex; the lock is only ever held for the
    /// length of one `send` and never across an `await`.
    inbound: Mutex<std::sync::mpsc::Sender<WebInbound>>,
    state: watch::Sender<Arc<HostState>>,
    state_rx: watch::Receiver<Arc<HostState>>,
    registry: Mutex<SeatRegistry>,
    shutdown: watch::Receiver<Option<ShutdownNotice>>,
    drain: Arc<Drain>,
}

impl Shared {
    fn registry(&self) -> std::sync::MutexGuard<'_, SeatRegistry> {
        // A poisoned seat registry would mean a panic mid-arbitration. Recover
        // rather than propagate: the alternative is that one panicking viewer
        // takes the whole web interface down with it.
        self.registry.unwrap_or_recover()
    }

    fn now_ms(&self) -> i64 {
        self.clock.now_millis() as i64
    }

    /// Tell the TUI something. Dropped silently once the TUI has gone away.
    fn notify(&self, msg: WebInbound) {
        if let Ok(tx) = self.inbound.lock() {
            let _ = tx.send(msg);
        }
    }

    /// Route one [`WebOutbound`] to its recipients.
    fn dispatch(&self, out: WebOutbound) {
        let mut registry = self.registry();
        match out {
            WebOutbound::All(msg) => registry.send_all(&msg),
            WebOutbound::Viewer { viewer_id, msg } => registry.send_to(&viewer_id, msg),
            WebOutbound::Controller(msg) => {
                if let Some(id) = registry.controller().cloned() {
                    registry.send_to(&id, msg);
                }
            }
        }
    }

    /// Build the snapshot a viewer gets on attach.
    fn snapshot_for(&self, viewer_id: &ViewerId, seat: Seat, last_input_seq: u64) -> Snapshot {
        let state = self.state_rx.borrow().clone();
        let seats = self.registry().seat_rows(Some(viewer_id));
        Snapshot {
            protocol_version: crate::web::protocol::PROTOCOL_VERSION,
            host_version: state.host_version.clone(),
            server_time_ms: self.now_ms(),
            viewer_id: viewer_id.clone(),
            seat,
            seats,
            last_input_seq,
            projects: state.projects.clone(),
            selection: state.selection.clone(),
            geometry: state.geometry,
            replay_capacity_bytes: state.replay_capacity_bytes,
            activity: state.activity.clone(),
            dialog: state.dialog.clone(),
        }
    }

    /// Fan out a [`Delta::Seats`] to everyone (each recipient's `you` differs)
    /// and tell the TUI, after any seat change.
    fn announce_seats(&self) {
        let (frames, rows) = {
            let registry = self.registry();
            (registry.seat_frames(), registry.seat_rows(None))
        };
        {
            let mut registry = self.registry();
            for (id, msg) in frames {
                registry.send_to(&id, msg);
            }
        }
        self.notify(WebInbound::SeatsChanged { seats: rows });
    }
}

/// `Mutex::lock` that survives a poisoned lock. See [`Shared::registry`].
trait UnwrapOrRecover<T> {
    fn unwrap_or_recover(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> UnwrapOrRecover<T> for Mutex<T> {
    fn unwrap_or_recover(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

// ===========================================================================
// Draining (Q5's ordering guarantee)
// ===========================================================================

/// A count of connections that have not yet finished writing their goodbye.
#[derive(Default)]
struct Drain {
    live: AtomicUsize,
    notify: Notify,
}

impl Drain {
    fn enter(self: &Arc<Self>) -> DrainGuard {
        self.live.fetch_add(1, Ordering::AcqRel);
        DrainGuard(Arc::clone(self))
    }

    async fn wait_for_idle(&self) {
        loop {
            // Register interest *before* the check, so a decrement that lands
            // between the two cannot be missed.
            let notified = self.notify.notified();
            if self.live.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }
}

/// Held by a connection task for its whole life. Its `Drop` is what tells the
/// shutdown path that this viewer is done.
struct DrainGuard(Arc<Drain>);

impl Drop for DrainGuard {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::AcqRel);
        self.0.notify.notify_waiters();
    }
}

/// Resolve once shutdown has been requested — either explicitly, or by the
/// [`WebServerHandle`] being dropped (which drops the sender).
async fn await_shutdown(rx: &mut watch::Receiver<Option<ShutdownNotice>>) -> ShutdownNotice {
    loop {
        // Scoped so the borrow guard is released before the await below.
        let current = rx.borrow_and_update().clone();
        if let Some(notice) = current {
            return notice;
        }
        if rx.changed().await.is_err() {
            // The handle was dropped without `stop`. Wind down anyway rather
            // than holding the listener past the app's lifetime.
            return ShutdownNotice::server_stopped();
        }
    }
}

// ===========================================================================
// Seat arbitration (D14)
// ===========================================================================

/// One attached browser.
struct Viewer {
    id: ViewerId,
    address: IpAddr,
    label: String,
    seat: Seat,
    since_ms: i64,
    tx: mpsc::Sender<ServerMsg>,
}

impl Viewer {
    fn info(&self, you: Option<&ViewerId>) -> SeatInfo {
        SeatInfo {
            viewer_id: Some(self.id.clone()),
            label: self.label.clone(),
            seat: self.seat,
            since_ms: self.since_ms,
            is_you: you == Some(&self.id),
        }
    }
}

/// One controlling browser plus N observers (D14), and the takeover path.
///
/// Insertion order is display order, so the viewer chip's rows are stable while
/// a tab is attached.
struct SeatRegistry {
    viewers: Vec<Viewer>,
    /// Highest [`crate::web::protocol::Input::seq`] forwarded per viewer, kept
    /// after a disconnect so a reconnect can be told what already landed
    /// ([`Snapshot::last_input_seq`]). Bounded by
    /// [`REMEMBERED_INPUT_CURSORS`], oldest dropped first.
    input_cursors: Vec<(ViewerId, u64)>,
    /// When the server started — the desktop row's `since_ms`.
    started_ms: i64,
}

/// What happened when a viewer asked for a seat.
#[derive(Debug, PartialEq, Eq)]
enum SeatOutcome {
    /// The seat (or observer status) was granted.
    Granted(Seat),
    /// [`SeatRequest::Control`] was refused because someone holds it. The
    /// incumbent is left alone; the browser renders the takeover prompt (2f).
    Refused(SeatInfo),
}

impl SeatRegistry {
    fn new(started_ms: i64) -> Self {
        SeatRegistry {
            viewers: Vec::new(),
            input_cursors: Vec::new(),
            started_ms,
        }
    }

    /// Add a viewer, or return the existing one's slot. New viewers start as
    /// observers: a seat is something you then ask for.
    fn register(
        &mut self,
        id: ViewerId,
        address: IpAddr,
        label: String,
        since_ms: i64,
        tx: mpsc::Sender<ServerMsg>,
    ) {
        if self.viewers.iter().any(|v| v.id == id) {
            return;
        }
        self.viewers.push(Viewer {
            id,
            address,
            label,
            seat: Seat::Observing,
            since_ms,
            tx,
        });
    }

    fn remove(&mut self, id: &ViewerId) {
        self.viewers.retain(|v| &v.id != id);
    }

    fn controller(&self) -> Option<&ViewerId> {
        self.viewers
            .iter()
            .find(|v| v.seat == Seat::Controlling)
            .map(|v| &v.id)
    }

    fn seat_of(&self, id: &ViewerId) -> Option<Seat> {
        self.viewers.iter().find(|v| &v.id == id).map(|v| v.seat)
    }

    /// Arbitrate one [`SeatRequest`]. The viewer must already be registered.
    ///
    /// Takeover has **no dedicated frame** in protocol v1 — the client re-sends
    /// `Attach { seat: TakeOver }` — and eviction is a [`Delta::Seats`], never a
    /// [`ServerMsg::Shutdown`], because the evicted socket stays open as an
    /// observer (2f).
    fn request_seat(&mut self, id: &ViewerId, request: SeatRequest) -> SeatOutcome {
        match request {
            SeatRequest::Observe => {
                self.set_seat(id, Seat::Observing);
                SeatOutcome::Granted(Seat::Observing)
            }
            SeatRequest::Control => match self.controller().cloned() {
                Some(incumbent) if &incumbent != id => {
                    let info = self
                        .viewers
                        .iter()
                        .find(|v| v.id == incumbent)
                        .map(|v| v.info(None))
                        .expect("the controller is in the viewer list");
                    SeatOutcome::Refused(info)
                }
                _ => {
                    self.set_seat(id, Seat::Controlling);
                    SeatOutcome::Granted(Seat::Controlling)
                }
            },
            SeatRequest::TakeOver => {
                for viewer in self.viewers.iter_mut() {
                    if &viewer.id != id && viewer.seat == Seat::Controlling {
                        // Demoted, not disconnected.
                        viewer.seat = Seat::Observing;
                    }
                }
                self.set_seat(id, Seat::Controlling);
                SeatOutcome::Granted(Seat::Controlling)
            }
        }
    }

    /// Give up the controlling seat voluntarily (`release_seat`).
    fn release(&mut self, id: &ViewerId) {
        self.set_seat(id, Seat::Observing);
    }

    fn set_seat(&mut self, id: &ViewerId, seat: Seat) {
        if let Some(viewer) = self.viewers.iter_mut().find(|v| &v.id == id) {
            viewer.seat = seat;
        }
    }

    /// The rows for the viewer chip, desktop first (2f).
    ///
    /// The desktop is [`Seat::Controlling`] unconditionally, because its
    /// keyboard is never revoked — a browser taking over does not stop the
    /// person at the machine from typing. The single *web* controller is the row
    /// with `viewer_id: Some(_)` and [`Seat::Controlling`], of which there is at
    /// most one.
    fn seat_rows(&self, you: Option<&ViewerId>) -> Vec<SeatInfo> {
        let mut rows = Vec::with_capacity(self.viewers.len() + 1);
        rows.push(SeatInfo {
            viewer_id: None,
            label: DESKTOP_SEAT_LABEL.to_string(),
            seat: Seat::Controlling,
            since_ms: self.started_ms,
            is_you: false,
        });
        rows.extend(self.viewers.iter().map(|v| v.info(you)));
        rows
    }

    /// One `Delta::Seats` per viewer, each with that viewer's own `you`.
    fn seat_frames(&self) -> Vec<(ViewerId, ServerMsg)> {
        self.viewers
            .iter()
            .map(|v| {
                (
                    v.id.clone(),
                    ServerMsg::Delta(Delta::Seats {
                        you: v.seat,
                        seats: self.seat_rows(Some(&v.id)),
                    }),
                )
            })
            .collect()
    }

    /// Queue a frame for one viewer. A viewer whose queue is full has stopped
    /// keeping up and is dropped from the registry — its connection task sees
    /// its channel close and shuts the socket. See [`VIEWER_QUEUE_FRAMES`].
    fn send_to(&mut self, id: &ViewerId, msg: ServerMsg) {
        let Some(viewer) = self.viewers.iter().find(|v| &v.id == id) else {
            return;
        };
        if viewer.tx.try_send(msg).is_err() {
            debuglog::log(&format!(
                "web VIEWER dropped id={id} address={} reason=queue_full",
                viewer.address
            ));
            self.remove(id);
        }
    }

    fn send_all(&mut self, msg: &ServerMsg) {
        let ids: Vec<ViewerId> = self.viewers.iter().map(|v| v.id.clone()).collect();
        for id in ids {
            self.send_to(&id, msg.clone());
        }
    }

    /// The last input seq this host forwarded for `id`, or zero.
    fn input_cursor(&self, id: &ViewerId) -> u64 {
        self.input_cursors
            .iter()
            .find(|(known, _)| known == id)
            .map(|(_, seq)| *seq)
            .unwrap_or(0)
    }

    fn record_input(&mut self, id: &ViewerId, seq: u64) {
        if let Some(entry) = self.input_cursors.iter_mut().find(|(known, _)| known == id) {
            entry.1 = entry.1.max(seq);
            return;
        }
        if self.input_cursors.len() >= REMEMBERED_INPUT_CURSORS {
            self.input_cursors.remove(0);
        }
        self.input_cursors.push((id.clone(), seq));
    }

    /// Carry a previous connection's input cursor onto the id resuming it, so
    /// `Attach { resume_viewer }` can be answered honestly (§5.1).
    fn adopt_cursor(&mut self, previous: &ViewerId, now: &ViewerId) -> u64 {
        let seq = self.input_cursor(previous);
        if seq > 0 {
            self.record_input(now, seq);
        }
        seq
    }
}

// ===========================================================================
// Credentials on the HTTP path
// ===========================================================================

/// The address the per-address rate limiter is keyed by: **the TCP peer's IP,
/// without the port**, which is what `credentials.rs` documents wanting.
///
/// This function takes no headers, and that is the security property, not an
/// omission. `X-Forwarded-For`, `X-Real-IP` and `Forwarded` are all set by the
/// client on a direct connection, so honouring any of them would let a browser
/// mint a fresh attempt budget per guess and defeat the limiter entirely — and
/// let a LAN peer claim to be loopback. D1/D5 put no reverse proxy in front of
/// this server: it is reached directly on loopback or the LAN, so there is no
/// deployment in which a forwarding header would carry more truth than the
/// socket. **No proxy header is trusted, ever.**
fn rate_limit_address(peer: SocketAddr) -> String {
    peer.ip().to_string()
}

/// Read the access cookie out of a `Cookie` header.
///
/// Hand-rolled rather than pulled from a cookie crate: one name, one value, no
/// attributes to parse on the way in, and the value alphabet is base64url.
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| key.trim() == name)
        .map(|(_, value)| value.trim().to_string())
}

/// The `Set-Cookie` value for a freshly issued token.
///
/// * `HttpOnly` — Q4's own requirement: script on the page never reads it, so an
///   XSS in the SPA cannot exfiltrate the credential.
/// * **No `Secure`.** This server speaks plain HTTP on loopback or the LAN (D1:
///   there is no relay, no certificate and no name to put on one). A `Secure`
///   cookie would never be sent back over `http://`, so setting it would not
///   harden anything — it would break authentication outright. Recording the
///   absence deliberately rather than by omission.
/// * `SameSite=Lax` — a middle that is doing real work here. `Strict` would drop
///   the cookie on the documented entry paths: a link or QR opened from another
///   app arrives as a cross-site top-level navigation, and `Strict` would send
///   the user back to the code screen every time. `None` requires `Secure`,
///   which we cannot set. `Lax` sends the cookie on top-level navigations only,
///   which means a hostile page's `new WebSocket("ws://127.0.0.1:…/ws")` — not a
///   navigation — carries no cookie and is refused. That is the cross-site
///   WebSocket-hijacking case, and `Lax` closes it.
/// * `Path=/` so the SPA's own client-side routes keep it, and a long `Max-Age`
///   ([`COOKIE_MAX_AGE_SECS`]) because revocation is server-side.
fn set_cookie_value(secret: &str) -> String {
    format!("{COOKIE_NAME}={secret}; Path=/; HttpOnly; SameSite=Lax; Max-Age={COOKIE_MAX_AGE_SECS}")
}

/// The wire spelling of the browser screen a refusal maps to (artboard 2b).
fn screen_name(screen: AccessScreen) -> &'static str {
    match screen {
        AccessScreen::CodeEntry => "code_entry",
        AccessScreen::Rejected => "rejected",
        AccessScreen::Revoked => "revoked",
        AccessScreen::RateLimited => "rate_limited",
    }
}

/// The HTTP status a refusal deserves: `429` only for the limiter, `401`
/// otherwise.
fn refusal_status(failure: AuthFailure) -> StatusCode {
    if failure.is_rate_limited() {
        StatusCode::TOO_MANY_REQUESTS
    } else {
        StatusCode::UNAUTHORIZED
    }
}

fn json_response(status: StatusCode, body: serde_json::Value) -> Response {
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    // A credential response must never be cached by a browser or an
    // intermediary on the way back.
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// The JSON body for a refusal: enough for the SPA to pick one of artboard 2b's
/// screens and count down, and nothing about the credential itself.
fn refusal_body(failure: AuthFailure, attempts_remaining: u32) -> serde_json::Value {
    let mut body = serde_json::json!({
        "ok": false,
        "screen": screen_name(failure.screen()),
        "reason": failure.as_str(),
        "attempts_remaining": attempts_remaining,
    });
    if let AuthFailure::RateLimited { retry_after_ms } = failure {
        body["retry_after_ms"] = serde_json::json!(retry_after_ms);
    }
    body
}

/// Add `Retry-After` (whole seconds, rounded up) when the limiter refused.
/// The JSON body carries the precise millisecond value the countdown uses.
fn attach_retry_after(response: &mut Response, failure: AuthFailure) {
    if let AuthFailure::RateLimited { retry_after_ms } = failure {
        let seconds = retry_after_ms.div_ceil(1000).max(1);
        if let Ok(value) = header::HeaderValue::from_str(&seconds.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
}

fn refusal_response(failure: AuthFailure, attempts_remaining: u32) -> Response {
    let mut response = json_response(
        refusal_status(failure),
        refusal_body(failure, attempts_remaining),
    );
    attach_retry_after(&mut response, failure);
    response
}

// ===========================================================================
// Routes
// ===========================================================================

/// The SPA and its assets (D9).
async fn asset_route(uri: axum::http::Uri) -> Response {
    // Deliberately unauthenticated. The SPA's own shell has to load before it
    // can show the code-entry screen at all, and it contains nothing secret —
    // every fact the app renders arrives over the authenticated WebSocket.
    match assets::lookup(uri.path()) {
        Lookup::Found(asset) => asset_response(StatusCode::OK, asset),
        Lookup::NotBuilt => asset_response(StatusCode::OK, assets::not_built_page()),
        Lookup::NotFound => {
            let mut response = Response::new(Body::from("not found"));
            *response.status_mut() = StatusCode::NOT_FOUND;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            response
        }
    }
}

fn asset_response(status: StatusCode, asset: assets::Asset) -> Response {
    let mut response = Response::new(Body::from(asset.body.into_owned()));
    *response.status_mut() = status;
    if let Ok(value) = header::HeaderValue::from_str(&asset.content_type) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    response
}

/// The body of a credential exchange (Q4).
#[derive(serde::Deserialize)]
struct ExchangeRequest {
    /// The bootstrap code the browser read from its **URL fragment**. It arrives
    /// in a POST body — never a query string, never a path — so it cannot land
    /// in an access log, a referrer, or the browser's history.
    code: String,
    /// A coarse self-description for the desktop's browser list. Untrusted free
    /// text; stored and displayed, never parsed.
    #[serde(default)]
    label: Option<String>,
}

/// `POST /auth/exchange` — the one-time bootstrap-code exchange (Q4).
async fn exchange_route(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    body: Result<axum::Json<ExchangeRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let address = rate_limit_address(peer);
    let Ok(axum::Json(request)) = body else {
        // A malformed body is not a credential attempt, so it must not spend the
        // address's budget — otherwise anyone could lock a browser out with
        // three junk POSTs.
        return json_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "ok": false, "reason": "malformed_request" }),
        );
    };

    let outcome = {
        let mut store = shared.credentials.unwrap_or_recover();
        // `exchange_code` consults the per-address limiter itself before it
        // looks at the digits, so the HTTP path is rate-limited by construction
        // rather than by remembering to ask.
        let result = store.exchange_code(&address, &request.code, request.label.as_deref());
        let attempts = store.attempts_remaining(&address);
        (result, attempts)
    };

    match outcome {
        (Ok(token), _) => {
            // The token id is public (it is what `revoke` names); the secret is
            // not, and is never logged.
            debuglog::log(&format!(
                "web AUTH exchange ok address={address} token={}",
                token.id()
            ));
            let mut response = json_response(StatusCode::OK, serde_json::json!({ "ok": true }));
            if let Ok(value) = header::HeaderValue::from_str(&set_cookie_value(token.reveal())) {
                response.headers_mut().insert(header::SET_COOKIE, value);
            }
            response
        }
        (Err(failure), attempts) => {
            debuglog::log(&format!(
                "web AUTH exchange refused address={address} reason={} attempts_left={attempts}",
                failure.as_str()
            ));
            refusal_response(failure, attempts)
        }
    }
}

/// `GET /auth/session` — is this browser's cookie still good?
async fn session_route(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    match authenticate(&shared, peer, &headers) {
        Ok(_) => json_response(StatusCode::OK, serde_json::json!({ "authenticated": true })),
        Err(failure) => {
            let attempts = shared
                .credentials
                .unwrap_or_recover()
                .attempts_remaining(&rate_limit_address(peer));
            let mut body = refusal_body(failure, attempts);
            body["authenticated"] = serde_json::json!(false);
            let mut response = json_response(refusal_status(failure), body);
            attach_retry_after(&mut response, failure);
            response
        }
    }
}

/// Verify the cookie on a request, keyed to the real peer address.
///
/// A missing cookie is [`AuthFailure::UnknownToken`] — the same answer as a
/// forged one, so the absence of a cookie is not a distinguishable state — but
/// it does **not** spend the address's attempt budget, because a first visit has
/// no cookie by definition and must not be punished for it.
fn authenticate(
    shared: &Shared,
    peer: SocketAddr,
    headers: &HeaderMap,
) -> Result<TokenId, AuthFailure> {
    let address = rate_limit_address(peer);
    let Some(presented) = cookie_value(headers, COOKIE_NAME) else {
        // Still consult the limiter, so a locked-out address is told to wait
        // rather than being invited to guess again — but spend nothing, because
        // a first visit has no cookie by definition.
        let lockout = shared
            .credentials
            .unwrap_or_recover()
            .lockout_remaining_ms(&address);
        if let Some(retry_after_ms) = lockout {
            return Err(AuthFailure::RateLimited { retry_after_ms });
        }
        return Err(AuthFailure::UnknownToken);
    };
    shared
        .credentials
        .unwrap_or_recover()
        .verify_token(&address, &presented)
}

/// `GET /ws` — the protocol v1 upgrade (D12).
///
/// **The credential is checked before the upgrade.** An unauthenticated peer
/// gets an HTTP refusal and no WebSocket, which is the property the tests pin:
/// there is no window in which an unauthenticated socket exists.
async fn ws_route(
    State(shared): State<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let address = rate_limit_address(peer);
    let token = match authenticate(&shared, peer, &headers) {
        Ok(token) => token,
        Err(failure) => {
            debuglog::log(&format!(
                "web WS refused address={address} reason={}",
                failure.as_str()
            ));
            let attempts = shared
                .credentials
                .unwrap_or_recover()
                .attempts_remaining(&address);
            return refusal_response(failure, attempts);
        }
    };

    let label = viewer_label(peer, &headers);
    debuglog::log(&format!(
        "web WS upgrade address={address} token={token} label={label}"
    ));
    upgrade.on_upgrade(move |socket| serve_viewer(shared, socket, peer.ip(), label))
}

/// The chip label for a viewer: the address the host observed, plus whatever the
/// browser says about itself.
///
/// The address comes from the socket; the user-agent is the browser's own claim
/// and is only ever displayed. Truncated so a hostile `User-Agent` cannot make
/// the desktop's chip unreadable.
fn viewer_label(peer: SocketAddr, headers: &HeaderMap) -> String {
    let address = peer.ip().to_string();
    let agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(coarse_user_agent)
        .filter(|s| !s.is_empty());
    match agent {
        Some(agent) => format!("{address} · {}", truncate_chars(&agent, MAX_LABEL_CHARS)),
        None => address,
    }
}

/// How much of a browser-supplied label the chip will carry.
const MAX_LABEL_CHARS: usize = 48;

/// Keep the first `max` **characters**, never splitting a codepoint.
/// `String::truncate` panics on a byte index that is not a char boundary, and
/// the label is attacker-supplied UTF-8.
fn truncate_chars(raw: &str, max: usize) -> String {
    raw.chars().take(max).collect()
}

/// Reduce a `User-Agent` to the `Chrome on macOS` shape turn 2 asks for, keeping
/// only characters that are safe to render verbatim in a terminal chip.
fn coarse_user_agent(raw: &str) -> String {
    let browser = if raw.contains("Firefox/") {
        "Firefox"
    } else if raw.contains("Edg/") {
        "Edge"
    } else if raw.contains("Chrome/") {
        "Chrome"
    } else if raw.contains("Safari/") {
        "Safari"
    } else {
        ""
    };
    let os = if raw.contains("Mac OS X") || raw.contains("Macintosh") {
        "macOS"
    } else if raw.contains("Windows") {
        "Windows"
    } else if raw.contains("Android") {
        "Android"
    } else if raw.contains("iPhone") || raw.contains("iPad") {
        "iOS"
    } else if raw.contains("Linux") {
        "Linux"
    } else {
        ""
    };
    match (browser, os) {
        ("", "") => String::new(),
        (b, "") => b.to_string(),
        ("", o) => o.to_string(),
        (b, o) => format!("{b} on {o}"),
    }
}

// ===========================================================================
// One viewer's connection
// ===========================================================================

/// A random opaque viewer id. Unguessable so it cannot be used to target
/// another tab's frames if a future route ever takes one.
fn new_viewer_id() -> ViewerId {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use rand_core::RngCore;
    let mut bytes = [0u8; 12];
    rand_core::OsRng.fill_bytes(&mut bytes);
    ViewerId::new(URL_SAFE_NO_PAD.encode(bytes))
}

/// Drive one authenticated WebSocket for its whole life.
async fn serve_viewer(shared: Arc<Shared>, socket: WebSocket, address: IpAddr, label: String) {
    let guard = shared.drain.enter();
    let viewer_id = new_viewer_id();
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<ServerMsg>(VIEWER_QUEUE_FRAMES);
    let mut shutdown = shared.shutdown.clone();
    let mut attached = false;

    loop {
        tokio::select! {
            // Frames from the browser.
            incoming = stream.next() => {
                let Some(Ok(message)) = incoming else { break };
                let Some(text) = message_text(message) else { continue };
                let Some(client_msg) = parse_client_msg(&text) else {
                    if send_msg(
                        &mut sink,
                        &ServerMsg::Error(WireError::new(
                            ErrorCode::NotSupported,
                            "frame was not valid protocol v1 JSON",
                        )),
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                    continue;
                };
                let verdict = handle_client_msg(
                    &shared,
                    &viewer_id,
                    address,
                    &label,
                    &tx,
                    &mut attached,
                    client_msg,
                    &mut sink,
                )
                .await;
                if verdict == Flow::Close {
                    break;
                }
            }
            // Frames the host pushed at this viewer.
            queued = rx.recv() => {
                match queued {
                    Some(msg) => {
                        if send_msg(&mut sink, &msg).await.is_err() {
                            break;
                        }
                    }
                    // Every sender dropped: the registry evicted us (it could
                    // not keep up) or the server is going away.
                    None => break,
                }
            }
            // The host is shutting down (Q5).
            notice = await_shutdown(&mut shutdown) => {
                let self_initiated = notice.initiator.as_ref() == Some(&viewer_id);
                let frame = ServerMsg::Shutdown {
                    reason: notice.reason,
                    self_initiated,
                    detail: notice.detail.clone(),
                };
                // Best effort: a socket that cannot take this frame is already
                // gone, and the browser's own disconnect handling covers it.
                let _ = send_msg(&mut sink, &frame).await;
                let _ = sink.send(Message::Close(None)).await;
                break;
            }
        }
    }

    let was_attached = attached;
    {
        let mut registry = shared.registry();
        registry.remove(&viewer_id);
    }
    if was_attached {
        shared.notify(WebInbound::ViewerDetached {
            viewer_id: viewer_id.clone(),
        });
        // Someone leaving is a seat change: the seat they held is free now.
        shared.announce_seats();
    }
    // Dropped last: the shutdown path waits on this, so it must outlive the
    // `Shutdown` frame write above (Q5's ordering).
    drop(guard);
}

/// Whether the connection loop should keep going.
#[derive(Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Close,
}

type Sink = futures_util::stream::SplitSink<WebSocket, Message>;

async fn send_msg(sink: &mut Sink, msg: &ServerMsg) -> Result<(), ()> {
    let json = match serde_json::to_string(msg) {
        Ok(json) => json,
        Err(e) => {
            debuglog::log(&format!("web SEND encode failed err={e}"));
            return Ok(());
        }
    };
    sink.send(Message::Text(json.into())).await.map_err(|_| ())
}

/// The text of a frame, or `None` for anything protocol v1 does not use.
///
/// v1 is JSON over text frames; binary frames are not part of it (terminal bytes
/// are base64 inside the JSON, see `protocol::TermBytes`). Ping/pong are handled
/// by axum itself.
fn message_text(message: Message) -> Option<String> {
    match message {
        Message::Text(text) => Some(text.as_str().to_string()),
        _ => None,
    }
}

/// Parse a client frame. `None` means the bytes were not JSON at all; an unknown
/// `type` parses fine and arrives as [`ClientMsg::Unrecognized`], which is the
/// forward-compatibility policy protocol v1 documents.
fn parse_client_msg(text: &str) -> Option<ClientMsg> {
    serde_json::from_str::<ClientMsg>(text).ok()
}

#[allow(clippy::too_many_arguments)]
async fn handle_client_msg(
    shared: &Arc<Shared>,
    viewer_id: &ViewerId,
    address: IpAddr,
    label: &str,
    tx: &mpsc::Sender<ServerMsg>,
    attached: &mut bool,
    msg: ClientMsg,
    sink: &mut Sink,
) -> Flow {
    match msg {
        ClientMsg::Attach(attach) => {
            handle_attach(
                shared, viewer_id, address, label, tx, attached, attach, sink,
            )
            .await
        }
        _ if !*attached => {
            // The seat, the cursors and the version are all established by
            // `Attach`; nothing else can be answered honestly before it.
            let _ = send_msg(
                sink,
                &ServerMsg::Error(WireError::new(
                    ErrorCode::NotSupported,
                    "attach before sending anything else",
                )),
            )
            .await;
            Flow::Close
        }
        ClientMsg::Input(input) => {
            let seat = shared.registry().seat_of(viewer_id);
            match seat {
                Some(Seat::Controlling) => {
                    shared.registry().record_input(viewer_id, input.seq);
                    // The applier owns the ack: forwarding is not the same claim
                    // as "the PTY took it". `src/web/stream.rs` answers with
                    // `WebOutbound::Viewer { ServerMsg::Ack }`.
                    shared.notify(WebInbound::Input {
                        viewer_id: viewer_id.clone(),
                        input,
                    });
                    Flow::Continue
                }
                _ => {
                    // Acked, never silently dropped (§5.1).
                    let _ = send_msg(
                        sink,
                        &ServerMsg::Ack(Ack {
                            seq: input.seq,
                            outcome: AckOutcome::Ignored,
                            detail: Some(
                                "this tab is watching read-only; take over to type".to_string(),
                            ),
                        }),
                    )
                    .await;
                    Flow::Continue
                }
            }
        }
        ClientMsg::Resize(resize) => {
            shared.notify(WebInbound::Resize {
                viewer_id: viewer_id.clone(),
                viewport: resize.viewport,
            });
            Flow::Continue
        }
        ClientMsg::Command(command) => {
            handle_command(shared, viewer_id, command, sink).await;
            Flow::Continue
        }
        ClientMsg::Unrecognized => {
            let _ = send_msg(
                sink,
                &ServerMsg::Error(WireError::new(
                    ErrorCode::NotSupported,
                    "this FlightDeck does not know that frame type",
                )),
            )
            .await;
            Flow::Continue
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_attach(
    shared: &Arc<Shared>,
    viewer_id: &ViewerId,
    address: IpAddr,
    label: &str,
    tx: &mpsc::Sender<ServerMsg>,
    attached: &mut bool,
    attach: Attach,
    sink: &mut Sink,
) -> Flow {
    if let Err(mismatch) = check_version(attach.protocol_version) {
        let _ = send_msg(
            sink,
            &ServerMsg::Error(WireError::version_mismatch(mismatch)),
        )
        .await;
        // Nothing this build can serve. The browser's answer is "reload to
        // update", not a retry, so the socket closes.
        return Flow::Close;
    }

    // The browser's self-description refines the chip label, but never the
    // address: that came off the socket.
    let label = match attach
        .client
        .as_ref()
        .and_then(|c| c.label.clone().or_else(|| c.user_agent.clone()))
    {
        Some(claim) => {
            let claim = truncate_chars(&sanitize_label(&claim), MAX_LABEL_CHARS);
            if claim.is_empty() {
                label.to_string()
            } else {
                format!("{address} · {claim}")
            }
        }
        None => label.to_string(),
    };

    let now = shared.now_ms();
    let (outcome, last_input_seq) = {
        let mut registry = shared.registry();
        registry.register(viewer_id.clone(), address, label.clone(), now, tx.clone());
        let last_input_seq = match attach.resume_viewer.as_ref() {
            Some(previous) => registry.adopt_cursor(previous, viewer_id),
            None => registry.input_cursor(viewer_id),
        };
        (
            registry.request_seat(viewer_id, attach.seat),
            last_input_seq,
        )
    };

    let seat = match outcome {
        SeatOutcome::Granted(seat) => seat,
        SeatOutcome::Refused(incumbent) => {
            // The incumbent is left alone (D14); the browser renders the
            // takeover prompt and may come back with `TakeOver` or `Observe` on
            // this same socket.
            let _ = send_msg(sink, &ServerMsg::Error(WireError::seat_held(incumbent))).await;
            if !*attached {
                let mut registry = shared.registry();
                registry.remove(viewer_id);
            }
            return Flow::Continue;
        }
    };

    *attached = true;
    let snapshot = shared.snapshot_for(viewer_id, seat, last_input_seq);
    if send_msg(sink, &ServerMsg::Snapshot(snapshot))
        .await
        .is_err()
    {
        return Flow::Close;
    }

    shared.notify(WebInbound::ViewerAttached {
        viewer_id: viewer_id.clone(),
        address,
        label,
        seat,
        cursors: attach.cursors,
    });
    // Any attach can change the seat map — a first attach adds a row, a
    // re-attach may have just evicted the incumbent. Eviction is exactly this
    // `Delta::Seats`, never a `Shutdown`: the evicted socket stays open as an
    // observer (2f).
    shared.announce_seats();
    Flow::Continue
}

/// Keep a browser-supplied label renderable: printable characters only, no
/// control bytes that could move a terminal cursor when the desktop draws the
/// chip.
fn sanitize_label(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

async fn handle_command(
    shared: &Arc<Shared>,
    viewer_id: &ViewerId,
    command: crate::web::protocol::Command,
    sink: &mut Sink,
) {
    use crate::web::protocol::command as names;

    let seat = shared
        .registry()
        .seat_of(viewer_id)
        .unwrap_or(Seat::Observing);

    match command.name.as_str() {
        // Answerable here, from the published state, for any seat: it is how a
        // viewer that believes it has drifted recovers.
        names::REQUEST_SNAPSHOT => {
            let last_input_seq = shared.registry().input_cursor(viewer_id);
            let snapshot = shared.snapshot_for(viewer_id, seat, last_input_seq);
            let _ = send_msg(sink, &ServerMsg::Snapshot(snapshot)).await;
            let _ = send_msg(sink, &applied(command.seq)).await;
        }
        // Seat bookkeeping is this module's job (D14), so it never travels to
        // the TUI and back.
        names::RELEASE_SEAT => {
            shared.registry().release(viewer_id);
            let _ = send_msg(sink, &applied(command.seq)).await;
            shared.announce_seats();
        }
        names::SELECT_PROJECT
        | names::SELECT_SESSION
        | names::SELECT_TERMINAL
        | names::MARK_ACTIVITY_READ => {
            if seat != Seat::Controlling {
                // Read-only means read-only. D3 makes the selection shared with
                // the desktop, so letting an observer move it would be input by
                // another name.
                let _ = send_msg(
                    sink,
                    &ServerMsg::Error(WireError {
                        seq: Some(command.seq),
                        ..WireError::new(
                            ErrorCode::ReadOnly,
                            "this tab is watching read-only; take over to drive",
                        )
                    }),
                )
                .await;
                return;
            }
            // The TUI applies it and acks (`WebOutbound::Viewer`), for the same
            // reason input is not acked here.
            shared.notify(WebInbound::Command {
                viewer_id: viewer_id.clone(),
                command,
            });
        }
        _ => {
            // The M2 door's failure mode: a newer browser asking for a command
            // this build does not implement is told no, clearly, and keeps its
            // socket.
            let _ = send_msg(
                sink,
                &ServerMsg::Error(WireError {
                    seq: Some(command.seq),
                    ..WireError::new(
                        ErrorCode::NotSupported,
                        format!("this FlightDeck does not implement `{}`", command.name),
                    )
                }),
            )
            .await;
        }
    }
}

fn applied(seq: u64) -> ServerMsg {
    ServerMsg::Ack(Ack {
        seq,
        outcome: AckOutcome::Applied,
        detail: None,
    })
}

#[cfg(test)]
mod tests;
