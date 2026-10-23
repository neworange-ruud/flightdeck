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
//! ## Worker hosts (v7, `specs/WORKER_PRD.md`)
//!
//! [`start_with`] runs the same server for a `flightdeck worker`, and three
//! things change when [`ServerOptions::host_kind`] says so:
//!
//! * **Where it listens.** [`ListenPlan::Tailscale`] keeps one listener per
//!   Tailscale address, re-read on a poll and reconciled — never loopback,
//!   never the unspecified address — and reports itself through
//!   [`WebServerHandle::listen_status`]. No SPA is served.
//! * **Who may connect.** Clients older than
//!   [`WORKER_MIN_CLIENT_VERSION`] are refused with the worker's own floor.
//! * **The seat model.** One **window** drives; a new window takes over the
//!   old one ([`ServerMsg::TakenOver`]), or silently replaces its own stale
//!   socket. Any number of **background links** watch status and nothing else
//!   (`background_may_receive`), never take over and are never taken over,
//!   and every connection is told who holds the window
//!   ([`Delta::Connections`]). TUI and Desktop hosts keep D14's multi-writer
//!   model unchanged.
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

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
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
use tokio::task::JoinHandle;

use crate::contracts::domain::WebConfig;
use crate::contracts::traits::Clock;
use crate::contracts::PtySize;
use crate::remote::debuglog;
use crate::remote::direct::net::{
    advertised_host, select_bind_addresses, AddressOrigin, AddressSource, TailscaleProbe,
    TailscaleStatus,
};
use crate::remote::runtime;
use crate::web::arbiter::{Claim, InputArbiter, SharedInputLock, Writer};
use crate::web::assets::{self, Lookup};
use crate::web::credentials::{
    AccessScreen, AuthFailure, CredentialStore, TokenId, BOOTSTRAP_CODE_TTL_MS,
    RATE_LIMIT_LOCKOUT_MS,
};
use crate::web::protocol::{
    check_version, negotiated, Ack, AckOutcome, ActivityEvent, Attach, ClientMsg, ConnectionKind,
    ConnectionsView, Delta, DialogView, ErrorCode, Geometry, Health, HostKind, HostMessage,
    ProjectView, Seat, SeatInfo, SeatRequest, Selection, ServerMsg, ShutdownReason, Snapshot,
    TermCursor, TerminalId, VersionMismatch, ViewerId, Viewport, WindowInfo, WireError, WorkerOp,
    WorkerShellView, WorkerSnapshot, WorkerStatusView, MAX_SUPPORTED_VERSION, PROTOCOL_VERSION,
    WORKER_MIN_CLIENT_VERSION,
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
/// revokes or rotates its token. That holds for a *live* socket as well as a
/// new one, and the two are enforced separately:
///
/// * a new connection is refused by [`CredentialStore::verify_token`], which
///   `ws_route` consults before the upgrade; and
/// * an attached socket is refused by [`CredentialStore::is_token_active`],
///   which [`Shared::credential_is_active`] consults on **every frame it
///   sends**, and is closed with [`ShutdownReason::TokenRevoked`] the moment
///   [`WebServerHandle::recheck_credentials`] tells it to look.
///
/// The second half is the one that did not exist until §6.5 R20
/// (`remote-control-glmt`): "consulted on every connection" was true, and was
/// not enough, because an already-connected browser never makes another one.
/// A short `Max-Age` would only mean the user re-enters a code for no security
/// gain.
pub const COOKIE_MAX_AGE_SECS: u64 = 400 * 24 * 60 * 60;

/// Where the browser POSTs its bootstrap code (Q4).
pub const AUTH_EXCHANGE_PATH: &str = "/auth/exchange";

/// A cheap "does my cookie still work?" probe, so the SPA can decide between
/// the app and the code-entry screen without opening a WebSocket it is about to
/// be refused.
pub const AUTH_SESSION_PATH: &str = "/auth/session";

/// The WebSocket endpoint the web protocol rides on (D12).
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

/// How often a [`ListenPlan::Tailscale`] server asks Tailscale itself about
/// this node even when the interface addresses have not moved.
///
/// The probe spawns the `tailscale` CLI (up to three seconds), so it is not
/// run on every poll: the interface list is the cheap signal, and this is the
/// backstop for a change the interface list cannot show — a node that logged
/// out keeps its `100.x` address on some platforms (design §2.2).
const TAILSCALE_REPROBE: Duration = Duration::from_secs(60);

/// The reason a Tailscale plan reports when there is nothing it may bind.
pub const NO_TAILSCALE_ADDRESS: &str = "no Tailscale address";

/// The label the desktop's own seat row carries, and the name it is refused
/// under when another writer holds the input lock (2f).
pub const DESKTOP_SEAT_LABEL: &str = "desktop";

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
    /// SPECS §23's help screen, as this build and this config have it
    /// (`remote-control-ll5.8`, §6.5 R16).
    ///
    /// It lives here rather than being computed in [`Shared::snapshot_for`]
    /// because two of its facts are the TUI's — `[ui]
    /// use_f2_to_leave_terminal_focus` and SPECS §32's `--isolated` — and the
    /// event loop is the only thread that holds them. Published rather than
    /// pushed: it changes at most once per config save, so no [`Delta`]
    /// describes it and a browser picks the current one up on its next
    /// snapshot.
    pub help: crate::tui::help::HelpDoc,
    /// The About screen's content. Constant for the build; carried here so
    /// [`Shared::snapshot_for`] has one place to read both overlays from.
    pub about: crate::tui::help::AboutDoc,
    /// SPECS §30's update notice, mirrored straight from the desktop's own
    /// `AppState::update_available` (`remote-control-gk94`, §6.5 R25).
    ///
    /// Published rather than pushed, like [`HostState::help`]: the check runs
    /// once at startup and the answer never moves again, so there is no change
    /// for a [`crate::web::protocol::Delta`] to describe. `None` is "this host
    /// has no notice", not "you are up to date" — see
    /// [`crate::web::protocol::Snapshot::update`] for the four ways to get one.
    pub update: Option<crate::web::protocol::UpdateNotice>,
    /// `[ui] agent_tab_position`, so the browser lays the body row out the way
    /// the desktop does (`remote-control-ecsv`, §6.5 R24).
    ///
    /// Published rather than pushed, like [`HostState::help`] and for the same
    /// reason — it is a `[ui]` setting the event loop holds, it changes at most
    /// once per config save, and the browser picks the current one up on its
    /// next snapshot.
    pub sidebar_position: crate::contracts::AgentTabPosition,
    /// What kind of FlightDeck publishes this state, stamped on every
    /// snapshot as [`Snapshot::host_kind`] (v7). [`HostKind::Tui`] unless the
    /// host says otherwise.
    pub host_kind: HostKind,
    /// The worker's own state (v7). `Some` only on a worker host; the server
    /// turns it into each recipient's [`Snapshot::worker`].
    pub worker: Option<WorkerHostState>,
}

/// The worker-only part of [`HostState`] (v7, `specs/WORKER_PRD.md`): what a
/// [`crate::web::protocol::WorkerSnapshot`] is built from, minus the
/// per-recipient [`crate::web::protocol::ConnectionsView`], which only the
/// server knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerHostState {
    /// `$HOME` on the worker.
    pub home: String,
    /// The worker shell, when one is running or has run.
    pub shell: Option<WorkerShellView>,
    /// The latest health sample.
    pub health: Option<Health>,
    /// Listen addresses, unread count, projects root.
    pub status: WorkerStatusView,
    /// The host messages nobody has seen yet, oldest first.
    pub host_messages: Vec<HostMessage>,
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
            // The defaults a server that has never heard from the event loop
            // serves: a real help screen for a build with no config loaded is
            // still this build's keybindings, which is better than an empty
            // panel and is not a guess — `use_f2` and `isolated` both default
            // to off.
            help: crate::tui::help::help_doc(false, false),
            about: crate::tui::help::about_doc(),
            // A server that has never heard from the event loop has heard
            // nothing about a release either, and says so.
            update: None,
            // A server that has not heard from the event loop has not been
            // told the user's `[ui]` either, and `left` is the default that
            // setting itself has.
            sidebar_position: crate::contracts::AgentTabPosition::default(),
            // Every host before v7 was a TUI or a Desktop, and neither has a
            // worker's state to publish.
            host_kind: HostKind::Tui,
            worker: None,
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
    /// To every viewer seated as a **writer**, and to no observer.
    ///
    /// Several writers is the normal case under D14 as revised, so this is a
    /// fan-out rather than a single recipient. It deliberately does *not* mean
    /// "whoever holds the input lock": the lock moves on every hand-off, and a
    /// frame addressed to a moving target would land on whoever happened to be
    /// typing when it was built.
    Writers(ServerMsg),
    /// To every **window** connection, and to no background link (v7). On a
    /// TUI or Desktop host every viewer is a window, so this is
    /// [`WebOutbound::All`] there.
    Windows(ServerMsg),
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
        /// The [`ViewerId`] of the connection this one is resuming
        /// ([`Attach::resume_viewer`]), when the browser named one.
        ///
        /// Forwarded rather than kept here because this module remembers what it
        /// *forwarded* and `src/web/stream.rs` remembers what a PTY actually
        /// *took* — and only the latter can safely dedup a replayed keystroke
        /// queue (§5.1). Without this field the applier's watermark would reset
        /// to zero on every reconnect and a browser replaying its held queue
        /// would have every keystroke typed twice.
        resume_viewer: Option<ViewerId>,
        /// A window or a background link (v7, [`Attach::connection`]). A
        /// background link is not replayed terminal bytes.
        kind: ConnectionKind,
        /// The version this viewer was answered at
        /// ([`crate::web::protocol::negotiated`]).
        protocol_version: u16,
    },
    /// A browser's socket closed.
    ViewerDetached {
        /// The viewer that went away.
        viewer_id: ViewerId,
    },
    /// The seat map changed — someone attached, left, changed role, or the
    /// input lock moved. Carries the same rows the viewers were just told about,
    /// so the desktop's viewer chip renders the same facts as the browser's
    /// without asking a second source.
    SeatsChanged {
        /// Everyone attached, desktop row first.
        seats: Vec<SeatInfo>,
    },
    /// Keystrokes from a writer **that held the input lock when they arrived**.
    ///
    /// Two kinds of input never reach here, and neither vanishes unremarked: an
    /// observer's is answered [`AckOutcome::Ignored`], and a writer's typed into
    /// somebody else's live burst is answered [`AckOutcome::Rejected`] plus
    /// [`ErrorCode::SeatHeld`]. Arbitrating *before* the channel is what makes
    /// this queue safe: everything in it is from one holder until the lock
    /// moves, so draining it in order cannot splice two writers together.
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
    /// A named command from a seated writer (the M2 door, D13). The
    /// server answers `release_seat` and `request_snapshot` itself and refuses
    /// unknown names with [`ErrorCode::NotSupported`], so only M1's remaining
    /// commands arrive here.
    Command {
        /// Who asked.
        viewer_id: ViewerId,
        /// The seat's chip label (`192.168.2.20 · Chrome on macOS`), so the host
        /// can tag a dialog this command opens with D13's origin without having
        /// to keep its own copy of the seat map.
        label: String,
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
    /// A window asked to size one terminal's PTY (v7, PRD W5). Only ever sent
    /// by a **worker** host, and only for its window; everywhere else the
    /// frame is dropped and D4 holds.
    ResizeTerminal {
        /// Who asked.
        viewer_id: ViewerId,
        /// The terminal to resize — an agent terminal or the worker shell.
        terminal_id: TerminalId,
        /// The requested grid.
        size: PtySize,
    },
    /// A parsed [`crate::web::protocol::worker_command`] from a worker's
    /// window (v7). The host applies it and sends the [`Ack`] for `seq`.
    WorkerOp {
        /// Who asked.
        viewer_id: ViewerId,
        /// The [`crate::web::protocol::Command::seq`] to ack.
        seq: u64,
        /// What they asked for.
        op: WorkerOp,
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
///
/// Derived in exactly one place — from the address the listener really got, in
/// [`start`] — and never from the configured `[web] bind` string. The string
/// can lie (`0.0.0.0` is not an address anything is reachable at, `localhost`
/// is whatever the resolver says today) and the socket cannot, so there is one
/// classifier rather than a pre-bind guess beside a post-bind fact. A second,
/// string-based one existed and nothing called it — `specs/WEB_INTERFACE.md`
/// §6.5 R26.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindExposure {
    /// `127.0.0.1` / `::1` — this machine only. The default.
    Loopback,
    /// Anything else. Only ever reached because the user typed a non-loopback
    /// `[web] bind` themselves, and the UI warns when the server actually
    /// starts.
    Routable,
}

/// How [`start_with`] runs a server: for which kind of host, on which
/// addresses, and whether it serves the browser SPA (v7).
#[derive(Clone, Debug)]
pub struct ServerOptions {
    /// What kind of FlightDeck this server belongs to. A worker gets the
    /// worker seat model (one window, takeover, background links) and refuses
    /// clients older than [`crate::web::protocol::WORKER_MIN_CLIENT_VERSION`].
    pub host_kind: HostKind,
    /// Where to listen.
    pub listen: ListenPlan,
    /// Whether the SPA's assets are served. A worker serves only the protocol
    /// routes, and every other path is a 404.
    pub serve_assets: bool,
}

/// Where a server listens (v7).
#[derive(Clone)]
pub enum ListenPlan {
    /// `[web] bind:port`: today's single listener (TUI/Desktop). [`start`]
    /// wraps this.
    Configured {
        /// The configured bind address, as `config.toml` spells it.
        bind: String,
        /// The configured port; `0` lets the OS choose.
        port: u16,
    },
    /// Worker: only this machine's Tailscale addresses, re-read every `poll`
    /// and rebound when they change (PRD W8, W27). Never loopback or the
    /// unspecified address.
    Tailscale {
        /// The port every Tailscale address is bound on.
        port: u16,
        /// This machine's interface addresses.
        source: Arc<dyn AddressSource>,
        /// What Tailscale says about this node.
        probe: Arc<dyn TailscaleProbe>,
        /// How often the addresses are re-read.
        poll: Duration,
    },
    /// Exactly these addresses (`127.0.0.1:0` allowed) — the seam tests and
    /// debugging use.
    Fixed(Vec<SocketAddr>),
}

impl std::fmt::Debug for ListenPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ListenPlan::Configured { bind, port } => f
                .debug_struct("Configured")
                .field("bind", bind)
                .field("port", port)
                .finish(),
            ListenPlan::Tailscale { port, poll, .. } => f
                .debug_struct("Tailscale")
                .field("port", port)
                .field("poll", poll)
                .finish_non_exhaustive(),
            ListenPlan::Fixed(addrs) => f.debug_tuple("Fixed").field(addrs).finish(),
        }
    }
}

/// Where a server is listening right now, and why not when it is not (v7,
/// PRD W27). What `flightdeck worker status` reports.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListenStatus {
    /// Every address a listener is bound on.
    pub bound: Vec<SocketAddr>,
    /// The `host:port` a client should dial, when a name is known
    /// (Tailscale's MagicDNS).
    pub advertised: Option<String>,
    /// Why nothing is bound, when nothing is.
    pub reason: Option<String>,
    /// Where the Tailscale addresses came from, on a Tailscale plan.
    pub origin: Option<AddressOrigin>,
}

/// One attached connection, as `flightdeck worker status` and the worker's
/// connection bookkeeping see it (v7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionInfo {
    /// The connection's viewer id.
    pub viewer_id: ViewerId,
    /// The credential it authenticated with.
    pub token: TokenId,
    /// A window or a background link.
    pub kind: ConnectionKind,
    /// Its chip label.
    pub label: String,
    /// The address the host observed on its socket.
    pub address: IpAddr,
    /// Host wall-clock (unix ms) it attached.
    pub since_ms: i64,
}

// ===========================================================================
// The handle
// ===========================================================================

/// The TUI's end of the server. Dropping it stops the server.
pub struct WebServerHandle {
    shared: Arc<Shared>,
    /// The port the plan asked for, so [`WebServerHandle::bound_addr`] has
    /// something honest to say while a Tailscale plan has nothing bound.
    port: u16,
    /// Where the listeners are right now. Written by the task that owns them
    /// — once for `Configured` and `Fixed`, on every poll for `Tailscale`.
    listen: watch::Receiver<ListenStatus>,
    exposure: BindExposure,
    /// `None` once [`WebServerHandle::stop`] has taken it.
    shutdown: Option<watch::Sender<Option<ShutdownNotice>>>,
    /// The desktop's end of [`Shared::revocations`]. Held for the life of the
    /// handle: unlike `shutdown`, sending on it is not a terminal event.
    revocations: watch::Sender<u64>,
    /// Reports `Disconnected` when the server task has finished, which is how
    /// `stop` waits for the listener to be released without needing a runtime
    /// context.
    done: Option<std::sync::mpsc::Receiver<()>>,
}

impl WebServerHandle {
    /// The address actually bound. With `[web] port = 0` (how the tests avoid
    /// colliding) this is where the OS put the listener, so the caller can print
    /// or QR-encode a URL that works.
    ///
    /// With several listeners this is the first of [`Self::bound_addrs`]; with
    /// none (a Tailscale plan on a node with no Tailscale address) it is the
    /// unspecified address on the planned port, which nothing can dial.
    pub fn bound_addr(&self) -> SocketAddr {
        self.listen
            .borrow()
            .bound
            .first()
            .copied()
            .unwrap_or_else(|| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), self.port))
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
        let holder = self.shared.holder();
        self.shared.registry().seat_rows(None, holder.as_ref())
    }

    /// The input lock, so the **desktop** can claim a turn before it writes to a
    /// PTY (D14 as revised).
    ///
    /// Handed out rather than wrapped in a `claim_for_desktop` helper because
    /// the desktop is one writer among several and gets no separate door: it
    /// calls [`crate::web::arbiter::InputArbiter::claim`], with the same
    /// arguments and the same possible refusal as a browser's socket does.
    pub fn input_lock(&self) -> SharedInputLock {
        Arc::clone(&self.shared.input_lock)
    }

    /// Expire a holder that has gone quiet and announce the lock if it moved.
    ///
    /// Called once per render tick. Every other lock movement is announced by
    /// whoever caused it, but *nobody* causes an expiry — it is the passage of
    /// time — so without this the chip would keep naming somebody who stopped
    /// typing a minute ago. Comparing against
    /// [`Shared::announced_holder`] is what keeps a per-tick call from
    /// producing a per-tick fan-out.
    pub fn sync_input_lock(&self, now_ms: i64) {
        let moved = {
            let mut lock = self.shared.input_lock();
            lock.expire(now_ms);
            let holder = lock.holder().cloned();
            let mut announced = self.shared.announced_holder.unwrap_or_recover();
            let moved = *announced != holder;
            if moved {
                *announced = holder;
            }
            moved
        };
        if moved {
            // The lock moved because time passed, not because anybody decided
            // anything: `None`, so no browser is shown 2f's evicted panel for
            // the most ordinary movement there is.
            self.shared.announce_seats(None);
        }
    }

    /// The desktop's explicit override: take the input lock now, interrupting
    /// whoever holds it.
    ///
    /// The mirror of a browser's `Attach { seat: TakeOver }`, and reachable the
    /// same way — only from an affordance a human chose (`Take Input Lock` in
    /// the palette). Returns whom it interrupted, so the caller can say so.
    pub fn preempt_input_for_desktop(&self, now_ms: i64) -> Option<String> {
        // Read before the preempt, because after it the holder is us: whom we
        // interrupted is a fact that exists for exactly one statement.
        let interrupted = {
            let mut lock = self.shared.input_lock();
            // Preempting a free lock, or our own, interrupts nobody — and must
            // not, or the palette command would announce an eviction to a
            // browser that never held anything.
            let interrupted = match lock.holder() {
                Some(Writer::Desktop) | None => None,
                Some(who @ Writer::Viewer(_)) => {
                    Some((who.clone(), lock.holder_label().map(str::to_string)))
                }
            };
            lock.preempt(&Writer::Desktop, DESKTOP_SEAT_LABEL, now_ms);
            interrupted
        };
        let (who, label) = match interrupted {
            Some((who, label)) => (Some(who), label),
            None => (None, None),
        };
        self.shared.announce_seats(who.as_ref());
        label
    }

    /// Who holds the input lock, as a label the desktop can render, or `None`
    /// when it is free.
    ///
    /// Reading, never claiming: the status bar asks this every frame, and a
    /// draw that took the lock would hand it to whoever repainted last.
    pub fn input_holder_label(&self) -> Option<String> {
        self.shared.input_lock().holder_label().map(str::to_string)
    }

    /// Whether any *browser* is seated as a writer — i.e. whether the desktop
    /// has anybody to contend with at all.
    ///
    /// The desktop's own row is always a writer, so it is deliberately not
    /// counted here: with no browser writing, naming a lock holder on the
    /// desktop's status bar would be permanent chrome about a contest that
    /// cannot happen.
    pub fn has_browser_writer(&self) -> bool {
        !self.shared.registry().writers().is_empty()
    }

    /// How many browsers are attached (observers included) — **windows**
    /// only. A worker's background links watch for status and notifications,
    /// and must not make the host believe somebody is looking at its
    /// terminals (this count drives the background git refresh).
    pub fn viewer_count(&self) -> usize {
        self.window_count()
    }

    /// Where the server is listening, and why not when it is not (v7).
    pub fn listen_status(&self) -> ListenStatus {
        self.listen.borrow().clone()
    }

    /// Every address a listener is bound on (v7). [`Self::bound_addr`] is the
    /// first of them.
    pub fn bound_addrs(&self) -> Vec<SocketAddr> {
        self.listen.borrow().bound.clone()
    }

    /// How many **window** connections are attached (v7). On a TUI or
    /// Desktop host every viewer is a window.
    pub fn window_count(&self) -> usize {
        self.shared.registry().count(ConnectionKind::Window)
    }

    /// How many **background links** are attached (v7). Always zero on a TUI
    /// or Desktop host.
    pub fn background_count(&self) -> usize {
        self.shared.registry().count(ConnectionKind::Background)
    }

    /// Every attached connection (v7), in attach order.
    pub fn connections(&self) -> Vec<ConnectionInfo> {
        self.shared
            .registry()
            .viewers
            .iter()
            .map(|viewer| ConnectionInfo {
                viewer_id: viewer.id.clone(),
                token: viewer.identity.token.clone(),
                kind: viewer.kind,
                label: viewer.identity.label(),
                address: viewer.identity.address,
                since_ms: viewer.since_ms,
            })
            .collect()
    }

    /// Have every attached socket re-check its own credential, closing the ones
    /// whose credential has been withdrawn.
    ///
    /// This is the second half of a revocation, and the half that makes the
    /// first half mean anything. The desktop's `x` (artboard 2a State B) writes
    /// the revocation into the [`CredentialStore`]; a browser that is *already*
    /// connected never asks that store another question of its own accord, so
    /// without this call it keeps full read/write control of every terminal for
    /// as long as it stays connected — `remote-control-glmt`, §6.5 R20.
    ///
    /// Takes no argument, and that is the design rather than an omission. Each
    /// socket asks about the one token it holds, so a revocation that named one
    /// browser closes that browser's sockets and leaves everyone else attached,
    /// with no set to build here and no chance of this side and the store
    /// disagreeing about who was revoked. `WebAccess`'s `x` happens to revoke
    /// everybody (`CredentialStore::rotate` is `revoke_all`); when
    /// `remote-control-gk94` makes it per-browser, nothing here changes.
    ///
    /// Cheap and safe to call when nothing was revoked: every socket looks, and
    /// every socket that is still authorised carries on.
    pub fn recheck_credentials(&self) {
        // `send_modify` marks the value changed even if the counter wrapped to
        // the same observed state, which `send` would not guarantee. A failed
        // send would mean no sockets are listening — nothing to evict.
        self.revocations
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
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
            .field("bound", &self.bound_addrs())
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
/// next connection — and, once the TUI calls
/// [`WebServerHandle::recheck_credentials`], on every socket already open.
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
    start_with(
        ServerOptions {
            host_kind: HostKind::Tui,
            listen: ListenPlan::Configured {
                bind: config.bind.clone(),
                port: config.port,
            },
            serve_assets: true,
        },
        config.replay_bytes,
        credentials,
        clock,
        initial_state,
        inbound,
    )
}

/// Start the server on the shared runtime with explicit [`ServerOptions`]
/// (v7): the worker's entry point, and what [`start`] wraps.
///
/// `replay_bytes` is `[web] replay_bytes`, the per-terminal ring capacity.
/// The server itself keeps no ring — `src/web/stream.rs` does, sized by the
/// host from the same setting — so it is accepted here only so both entry
/// points take the same facts.
///
/// What each [`ListenPlan`] does before this returns:
///
/// * **`Configured`** — exactly what [`start`] always did: one listener on
///   `bind:port`, and a [`StartError::Bind`] if it cannot be had.
/// * **`Fixed`** — one listener per address; the first that cannot be bound
///   fails the whole start, and an empty list is refused.
/// * **`Tailscale`** — one reconcile pass (design §2.2), then `Ok` **whether
///   or not anything was bound**: a worker on a node whose Tailscale is down
///   keeps running and keeps polling, and [`WebServerHandle::listen_status`]
///   says why it is not listening.
pub fn start_with(
    options: ServerOptions,
    replay_bytes: usize,
    credentials: Arc<Mutex<CredentialStore>>,
    clock: Arc<dyn Clock + Send + Sync>,
    initial_state: HostState,
    inbound: std::sync::mpsc::Sender<WebInbound>,
) -> Result<WebServerHandle, StartError> {
    let _ = replay_bytes;
    let ServerOptions {
        host_kind,
        listen: plan,
        serve_assets,
    } = options;
    // Refused before a runtime is touched: there is nothing to try.
    if let ListenPlan::Fixed(addrs) = &plan {
        if addrs.is_empty() {
            return Err(StartError::Bind {
                address: String::new(),
                reason: "no address to listen on".to_string(),
            });
        }
    }
    let port = match &plan {
        ListenPlan::Configured { port, .. } | ListenPlan::Tailscale { port, .. } => *port,
        ListenPlan::Fixed(addrs) => addrs.first().map_or(0, SocketAddr::port),
    };

    let Some(rt) = runtime::try_shared() else {
        debuglog::log("web START skipped — no async runtime");
        return Err(StartError::NoRuntime);
    };
    let (shutdown_tx, shutdown_rx) = watch::channel::<Option<ShutdownNotice>>(None);
    let (revocations_tx, revocations_rx) = watch::channel::<u64>(0);
    let (state_tx, state_rx) = watch::channel(Arc::new(initial_state));
    let (listen_tx, listen_rx) = watch::channel(ListenStatus::default());
    let started_ms = clock.now_millis() as i64;

    let shared = Arc::new(Shared {
        credentials,
        clock,
        inbound: Mutex::new(inbound),
        state: state_tx,
        state_rx,
        registry: Mutex::new(SeatRegistry::new(started_ms, host_kind)),
        input_lock: InputArbiter::shared(),
        announced_holder: Mutex::new(None),
        shutdown: shutdown_rx,
        revocations: revocations_rx,
        drain: Arc::new(Drain::default()),
        host_kind,
        serve_assets,
    });

    // Rendezvous: the task reports the bind result, then serves. Blocking the
    // caller here is bounded by the binds (plus, for a Tailscale plan, one
    // bounded probe) and is what lets the palette command say "listening on
    // …" or "could not bind" synchronously.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), StartError>>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();

    let task_shared = Arc::clone(&shared);
    rt.spawn(async move {
        // Owned by the task: dropped when it returns, which is what unblocks
        // `stop`. Never sent on — its drop is the signal.
        let _done = done_tx;
        let router = router(Arc::clone(&task_shared));
        match plan {
            ListenPlan::Configured { bind, port } => {
                serve_configured(task_shared, router, &bind, port, listen_tx, ready_tx).await;
            }
            ListenPlan::Fixed(addrs) => {
                serve_fixed(task_shared, router, &addrs, listen_tx, ready_tx).await;
            }
            ListenPlan::Tailscale {
                port,
                source,
                probe,
                poll,
            } => {
                let plan = TailscalePlan {
                    port,
                    source,
                    probe,
                    poll,
                };
                supervise_tailscale(task_shared, router, plan, listen_tx, ready_tx).await;
            }
        }
    });

    match ready_rx.recv() {
        Ok(Ok(())) => {
            let status = listen_rx.borrow().clone();
            let exposure = if !status.bound.is_empty()
                && status.bound.iter().all(|addr| addr.ip().is_loopback())
            {
                BindExposure::Loopback
            } else {
                BindExposure::Routable
            };
            debuglog::log(&format!(
                "web START kind={host_kind:?} bound={:?} exposure={}",
                status.bound,
                match exposure {
                    BindExposure::Loopback => "loopback",
                    BindExposure::Routable => "routable",
                }
            ));
            Ok(WebServerHandle {
                shared,
                port,
                listen: listen_rx,
                exposure,
                shutdown: Some(shutdown_tx),
                revocations: revocations_tx,
                done: Some(done_rx),
            })
        }
        Ok(Err(error)) => {
            if let StartError::Bind { address, .. } = &error {
                debuglog::log(&format!("web START failed address={address}"));
            }
            Err(error)
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

/// The rendezvous [`start_with`] blocks on.
type Ready = std::sync::mpsc::Sender<Result<(), StartError>>;

/// `[web] bind:port`, exactly as [`start`] always bound it: resolved by tokio
/// (so `localhost` works), one listener.
async fn serve_configured(
    shared: Arc<Shared>,
    router: Router,
    bind: &str,
    port: u16,
    listen: watch::Sender<ListenStatus>,
    ready: Ready,
) {
    let address = listen_address(bind, port);
    let listener = match tokio::net::TcpListener::bind(address.as_str()).await {
        Ok(listener) => listener,
        Err(e) => {
            let _ = ready.send(Err(StartError::Bind {
                address,
                reason: e.to_string(),
            }));
            return;
        }
    };
    let bound = match listener.local_addr() {
        Ok(addr) => addr,
        Err(e) => {
            let _ = ready.send(Err(StartError::Bind {
                address,
                reason: e.to_string(),
            }));
            return;
        }
    };
    listen.send_replace(ListenStatus {
        bound: vec![bound],
        ..ListenStatus::default()
    });
    if ready.send(Ok(())).is_err() {
        // Nobody is waiting for this server; do not hold the port.
        return;
    }
    serve(shared, router, listener).await;
}

/// Exactly these addresses, all or nothing.
async fn serve_fixed(
    shared: Arc<Shared>,
    router: Router,
    addrs: &[SocketAddr],
    listen: watch::Sender<ListenStatus>,
    ready: Ready,
) {
    let mut listeners = Vec::with_capacity(addrs.len());
    let mut bound = Vec::with_capacity(addrs.len());
    for addr in addrs {
        let listener = bind_std(*addr)
            .and_then(tokio::net::TcpListener::from_std)
            .and_then(|l| l.local_addr().map(|local| (l, local)));
        match listener {
            Ok((listener, local)) => {
                bound.push(local);
                listeners.push(listener);
            }
            Err(e) => {
                // Dropping the listeners bound so far releases them: a plan
                // that only half binds is not the plan that was asked for.
                let _ = ready.send(Err(StartError::Bind {
                    address: addr.to_string(),
                    reason: e.to_string(),
                }));
                return;
            }
        }
    }
    listen.send_replace(ListenStatus {
        bound,
        ..ListenStatus::default()
    });
    if ready.send(Ok(())).is_err() {
        return;
    }
    let tasks: Vec<JoinHandle<()>> = listeners
        .into_iter()
        .map(|listener| tokio::spawn(serve(Arc::clone(&shared), router.clone(), listener)))
        .collect();
    for task in tasks {
        let _ = task.await;
    }
}

/// A [`ListenPlan::Tailscale`], unpacked for the task that runs it.
struct TailscalePlan {
    port: u16,
    source: Arc<dyn AddressSource>,
    probe: Arc<dyn TailscaleProbe>,
    poll: Duration,
}

/// Keep one listener per Tailscale address until shutdown (design §2.2).
///
/// Every `poll` the interface addresses are read (cheap); Tailscale itself is
/// asked (the CLI, off the async workers) only when that set changed or
/// [`TAILSCALE_REPROBE`] has passed. The listeners are then [`reconcile`]d
/// against what is wanted and the [`ListenStatus`] republished if it moved —
/// the same shape as `remote::direct::server`'s supervisor, sharing one
/// [`Shared`] (registry, input lock, credentials) across every listener.
async fn supervise_tailscale(
    shared: Arc<Shared>,
    router: Router,
    plan: TailscalePlan,
    listen: watch::Sender<ListenStatus>,
    ready: Ready,
) {
    let mut ready = Some(ready);
    let mut shutdown = shared.shutdown.clone();
    let mut listeners: HashMap<IpAddr, ListenerTask> = HashMap::new();
    let mut seen: Option<Vec<IpAddr>> = None;
    let mut probed: Option<(tokio::time::Instant, Option<TailscaleStatus>)> = None;
    loop {
        let source = Arc::clone(&plan.source);
        let mut all = tokio::task::spawn_blocking(move || source.addresses())
            .await
            .unwrap_or_default();
        all.sort_unstable();
        all.dedup();
        let stale = probed
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() >= TAILSCALE_REPROBE);
        if seen.as_ref() != Some(&all) || stale {
            let probe = Arc::clone(&plan.probe);
            let status = tokio::task::spawn_blocking(move || probe.status())
                .await
                .unwrap_or(None);
            probed = Some((tokio::time::Instant::now(), status));
            seen = Some(all.clone());
        }
        let status = probed.as_ref().and_then(|(_, status)| status.as_ref());
        let (wanted, origin) = select_bind_addresses(&all, status.map(|s| s.ips.as_slice()));

        let errors = reconcile(&mut listeners, &wanted, plan.port, &mut |addr| {
            let listener = bind_std(addr).and_then(tokio::net::TcpListener::from_std)?;
            debuglog::log(&format!("web LISTEN {addr}"));
            Ok(ListenerTask(tokio::spawn(serve(
                Arc::clone(&shared),
                router.clone(),
                listener,
            ))))
        });
        for (ip, error) in &errors {
            debuglog::log(&format!("web LISTEN failed {ip}:{} err={error}", plan.port));
        }
        let bound: Vec<IpAddr> = listeners.keys().copied().collect();
        let next = tailscale_listen_status(
            plan.port,
            &wanted,
            &bound,
            &errors,
            status.and_then(|s| s.magic_dns.as_deref()),
            origin,
        );
        listen.send_if_modified(|current| {
            let moved = *current != next;
            if moved {
                *current = next;
            }
            moved
        });
        if let Some(ready) = ready.take() {
            if ready.send(Ok(())).is_err() {
                // Nobody is waiting for this server; do not hold the ports.
                break;
            }
        }
        tokio::select! {
            _ = await_shutdown(&mut shutdown) => break,
            _ = tokio::time::sleep(plan.poll) => {}
        }
    }
    // Every listener watches the same shutdown signal and drains on its own;
    // waiting for them is what keeps `stop` honest about the ports.
    for (_, ListenerTask(task)) in listeners {
        let _ = task.await;
    }
}

/// One Tailscale address's listener: the task serving it.
struct ListenerTask(JoinHandle<()>);

/// Something [`reconcile`] can retire when its address goes away.
trait Retire {
    /// Stop serving. Called once, when the address is no longer wanted.
    fn retire(self);
}

impl Retire for ListenerTask {
    fn retire(self) {
        // The connections already accepted are their own tasks and finish on
        // their own; only the accept loop — and with it the port — goes.
        self.0.abort();
    }
}

/// Whether an address may be listened on by a Tailscale plan at all.
///
/// Defence in depth on top of [`select_bind_addresses`]: whatever Tailscale or
/// the range check says, a worker never listens on loopback (any local process
/// could reach it) or on the unspecified address (every interface, the LAN
/// included). PRD W8.
fn tailscale_bindable(ip: IpAddr) -> bool {
    !ip.is_loopback() && !ip.is_unspecified()
}

/// Bring `current` in line with `wanted`: retire the listeners whose address
/// went away, bind the ones that appeared, keep the rest untouched (their
/// connections stay up). Loopback and the unspecified address are never bound,
/// whatever `wanted` says.
///
/// Returns the addresses that could not be bound, with the OS's reason; they
/// are tried again on the next pass.
fn reconcile<T: Retire>(
    current: &mut HashMap<IpAddr, T>,
    wanted: &[IpAddr],
    port: u16,
    bind: &mut dyn FnMut(SocketAddr) -> std::io::Result<T>,
) -> Vec<(IpAddr, String)> {
    let wanted: Vec<IpAddr> = wanted
        .iter()
        .copied()
        .filter(|ip| tailscale_bindable(*ip))
        .collect();
    let gone: Vec<IpAddr> = current
        .keys()
        .filter(|ip| !wanted.contains(ip))
        .copied()
        .collect();
    for ip in gone {
        if let Some(task) = current.remove(&ip) {
            task.retire();
        }
    }
    let mut errors = Vec::new();
    for ip in wanted {
        if current.contains_key(&ip) {
            continue;
        }
        match bind(SocketAddr::new(ip, port)) {
            Ok(task) => {
                current.insert(ip, task);
            }
            Err(e) => errors.push((ip, e.to_string())),
        }
    }
    errors
}

/// What a Tailscale plan reports after one [`reconcile`] pass.
///
/// `reason` is set only when nothing is bound: `"no Tailscale address"` when
/// there was nothing it may bind, `"bind failed: <addr>: <error>"` when every
/// bind failed. `advertised` is the MagicDNS name (or the first bound address)
/// plus the port, as a client should dial it.
fn tailscale_listen_status(
    port: u16,
    wanted: &[IpAddr],
    bound: &[IpAddr],
    errors: &[(IpAddr, String)],
    magic_dns: Option<&str>,
    origin: AddressOrigin,
) -> ListenStatus {
    let bound = sort_ipv4_first(bound);
    let reason = if !bound.is_empty() {
        None
    } else if !wanted.iter().any(|ip| tailscale_bindable(*ip)) {
        Some(NO_TAILSCALE_ADDRESS.to_string())
    } else {
        let failures: Vec<String> = errors
            .iter()
            .map(|(ip, error)| format!("{}: {error}", SocketAddr::new(*ip, port)))
            .collect();
        Some(format!("bind failed: {}", failures.join("; ")))
    };
    let advertised =
        advertised_host("", magic_dns, &bound).map(|host| match host.parse::<IpAddr>() {
            Ok(ip) => SocketAddr::new(ip, port).to_string(),
            Err(_) => format!("{host}:{port}"),
        });
    ListenStatus {
        bound: bound.iter().map(|ip| SocketAddr::new(*ip, port)).collect(),
        advertised,
        reason,
        origin: Some(origin),
    }
}

/// `ips` IPv4 first, each family ascending — the order the advert picks from
/// and `worker status` prints.
fn sort_ipv4_first(ips: &[IpAddr]) -> Vec<IpAddr> {
    let mut sorted = ips.to_vec();
    sorted.sort_unstable_by_key(|ip| (ip.is_ipv6(), *ip));
    sorted.dedup();
    sorted
}

/// A non-blocking std listener, ready for [`tokio::net::TcpListener::from_std`].
fn bind_std(addr: SocketAddr) -> std::io::Result<std::net::TcpListener> {
    let listener = std::net::TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Run axum on one listener until shutdown, then release it.
async fn serve(shared: Arc<Shared>, app: Router, listener: tokio::net::TcpListener) {
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
    let serve_assets = shared.serve_assets;
    let routes = Router::new()
        .route(WS_PATH, get(ws_route))
        .route(AUTH_EXCHANGE_PATH, post(exchange_route))
        .route(AUTH_SESSION_PATH, get(session_route));
    let routes = if serve_assets {
        // Everything else is the SPA: `/`, its hashed assets, and its
        // client-side routes (D9). `assets::lookup` owns the fallback rules.
        routes.fallback(asset_route)
    } else {
        // A worker serves the protocol routes and nothing else (design A5).
        routes.fallback(not_found_route)
    };
    routes.with_state(shared)
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
    /// Who may type right now (D14 as revised). Shared with the TUI thread,
    /// which is one of the writers — see [`crate::web::arbiter`].
    input_lock: SharedInputLock,
    /// The holder the viewers were last told about, so a move is announced
    /// exactly once. Compared against [`Shared::input_lock`] on every
    /// [`WebServerHandle::sync_input_lock`].
    announced_holder: Mutex<Option<Writer>>,
    shutdown: watch::Receiver<Option<ShutdownNotice>>,
    /// Bumped by [`WebServerHandle::recheck_credentials`]. Every live socket
    /// watches it and, when it moves, asks the credential store whether *its
    /// own* token is still active.
    ///
    /// The value is a counter and not a list of revoked ids on purpose. A list
    /// would be a second copy of a fact the store already owns, and a `watch`
    /// keeps only the latest value, so two revocations inside one tick would
    /// lose the first list. A counter cannot go stale: it says "go and look",
    /// and the place it sends you is the same authority the input path
    /// consults.
    revocations: watch::Receiver<u64>,
    drain: Arc<Drain>,
    /// What kind of FlightDeck this server belongs to. A worker gets the
    /// one-window seat model, background links, the worker commands and the
    /// v7 floor; every other host keeps the multi-writer model unchanged.
    host_kind: HostKind,
    /// Whether the SPA's assets are served (design A5).
    serve_assets: bool,
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

    /// Whether the credential a socket authenticated with still grants access.
    ///
    /// Asked of the store rather than of a flag cached beside the socket, and
    /// that is the difference between a check and a race. The desktop revokes by
    /// writing to this store; anything else the server could compare against is
    /// a copy that is made *after* the revocation lands and is therefore wrong
    /// for as long as it takes to make.
    ///
    /// Recovered rather than propagated if a poisoned lock is found, on the same
    /// terms as [`Shared::registry`] — except that the failure direction here is
    /// the other way round and deliberately so: a panicking surface must not be
    /// able to turn every browser's credential into "unknown", so recovery hands
    /// back the real state instead of a refusal.
    fn credential_is_active(&self, token: &TokenId) -> bool {
        self.credentials.unwrap_or_recover().is_token_active(token)
    }

    /// The input lock, recovered rather than propagated if a writer panicked
    /// mid-claim — the same reasoning as [`Shared::registry`]: one panicking
    /// surface must not take the terminal away from everyone else.
    fn input_lock(&self) -> std::sync::MutexGuard<'_, InputArbiter> {
        self.input_lock.unwrap_or_recover()
    }

    /// Who the seat rows should mark with [`SeatInfo::holds_input`].
    fn holder(&self) -> Option<Writer> {
        self.input_lock().holder().cloned()
    }

    /// Tell the TUI something. Dropped silently once the TUI has gone away.
    fn notify(&self, msg: WebInbound) {
        if let Ok(tx) = self.inbound.lock() {
            let _ = tx.send(msg);
        }
    }

    fn is_worker(&self) -> bool {
        self.host_kind == HostKind::Worker
    }

    /// Route one [`WebOutbound`] to its recipients. A background link only
    /// ever receives what [`background_may_receive`] lets through, whatever
    /// the address says — that filter lives in [`SeatRegistry::send_to`].
    fn dispatch(&self, out: WebOutbound) {
        let mut registry = self.registry();
        match out {
            WebOutbound::All(msg) => registry.send_all(&msg),
            // On a TUI or Desktop host every viewer is a window, so this is
            // `All` there.
            WebOutbound::Windows(msg) => {
                for id in registry.ids_of(ConnectionKind::Window) {
                    registry.send_to(&id, msg.clone());
                }
            }
            WebOutbound::Viewer { viewer_id, msg } => registry.send_to(&viewer_id, msg),
            WebOutbound::Writers(msg) => {
                for id in registry.writers() {
                    registry.send_to(&id, msg.clone());
                }
            }
        }
    }

    /// Build the snapshot a viewer gets on attach.
    ///
    /// `protocol_version` is the version this viewer was answered at
    /// ([`negotiated`]), so a v6 client is told 6.
    fn snapshot_for(
        &self,
        viewer_id: &ViewerId,
        seat: Seat,
        last_input_seq: u64,
        protocol_version: u16,
    ) -> Snapshot {
        let state = self.state_rx.borrow().clone();
        let holder = self.holder();
        let (seats, worker) = {
            let registry = self.registry();
            let seats = registry.seat_rows(Some(viewer_id), holder.as_ref());
            // The worker's own state, made this recipient's: who else is
            // connected is relative to its credential, and a background link
            // is not somewhere a host message is read.
            let worker = state.worker.as_ref().filter(|_| self.is_worker()).map(|w| {
                let recipient = registry.viewer(viewer_id);
                let background = recipient.is_some_and(|v| v.kind == ConnectionKind::Background);
                WorkerSnapshot {
                    home: w.home.clone(),
                    shell: w.shell.clone(),
                    health: w.health.clone(),
                    status: w.status.clone(),
                    connections: registry.connections_view(recipient.map(|v| &v.identity.token)),
                    host_messages: if background {
                        Vec::new()
                    } else {
                        w.host_messages.clone()
                    },
                }
            });
            (seats, worker)
        };
        // The server's own kind wins for a worker: it is what decided the
        // seat model this snapshot describes. Otherwise the host says whether
        // it is a TUI or a Desktop.
        let host_kind = if self.is_worker() {
            HostKind::Worker
        } else {
            state.host_kind
        };
        Snapshot {
            protocol_version,
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
            // Static for the life of the build, so it rides on the snapshot
            // rather than on `HostState`: there is no change for a `Delta` to
            // describe, and the browser needs it in the same frame it paints
            // the palette from.
            commands: crate::web::commands::inventory_for(host_kind),
            // Both overlays are the host's own words, sent so the browser
            // renders them rather than authoring a copy (§6.5 R16).
            help: Some(state.help.clone()),
            about: Some(state.about.clone()),
            // SPECS §30's notice, as the desktop's own status bar has it.
            update: state.update.clone(),
            // 1h position 4: the browser mirrors the body row on the same
            // setting the desktop mirrors it on, read from the same config.
            sidebar_position: state.sidebar_position,
            // Fixed for the life of the process, like `commands`.
            host_machine: Some(crate::web::protocol::HostMachine::current()),
            host_kind: Some(host_kind),
            worker,
        }
    }

    /// Fan out a [`Delta::Connections`] — each recipient's own view, since
    /// `same_device` is relative to its credential — after any attach,
    /// detach or takeover on a **worker** host. Nothing on any other host:
    /// the frame is v7-only and a v6 client must never receive one.
    fn announce_connections(&self) {
        if !self.is_worker() {
            return;
        }
        let mut registry = self.registry();
        let frames: Vec<(ViewerId, ServerMsg)> = registry
            .viewers
            .iter()
            .map(|v| {
                (
                    v.id.clone(),
                    ServerMsg::Delta(Delta::Connections(
                        registry.connections_view(Some(&v.identity.token)),
                    )),
                )
            })
            .collect();
        for (id, msg) in frames {
            registry.send_to(&id, msg);
        }
    }

    /// Fan out a [`Delta::Seats`] to everyone (each recipient's `you` differs)
    /// and tell the TUI, after any seat change.
    ///
    /// `interrupted` names the writer a human just took the lock from, and is
    /// `Some` at exactly the three sites that can do that — a browser's
    /// `Attach { seat: TakeOver }`, a browser's `take_input_lock`, and the
    /// desktop's palette command. Every other caller passes `None`, which is
    /// what keeps the browser's evicted panel off the screen during the
    /// ordinary hand-offs that make up almost all of this frame's traffic. It is
    /// a parameter rather than something read back out of
    /// [`Shared::announced_holder`] because that only records *what* moved; the
    /// caller is the only one that knows *why*.
    fn announce_seats(&self, interrupted: Option<&Writer>) {
        let now_ms = self.now_ms();
        let holder = self.holder();
        let (frames, rows) = {
            let registry = self.registry();
            (
                registry.seat_frames(now_ms, holder.as_ref(), interrupted),
                registry.seat_rows(None, holder.as_ref()),
            )
        };
        {
            let mut registry = self.registry();
            for (id, msg) in frames {
                registry.send_to(&id, msg);
            }
        }
        // Whatever we just said is now what everyone has been told, including
        // about the lock — so a `sync_input_lock` right behind an attach does
        // not repeat it.
        *self.announced_holder.unwrap_or_recover() = holder;
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

/// Resolve once the desktop has asked the live sockets to re-check their
/// credentials ([`WebServerHandle::recheck_credentials`]).
///
/// A dropped sender means the handle is going away, which
/// [`await_shutdown`] already answers with a `Shutdown` frame — so this then
/// never resolves at all rather than resolving forever, which would spin the
/// `select!` it sits in.
async fn await_revocation(rx: &mut watch::Receiver<u64>) {
    if rx.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

// ===========================================================================
// Seat arbitration (D14)
// ===========================================================================

/// One attached browser.
struct Viewer {
    id: ViewerId,
    /// The address off the socket plus the browser's own claim about itself,
    /// kept apart so [`SeatInfo`] can carry each fact in its own field (2f).
    identity: ViewerIdentity,
    seat: Seat,
    since_ms: i64,
    /// The version this viewer was answered at ([`negotiated`]), so a later
    /// `request_snapshot` is stamped the way its first snapshot was.
    protocol_version: u16,
    /// A window or a background link (v7). Always [`ConnectionKind::Window`]
    /// on a TUI or Desktop host, which has no background links.
    kind: ConnectionKind,
    tx: mpsc::Sender<ServerMsg>,
}

impl Viewer {
    fn info(&self, you: Option<&ViewerId>, holder: Option<&Writer>) -> SeatInfo {
        SeatInfo {
            viewer_id: Some(self.id.clone()),
            label: self.identity.label(),
            // Host-observed, always known for a viewer: this row exists because
            // a socket connected from somewhere.
            address: Some(self.identity.address.to_string()),
            // The browser's claim, and `None` when it made none. 2f drops the
            // `browser` row rather than printing a guess.
            user_agent_label: self.identity.user_agent_label.clone(),
            seat: self.seat,
            // The role and the turn are separate facts: an observer can never
            // hold the lock, and a writer only holds it while it is typing.
            holds_input: holder == Some(&Writer::Viewer(self.id.clone())),
            since_ms: self.since_ms,
            is_you: you == Some(&self.id),
        }
    }
}

/// N writers plus N observers (D14 as revised), and the preemption path.
///
/// **The registry owns roles, not turns.** Which writer may type at this instant
/// is [`crate::web::arbiter::InputArbiter`], deliberately kept out of here: the
/// roster changes when a tab opens or closes, the lock changes several times a
/// minute, and one mutex for both would put every keystroke behind the same lock
/// as every fan-out. The two meet only in [`SeatRegistry::seat_rows`], which is
/// handed the current holder and marks the row that has it.
///
/// Insertion order is display order, so the viewer chip's rows are stable while
/// a tab is attached.
struct SeatRegistry {
    viewers: Vec<Viewer>,
    /// A worker's registry has no desktop row (nobody sits at a worker) and
    /// keeps its background links out of the seat rows.
    host_kind: HostKind,
    /// Highest [`crate::web::protocol::Input::seq`] forwarded per viewer, kept
    /// after a disconnect so a reconnect can be told what already landed
    /// ([`Snapshot::last_input_seq`]). Bounded by
    /// [`REMEMBERED_INPUT_CURSORS`], oldest dropped first.
    input_cursors: Vec<(ViewerId, u64)>,
    /// When the server started — the desktop row's `since_ms`.
    started_ms: i64,
}

impl SeatRegistry {
    fn new(started_ms: i64, host_kind: HostKind) -> Self {
        SeatRegistry {
            viewers: Vec::new(),
            host_kind,
            input_cursors: Vec::new(),
            started_ms,
        }
    }

    /// Add a viewer, or return the existing one's slot. New viewers start as
    /// observers: a writer's seat is something you then ask for, and asking is
    /// what makes a browser that only ever watches cost nothing in arbitration.
    ///
    /// `kind` is what the attach asked for; a host that is not a worker has
    /// no background links and records every viewer as a window.
    fn register(
        &mut self,
        id: ViewerId,
        identity: ViewerIdentity,
        since_ms: i64,
        protocol_version: u16,
        kind: ConnectionKind,
        tx: mpsc::Sender<ServerMsg>,
    ) {
        let kind = if self.host_kind == HostKind::Worker {
            kind
        } else {
            ConnectionKind::Window
        };
        if let Some(viewer) = self.viewers.iter_mut().find(|v| v.id == id) {
            // A re-attach on the same socket may speak another version, and
            // may say what it is differently.
            viewer.protocol_version = protocol_version;
            viewer.kind = kind;
            return;
        }
        self.viewers.push(Viewer {
            id,
            identity,
            seat: Seat::Observing,
            since_ms,
            protocol_version,
            kind,
            tx,
        });
    }

    fn viewer(&self, id: &ViewerId) -> Option<&Viewer> {
        self.viewers.iter().find(|v| &v.id == id)
    }

    fn kind_of(&self, id: &ViewerId) -> Option<ConnectionKind> {
        self.viewer(id).map(|v| v.kind)
    }

    /// How many connections of `kind` are attached.
    fn count(&self, kind: ConnectionKind) -> usize {
        self.viewers.iter().filter(|v| v.kind == kind).count()
    }

    /// Every connection of `kind`, in attach order.
    fn ids_of(&self, kind: ConnectionKind) -> Vec<ViewerId> {
        self.viewers
            .iter()
            .filter(|v| v.kind == kind)
            .map(|v| v.id.clone())
            .collect()
    }

    /// Who is connected to a worker, as the holder of `token` sees it: the
    /// window (if any), and whether it is the recipient's own device.
    fn connections_view(&self, token: Option<&TokenId>) -> ConnectionsView {
        let window = self
            .viewers
            .iter()
            .find(|v| v.kind == ConnectionKind::Window)
            .map(|v| WindowInfo {
                label: v.identity.device_label(),
                address: Some(v.identity.address.to_string()),
                since_ms: v.since_ms,
                same_device: token == Some(&v.identity.token),
            });
        ConnectionsView {
            window,
            background_links: u32::try_from(self.count(ConnectionKind::Background))
                .unwrap_or(u32::MAX),
        }
    }

    /// A worker's one-window rule (design §2.5): make `keep` the only window
    /// by removing every other one.
    ///
    /// The one `keep` is resuming ([`Attach::resume_viewer`]) is the same
    /// window coming back over a fresh socket — its old one is usually
    /// half-open — so it is removed **silently**. Every other window is sent
    /// [`ServerMsg::TakenOver`] naming `by`, then removed; its connection task
    /// writes that frame and closes. Removal drops the registry's sender,
    /// which is what closes a silently replaced socket. Background links are
    /// never touched: they neither take over nor are taken over.
    ///
    /// Returns every id removed, so the caller can free their input lock.
    fn displace_windows(
        &mut self,
        keep: &ViewerId,
        resuming: Option<&ViewerId>,
        by: &str,
    ) -> Vec<ViewerId> {
        let displaced: Vec<ViewerId> = self
            .viewers
            .iter()
            .filter(|v| v.kind == ConnectionKind::Window && &v.id != keep)
            .map(|v| v.id.clone())
            .collect();
        for id in &displaced {
            if resuming != Some(id) {
                debuglog::log(&format!("web VIEWER taken over id={id}"));
                self.send_to(id, ServerMsg::TakenOver { by: by.to_string() });
            }
            self.remove(id);
        }
        displaced
    }

    /// The version this viewer was answered at; [`PROTOCOL_VERSION`] for one
    /// that has gone.
    ///
    /// [`PROTOCOL_VERSION`]: crate::web::protocol::PROTOCOL_VERSION
    fn protocol_version_of(&self, id: &ViewerId) -> u16 {
        self.viewers
            .iter()
            .find(|v| &v.id == id)
            .map_or(crate::web::protocol::PROTOCOL_VERSION, |v| {
                v.protocol_version
            })
    }

    fn remove(&mut self, id: &ViewerId) {
        self.viewers.retain(|v| &v.id != id);
    }

    /// Every seated writer, in display order. Several is now the normal case.
    fn writers(&self) -> Vec<ViewerId> {
        self.viewers
            .iter()
            .filter(|v| v.seat == Seat::Writing)
            .map(|v| v.id.clone())
            .collect()
    }

    fn seat_of(&self, id: &ViewerId) -> Option<Seat> {
        self.viewers.iter().find(|v| &v.id == id).map(|v| v.seat)
    }

    /// The viewer's chip label — the address the host observed plus whatever the
    /// browser said about itself, already sanitised and length-capped by
    /// [`sanitize_label`]. D13's origin line renders it verbatim.
    fn label_of(&self, id: &ViewerId) -> Option<String> {
        self.viewers
            .iter()
            .find(|v| &v.id == id)
            .map(|v| v.identity.label())
    }

    /// Grant one [`SeatRequest`]. The viewer must already be registered.
    ///
    /// **No request is refused any more.** D14's revision is exactly this: a
    /// seat is a role, several viewers may be writers at once, and the scarce
    /// thing — the turn to type — is the input lock, which lives in
    /// [`crate::web::arbiter`] and is not granted here.
    ///
    /// Takeover has **no dedicated frame** — the client re-sends
    /// `Attach { seat: TakeOver }` — and it now means *seat me as a writer and
    /// take the lock now*. Nobody is demoted by it: the writer that was
    /// interrupted keeps its seat and gets the lock back the moment the
    /// interrupter goes quiet, which is why 2f can honestly offer `Watch
    /// read-only` as a choice rather than as a consolation.
    fn request_seat(&mut self, id: &ViewerId, request: SeatRequest) -> Seat {
        let seat = match request {
            SeatRequest::Observe => Seat::Observing,
            SeatRequest::Write | SeatRequest::TakeOver => Seat::Writing,
        };
        self.set_seat(id, seat);
        seat
    }

    /// Stop competing for input voluntarily (`release_seat`): become an
    /// observer. Whether this viewer also held the input lock is the arbiter's
    /// business, and the caller releases it there.
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
    /// **The desktop is always a writer**, because its keyboard is never
    /// revoked: nothing a browser does takes the role away from the person at
    /// the machine. What it does *not* always have is the turn — the desktop
    /// contends for the input lock on exactly the same terms as every browser,
    /// and this is the row that says so when it does not hold it.
    ///
    /// `holder` is passed in rather than read here because the lock lives in
    /// [`crate::web::arbiter`]; see the type's doc for why the two are apart.
    ///
    /// A **worker** has no desktop row — nobody sits at it, and nothing there
    /// types — and its background links have no row either: they are not
    /// seated, they watch.
    fn seat_rows(&self, you: Option<&ViewerId>, holder: Option<&Writer>) -> Vec<SeatInfo> {
        let mut rows = Vec::with_capacity(self.viewers.len() + 1);
        if self.host_kind == HostKind::Worker {
            rows.extend(
                self.viewers
                    .iter()
                    .filter(|v| v.kind == ConnectionKind::Window)
                    .map(|v| v.info(you, holder)),
            );
            return rows;
        }
        rows.push(SeatInfo {
            viewer_id: None,
            label: DESKTOP_SEAT_LABEL.to_string(),
            // The desktop arrived over no socket, so there is no address the
            // host observed and no browser to describe. `None` says so; a
            // placeholder like `localhost` would be an invention.
            address: None,
            user_agent_label: None,
            seat: Seat::Writing,
            holds_input: holder == Some(&Writer::Desktop),
            since_ms: self.started_ms,
            is_you: false,
        });
        rows.extend(self.viewers.iter().map(|v| v.info(you, holder)));
        rows
    }

    /// One `Delta::Seats` per viewer, each with that viewer's own `you` — and,
    /// when a preemption caused this fan-out, its own `you_were_preempted`.
    ///
    /// `interrupted` is the writer that a human *deliberately* took the lock
    /// from, and it is `None` for every other reason a seat list is sent: a tab
    /// arriving or leaving, a seat released, and — the one that matters — the
    /// ordinary idle hand-off that [`WebServerHandle::sync_input_lock`]
    /// announces. Only the first of those is worth a panel, so only the first
    /// sets the flag.
    fn seat_frames(
        &self,
        server_time_ms: i64,
        holder: Option<&Writer>,
        interrupted: Option<&Writer>,
    ) -> Vec<(ViewerId, ServerMsg)> {
        // An interrupted *desktop* reaches no browser's panel, and that is not
        // an oversight: 2f gives the desktop a transient strip in D13's origin
        // vocabulary, because the person at the machine has no decision to make
        // — their keyboard was never revoked, only their turn. Matched
        // exhaustively so that a third kind of writer cannot be silently
        // dropped into the `None` arm.
        let interrupted = match interrupted {
            Some(Writer::Viewer(id)) => Some(id),
            Some(Writer::Desktop) | None => None,
        };
        // A background link is not seated, so a seat list is not news to it.
        self.viewers
            .iter()
            .filter(|v| v.kind == ConnectionKind::Window)
            .map(|v| {
                (
                    v.id.clone(),
                    ServerMsg::Delta(Delta::Seats {
                        you: v.seat,
                        seats: self.seat_rows(Some(&v.id), holder),
                        // The reference clock for every row's `since_ms`. A
                        // seat list without one is a seat list the browser
                        // cannot date, which is how 2f's `connected` row came
                        // to be silently dropped on this path.
                        server_time_ms,
                        you_were_preempted: interrupted == Some(&v.id),
                    }),
                )
            })
            .collect()
    }

    /// Queue a frame for one viewer. A viewer whose queue is full has stopped
    /// keeping up and is dropped from the registry — its connection task sees
    /// its channel close and shuts the socket. See [`VIEWER_QUEUE_FRAMES`].
    ///
    /// A background link is sent only what [`background_may_receive`] allows;
    /// anything else addressed to it is dropped here, which is the one place
    /// every route out of the host passes.
    fn send_to(&mut self, id: &ViewerId, msg: ServerMsg) {
        let Some(viewer) = self.viewers.iter().find(|v| &v.id == id) else {
            return;
        };
        if viewer.kind == ConnectionKind::Background && !background_may_receive(&msg) {
            return;
        }
        if viewer.tx.try_send(msg).is_err() {
            debuglog::log(&format!(
                "web VIEWER dropped id={id} address={} reason=queue_full",
                viewer.identity.address
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

/// Whether a frame may reach a worker's **background link** (design §2.5).
///
/// A background link keeps a Desktop's worker list honest — status, health,
/// notifications, who is connected — and nothing more. It is denied what only
/// a window renders, and what would cost the host per-link work for a screen
/// nobody is looking at: terminal bytes, git and configuration answers, the
/// project picker's and the clone's frames, and host messages (a window reads
/// those, and marks them read by being there).
///
/// An allow-list *by exception*: everything else passes, so a frame added to
/// the protocol later reaches a background link unless it is added here.
fn background_may_receive(msg: &ServerMsg) -> bool {
    !matches!(
        msg,
        ServerMsg::TermBytes(_)
            | ServerMsg::GitStatus(_)
            | ServerMsg::Configuration(_)
            | ServerMsg::DirListing(_)
            | ServerMsg::ProjectScan(_)
            | ServerMsg::CloneProgress(_)
            | ServerMsg::CloneResult(_)
            | ServerMsg::Delta(Delta::HostMessage(_))
    )
}

/// Accept or refuse a peer's protocol version on a host of `kind`.
///
/// Every host takes [`check_version`]'s range. A worker additionally refuses
/// anything below [`WORKER_MIN_CLIENT_VERSION`] — a v6 client knows nothing of
/// one-window takeover, background links or the worker's commands — and says
/// so with its own floor, so the client's "update FlightDeck" message names
/// the right number.
fn accept_version(kind: HostKind, peer: u16) -> Result<u16, VersionMismatch> {
    let accepted = check_version(peer);
    if kind != HostKind::Worker {
        return accepted;
    }
    match accepted {
        Ok(version) if version >= WORKER_MIN_CLIENT_VERSION => Ok(version),
        _ => Err(VersionMismatch {
            local: PROTOCOL_VERSION,
            peer,
            min_supported: WORKER_MIN_CLIENT_VERSION,
            max_supported: MAX_SUPPORTED_VERSION,
        }),
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
///
/// ## Why the two policy numbers ride on the refusal
///
/// Artboard 2b needs [`RATE_LIMIT_LOCKOUT_MS`] and [`BOOTSTRAP_CODE_TTL_MS`] in
/// its copy — *"3 attempts left before this address is rate-limited for 60s"*
/// and *"Codes last 120 seconds and only work once"* — and it needs the first of
/// those **before the limiter has ever fired**, which `retry_after_ms` cannot
/// give it: that value only exists once the address is already locked out. The
/// browser used to mirror both as TypeScript constants, which is a duplication
/// that drifts silently the moment either constant here is tuned.
///
/// `GET /auth/session` was the other candidate carrier, and it is unnecessary:
/// the SPA's very first act is that call, a browser with no live cookie is
/// **refused** by it, and that refusal comes through this same function. So
/// every path that can reach one of 2b's screens has already been through here,
/// while the `authenticated: true` body is only ever followed by the app — which
/// draws none of that copy. One carrier, and no field nobody reads.
///
/// The numbers follow `attempts_remaining`'s precedent exactly: host-sent,
/// never guessed, and the browser degrades to a sentence without the clause
/// rather than filling the gap from memory.
fn refusal_body(
    failure: AuthFailure,
    attempts_remaining: u32,
    server_time_ms: i64,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "ok": false,
        "screen": screen_name(failure.screen()),
        "reason": failure.as_str(),
        "attempts_remaining": attempts_remaining,
        // Whole seconds: the copy that reads them is written in seconds, and a
        // millisecond value on a screen would be the browser's problem to round.
        "lockout_seconds": RATE_LIMIT_LOCKOUT_MS / 1000,
        "code_ttl_seconds": BOOTSTRAP_CODE_TTL_MS / 1000,
    });
    if let AuthFailure::RateLimited { retry_after_ms } = failure {
        body["retry_after_ms"] = serde_json::json!(retry_after_ms);
    }
    if let AuthFailure::TokenRevoked {
        revoked_at_unix_secs,
    } = failure
    {
        // 2b: "withdrew this browser's access **12s ago**". Sent as an absolute
        // instant paired with the host's own clock, exactly as `Snapshot` pairs
        // `since_ms` with `server_time_ms`, so the browser subtracts two host
        // timestamps instead of measuring a host instant with its own clock.
        //
        // A zero is not a time — it is 1970 — so it is sent as no time at all
        // and the browser renders the sentence without the clause. Better a
        // shorter true sentence than a precise false one.
        if revoked_at_unix_secs > 0 {
            let revoked_at_ms = (revoked_at_unix_secs as i64).saturating_mul(1000);
            body["revoked_at_ms"] = serde_json::json!(revoked_at_ms);
            body["server_time_ms"] = serde_json::json!(server_time_ms);
        }
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

fn refusal_response(
    failure: AuthFailure,
    attempts_remaining: u32,
    server_time_ms: i64,
) -> Response {
    let mut response = json_response(
        refusal_status(failure),
        refusal_body(failure, attempts_remaining, server_time_ms),
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
        Lookup::NotFound => not_found(),
    }
}

/// Every non-protocol path on a server that does not serve the SPA (a
/// worker, design A5).
async fn not_found_route() -> Response {
    not_found()
}

fn not_found() -> Response {
    let mut response = Response::new(Body::from("not found"));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
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
        // The label is whatever the browser said about itself — in practice
        // `navigator.userAgent`, in principle anything that fits in a JSON
        // body. It is stripped of control characters and capped **before** it
        // is persisted, because `web.json` keeps it for the life of the
        // credential and a megabyte of it would be a megabyte on disk forever.
        // The cap is generous enough for every real user-agent; the desktop's
        // own list shortens it further when it draws it.
        let label = request
            .label
            .as_deref()
            .map(|raw| truncate_chars(&sanitize_label(raw), MAX_STORED_LABEL_CHARS))
            .filter(|s| !s.is_empty());
        let result = store.exchange_code(&address, &request.code, label.as_deref());
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
            refusal_response(failure, attempts, shared.now_ms())
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
            let mut body = refusal_body(failure, attempts, shared.now_ms());
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

/// `GET /ws` — the web-protocol upgrade (D12).
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
            return refusal_response(failure, attempts, shared.now_ms());
        }
    };

    // The `TokenId` goes *into* the identity rather than being dropped here.
    // That is the whole of R20's fix on this side: a socket that cannot name its
    // own credential cannot be told that credential was withdrawn.
    let identity = viewer_identity(peer, &headers, token);
    debuglog::log(&format!(
        "web WS upgrade address={address} token={} label={}",
        identity.token,
        identity.label()
    ));
    upgrade.on_upgrade(move |socket| serve_viewer(shared, socket, identity))
}

/// Who a viewer is, as three facts of very different standing.
///
/// The two *displayed* facts — address and user-agent — are kept apart all the
/// way to [`SeatInfo`] rather than being merged into one string the browser
/// would have to take back apart. A user-agent string is attacker-supplied and
/// can contain the separator, so a browser-side split is a parse an attacker can
/// steer — see the `SeatInfo` doc comment.
///
/// The third fact, `token`, is never displayed and never sent anywhere: it is
/// the credential this socket authenticated with, so the host can ask the
/// credential store about *this* socket rather than about sockets in general.
/// It is what makes revocation per-browser instead of all-or-nothing (§6.5
/// R20).
///
/// [`ViewerIdentity::label`] is the *derived* form: the one-line chip label, and
/// the only place the two displayed facts are ever joined.
#[derive(Clone, Debug)]
struct ViewerIdentity {
    /// The peer address the host observed on the socket. Never client-supplied.
    address: IpAddr,
    /// What the browser said it is, already sanitised and length-capped, or
    /// `None` when it said nothing we could use. Only ever displayed.
    user_agent_label: Option<String>,
    /// The credential [`ws_route`] verified before the upgrade.
    ///
    /// A public identifier, not a secret — [`TokenId`]'s own docs say so, which
    /// is why the auth `debuglog` line prints it. The secret itself is seen once
    /// by [`CredentialStore::verify_token`] and never held here.
    token: TokenId,
}

impl ViewerIdentity {
    /// The one-line chip label, `192.168.2.20 · Chrome on macOS`.
    ///
    /// Joining is safe; it is the *un*joining that is not, which is why the
    /// parts also travel on the wire in their own fields.
    fn label(&self) -> String {
        match &self.user_agent_label {
            Some(agent) => format!("{} · {agent}", self.address),
            None => self.address.to_string(),
        }
    }

    /// The device this connection is from, as a worker names it — in
    /// [`ServerMsg::TakenOver`] and [`WindowInfo::label`].
    ///
    /// FlightDeck Desktop introduces itself as
    /// `FlightDeck Desktop/<version> (<hostname>)` (design A11), and the
    /// hostname is what tells one Desktop from another. Any other client is
    /// named by its chip label.
    fn device_label(&self) -> String {
        let host = self
            .user_agent_label
            .as_deref()
            .and_then(|agent| agent.strip_prefix(NATIVE_CLIENT_AGENT))
            .and_then(|rest| rest.strip_suffix(')'))
            .and_then(|rest| rest.rsplit_once('('))
            .map(|(_, host)| host.trim())
            .filter(|host| !host.is_empty());
        match host {
            Some(host) => host.to_string(),
            None => self.label(),
        }
    }

    /// Replace the browser's self-description with the claim it sent in its
    /// [`Attach`] frame, if that claim survives sanitising. The address is left
    /// alone unconditionally: it came off the socket, and no frame can move it.
    fn with_claim(&self, claim: Option<&str>) -> ViewerIdentity {
        let refined = claim
            .map(|raw| truncate_chars(&sanitize_label(raw), MAX_LABEL_CHARS))
            .filter(|s| !s.is_empty());
        match refined {
            Some(agent) => ViewerIdentity {
                address: self.address,
                user_agent_label: Some(agent),
                // The credential is not up for renegotiation either: an `Attach`
                // frame refines what we *say* the browser is, never what it is
                // allowed to do.
                token: self.token.clone(),
            },
            // Nothing usable was claimed, so we keep whatever the `User-Agent`
            // header gave us rather than forgetting a fact we already had.
            None => self.clone(),
        }
    }
}

/// The identity of a viewer as the request itself describes it: the address off
/// the socket, the credential the upgrade already verified, plus whatever the
/// browser says about itself.
///
/// The user-agent is the browser's own claim and is only ever displayed.
/// Truncated so a hostile `User-Agent` cannot make the desktop's chip unreadable.
///
/// `token` is a parameter rather than something read out of the headers here,
/// because the only honest source for it is the verification [`ws_route`]
/// already performed: re-deriving it would be a second answer to a question
/// that has one.
fn viewer_identity(peer: SocketAddr, headers: &HeaderMap, token: TokenId) -> ViewerIdentity {
    let user_agent_label = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(coarse_user_agent)
        .filter(|s| !s.is_empty())
        .map(|agent| truncate_chars(&agent, MAX_LABEL_CHARS));
    ViewerIdentity {
        address: peer.ip(),
        user_agent_label,
        token,
    }
}

/// How much of a browser-supplied label the chip will carry.
pub(crate) const MAX_LABEL_CHARS: usize = 48;

/// How much of it `web.json` will keep. Longer than [`MAX_LABEL_CHARS`] because
/// what is *stored* is the raw claim and what is *drawn* is a reduction of it
/// ([`coarse_user_agent`] needs the whole string to find `Safari/` and
/// `iPhone`), and shorter than any browser's user-agent is unbounded.
const MAX_STORED_LABEL_CHARS: usize = 256;

/// Keep the first `max` **characters**, never splitting a codepoint.
/// `String::truncate` panics on a byte index that is not a char boundary, and
/// the label is attacker-supplied UTF-8.
pub(crate) fn truncate_chars(raw: &str, max: usize) -> String {
    raw.chars().take(max).collect()
}

/// The product name FlightDeck Desktop sends as its `User-Agent`
/// (`FlightDeck Desktop/<version>`) when it controls another instance.
pub const NATIVE_CLIENT_AGENT: &str = "FlightDeck Desktop";

/// Reduce a `User-Agent` to the `Chrome on macOS` shape turn 2 asks for, keeping
/// only characters that are safe to render verbatim in a terminal chip.
pub(crate) fn coarse_user_agent(raw: &str) -> String {
    // FlightDeck Desktop controlling this instance as a native client
    // (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.4). It names itself in full,
    // so the access overlay can list — and revoke — it by name.
    if raw.starts_with(NATIVE_CLIENT_AGENT) {
        return NATIVE_CLIENT_AGENT.to_string();
    }
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
    // iOS is tested **before** macOS on purpose: Safari on an iPhone sends
    // `(iPhone; CPU iPhone OS 17_0 like Mac OS X)`, so a `Mac OS X` test that
    // ran first would call every phone a Mac — and telling a phone from a
    // desktop is exactly what this label is for (`remote-control-gk94`).
    // Android is likewise before Linux, which its user-agent also contains.
    let os = if raw.contains("iPhone") || raw.contains("iPad") {
        "iOS"
    } else if raw.contains("Android") {
        "Android"
    } else if raw.contains("Mac OS X") || raw.contains("Macintosh") {
        "macOS"
    } else if raw.contains("Windows") {
        "Windows"
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
async fn serve_viewer(shared: Arc<Shared>, socket: WebSocket, identity: ViewerIdentity) {
    let guard = shared.drain.enter();
    let viewer_id = new_viewer_id();
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<ServerMsg>(VIEWER_QUEUE_FRAMES);
    // Once attached, the registry holds the **only** strong sender, so the
    // registry removing this viewer — a takeover, a silent replacement by its
    // own reconnect, a queue it could not keep up with — closes the channel,
    // and the `None` below closes the socket. Until then this task holds one
    // so the channel is open to attach with.
    let weak_tx = tx.downgrade();
    let mut unregistered_tx = Some(tx);
    let mut shutdown = shared.shutdown.clone();
    let mut revocations = shared.revocations.clone();
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
                            "frame was not valid web-protocol JSON",
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
                    &identity,
                    &weak_tx,
                    &mut attached,
                    client_msg,
                    &mut sink,
                )
                .await;
                if attached {
                    drop(unregistered_tx.take());
                }
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
                        // A frame that ends the connection ends it here, right
                        // after it is written: nothing the host queued behind
                        // it is for this socket any more.
                        if is_terminal_frame(&msg) {
                            let _ = sink.send(Message::Close(None)).await;
                            break;
                        }
                    }
                    // Every sender dropped: the registry evicted us (it could
                    // not keep up) or the server is going away.
                    None => break,
                }
            }
            // The desktop withdrew somebody's access. Ask whether it was
            // ours — every socket wakes, only the revoked ones leave.
            //
            // `changed()` is level-triggered and cancel-safe, so a bump that
            // arrives while another branch of this `select!` is running is still
            // seen on the next pass rather than lost.
            _ = await_revocation(&mut revocations) => {
                if shared.credential_is_active(&identity.token) {
                    continue;
                }
                debuglog::log(&format!(
                    "web WS revoked token={} label={}",
                    identity.token,
                    identity.label()
                ));
                // Best effort, exactly as the shutdown branch below: a socket
                // that cannot take this frame is already gone, and it has lost
                // its powers either way — the gate in `handle_client_msg`
                // does not depend on this frame arriving.
                let _ = send_msg(&mut sink, &revoked_frame()).await;
                let _ = sink.send(Message::Close(None)).await;
                break;
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
    // A closed socket must not hold the terminal for the rest of the idle
    // window: nobody is coming back to finish that burst.
    shared
        .input_lock()
        .release(&Writer::Viewer(viewer_id.clone()));
    if was_attached {
        shared.notify(WebInbound::ViewerDetached {
            viewer_id: viewer_id.clone(),
        });
        // Someone leaving is a seat change: the seat they held is free now.
        // Nobody was interrupted — a socket that closed took its own turn with
        // it, and the writer that types next is claiming a free lock.
        shared.announce_seats(None);
        shared.announce_connections();
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

/// Whether the host queueing `msg` for a viewer means "and then you are
/// gone": a takeover or a shutdown. The connection closes right after writing
/// one (design §2.5).
fn is_terminal_frame(msg: &ServerMsg) -> bool {
    matches!(
        msg,
        ServerMsg::TakenOver { .. } | ServerMsg::Shutdown { .. }
    )
}

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

/// The text of a frame, or `None` for anything the web protocol does not use.
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
/// forward-compatibility policy `crate::web::protocol` documents.
fn parse_client_msg(text: &str) -> Option<ClientMsg> {
    serde_json::from_str::<ClientMsg>(text).ok()
}

/// The frame a socket is closed with when its credential has been withdrawn.
///
/// `self_initiated` is unconditionally `false`: the browser did not ask for
/// this, the desktop did, and Q5 is explicit that the difference is not
/// derivable from the reason alone. `detail` is `None` because the host has
/// nothing to add that the browser cannot say better — 2b's revoked panel
/// already knows the words, and inventing a time here would be a claim about
/// *when* that this frame has no honest source for.
fn revoked_frame() -> ServerMsg {
    ServerMsg::Shutdown {
        reason: ShutdownReason::TokenRevoked,
        self_initiated: false,
        detail: None,
    }
}

/// Handle one frame from the browser.
///
/// **Every frame is gated on the credential first.** Not just `Input`: a
/// revoked browser must not be able to open a dialog, run a palette command,
/// re-attach under a new seat or resize anything either, and listing the
/// dangerous frames would be a list that goes stale the next time the wire
/// grows a member.
///
/// The gate is here, on the frame's own path, rather than in the `select!`
/// branch that closes the socket, because those two are not the same guarantee.
/// The branch is *prompt*; this is *total*. A keystroke that arrived in the
/// microseconds between the desktop writing the revocation and this socket
/// noticing it is refused by this check, because this check asks the store —
/// the same store, the same instant — instead of asking a flag that had not
/// been set yet.
#[allow(clippy::too_many_arguments)]
async fn handle_client_msg(
    shared: &Arc<Shared>,
    viewer_id: &ViewerId,
    identity: &ViewerIdentity,
    tx: &mpsc::WeakSender<ServerMsg>,
    attached: &mut bool,
    msg: ClientMsg,
    sink: &mut Sink,
) -> Flow {
    if !shared.credential_is_active(&identity.token) {
        debuglog::log(&format!(
            "web WS frame refused — revoked token={} label={}",
            identity.token,
            identity.label()
        ));
        let _ = send_msg(sink, &revoked_frame()).await;
        return Flow::Close;
    }
    match msg {
        ClientMsg::Attach(attach) => {
            handle_attach(shared, viewer_id, identity, tx, attached, attach, sink).await
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
                Some(Seat::Writing) => forward_or_refuse(shared, viewer_id, input, sink).await,
                // An observer never contends, so it is never *refused* by the
                // lock — it simply has no input to arbitrate. Acked all the
                // same, never silently dropped (§5.1).
                Some(Seat::Observing) | None => {
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
        ClientMsg::ResizeTerminal(resize) => {
            // A worker has no pane of its own, so its one window sizes the
            // PTYs (PRD W5). Everywhere else D4 holds and the frame is dropped
            // silently — as it is from a background link, which only watches,
            // and for a grid nothing could draw.
            let from_window = shared.is_worker()
                && shared.registry().kind_of(viewer_id) == Some(ConnectionKind::Window);
            if from_window && resize.cols > 0 && resize.rows > 0 {
                shared.notify(WebInbound::ResizeTerminal {
                    viewer_id: viewer_id.clone(),
                    terminal_id: resize.terminal_id,
                    size: PtySize {
                        rows: resize.rows,
                        cols: resize.cols,
                    },
                });
            }
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

/// One writer's keystrokes: arbitrate, then forward or refuse.
///
/// **The claim happens here, before the bytes enter the channel**, which is what
/// makes the drain on the other side safe: everything queued is from one holder
/// until the lock moves, so applying it in order cannot splice two writers'
/// bytes together. Forwarding a frame and letting the applier decide would put
/// both writers' bytes in the same queue and lose the property entirely.
///
/// A refusal is two frames, deliberately:
///
/// * an [`Ack`] with [`AckOutcome::Rejected`], because §5.1's held-keystroke
///   queue is keyed by `seq` and would replay this one for ever otherwise; and
/// * an [`ErrorCode::SeatHeld`] carrying the holder's [`SeatInfo`], because 2f's
///   panel names three facts about them and an ack's free-text `detail` is not
///   a place to encode three facts.
///
/// The `seq` is **not** recorded: nothing was forwarded, so
/// [`Snapshot::last_input_seq`] must not claim it was.
async fn forward_or_refuse(
    shared: &Arc<Shared>,
    viewer_id: &ViewerId,
    input: crate::web::protocol::Input,
    sink: &mut Sink,
) -> Flow {
    let now = shared.now_ms();
    let who = Writer::Viewer(viewer_id.clone());
    let label = shared
        .registry()
        .label_of(viewer_id)
        .unwrap_or_else(|| viewer_id.to_string());
    let claim = shared.input_lock().claim(&who, &label, now);
    match claim {
        Claim::Granted => {
            shared.registry().record_input(viewer_id, input.seq);
            // The applier owns the ack: forwarding is not the same claim as
            // "the PTY took it". `src/web/stream.rs` answers with
            // `WebOutbound::Viewer { ServerMsg::Ack }`.
            shared.notify(WebInbound::Input {
                viewer_id: viewer_id.clone(),
                input,
            });
            Flow::Continue
        }
        Claim::Refused { by, label } => {
            let holder = shared
                .registry()
                .seat_rows(Some(viewer_id), Some(&by))
                .into_iter()
                .find(|row| row.holds_input)
                .unwrap_or_else(|| lost_holder(&by, &label));
            let _ = send_msg(
                sink,
                &ServerMsg::Ack(Ack {
                    seq: input.seq,
                    outcome: AckOutcome::Rejected,
                    detail: Some(format!(
                        "{label} is typing — this keystroke was refused rather than \
                         mixed into theirs"
                    )),
                }),
            )
            .await;
            let _ = send_msg(sink, &ServerMsg::Error(WireError::seat_held(holder))).await;
            Flow::Continue
        }
    }
}

/// The holder's row when the seat list no longer has one — the holder's socket
/// closed in the moment between the claim and the lookup.
///
/// Only what the arbiter still knows is filled in: who, and their label. The
/// facts that came off a socket are `None`, which is the honest shape — 2f drops
/// a row it was told nothing about rather than printing a placeholder, and
/// `since_ms: 0` is never read here, because `WireError::seat_held` is not a
/// seat list and the panel it opens leaves `connected` blank until one arrives.
fn lost_holder(who: &Writer, label: &str) -> SeatInfo {
    SeatInfo {
        // `None` is the desktop's spelling and would be a lie about a browser,
        // so the viewer's own id travels when there is one.
        viewer_id: match who {
            Writer::Desktop => None,
            Writer::Viewer(id) => Some(id.clone()),
        },
        label: label.to_string(),
        address: None,
        user_agent_label: None,
        seat: Seat::Writing,
        holds_input: true,
        since_ms: 0,
        is_you: false,
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_attach(
    shared: &Arc<Shared>,
    viewer_id: &ViewerId,
    identity: &ViewerIdentity,
    tx: &mpsc::WeakSender<ServerMsg>,
    attached: &mut bool,
    attach: Attach,
    sink: &mut Sink,
) -> Flow {
    if let Err(mismatch) = accept_version(shared.host_kind, attach.protocol_version) {
        let _ = send_msg(
            sink,
            &ServerMsg::Error(WireError::version_mismatch(mismatch)),
        )
        .await;
        // Nothing this build can serve. The browser's answer is "reload to
        // update", not a retry, so the socket closes.
        return Flow::Close;
    }

    // The browser's self-description refines what we say the *browser* is, and
    // nothing else. The address came off the socket and no frame can move it —
    // which is exactly why the two are separate fields rather than one string.
    let claim = attach
        .client
        .as_ref()
        .and_then(|c| c.label.clone().or_else(|| c.user_agent.clone()));
    let identity = identity.with_claim(claim.as_deref());
    let address = identity.address;
    let label = identity.label();

    // A v6 client is answered at v6: its link compares the snapshot's version
    // for equality, and nothing it is sent is newer than v6 (v7 frames come
    // only from worker hosts, which refuse it).
    let protocol_version = negotiated(attach.protocol_version);
    let now = shared.now_ms();
    let Some(tx) = tx.upgrade() else {
        // The registry already let this socket go (it was taken over, or fell
        // behind) and the close is on its way: nothing left to attach.
        return Flow::Close;
    };
    let worker = shared.is_worker();
    let (seat, kind, last_input_seq, displaced) = {
        let mut registry = shared.registry();
        registry.register(
            viewer_id.clone(),
            identity.clone(),
            now,
            protocol_version,
            attach.connection,
            tx,
        );
        let kind = registry.kind_of(viewer_id).unwrap_or_default();
        let last_input_seq = match attach.resume_viewer.as_ref() {
            Some(previous) => registry.adopt_cursor(previous, viewer_id),
            None => registry.input_cursor(viewer_id),
        };
        // A worker's seat model (design §2.5): its one window is the writer,
        // whatever seat it asked for, and every other window gives way —
        // removed here, before anyone is told about seats or connections, so
        // no announcement can still list it. A background link is never
        // seated and displaces nobody. Every other host keeps D14's
        // multi-writer model exactly.
        let (seat, displaced) = match (worker, kind) {
            (true, ConnectionKind::Window) => {
                let displaced = registry.displace_windows(
                    viewer_id,
                    attach.resume_viewer.as_ref(),
                    &identity.device_label(),
                );
                (
                    registry.request_seat(viewer_id, SeatRequest::Write),
                    displaced,
                )
            }
            (true, ConnectionKind::Background) => (
                registry.request_seat(viewer_id, SeatRequest::Observe),
                Vec::new(),
            ),
            (false, _) => (registry.request_seat(viewer_id, attach.seat), Vec::new()),
        };
        (seat, kind, last_input_seq, displaced)
    };
    // A displaced window must not keep the turn it held: its socket is
    // closing, and nobody is coming back to finish that burst.
    for gone in &displaced {
        shared.input_lock().release(&Writer::Viewer(gone.clone()));
    }

    // The one explicit override in the model, and the only way past a live burst
    // (D14 as revised). It is a *separate* frame from `Write` precisely because
    // 2f gates it behind a confirmation: seating yourself as a writer must not
    // silently interrupt somebody mid-word.
    //
    // A worker has one writer, so there is nobody to interrupt: its window
    // claims the lock by typing like any writer, and a background link never
    // holds it.
    let request = if worker {
        match kind {
            ConnectionKind::Window => SeatRequest::Write,
            ConnectionKind::Background => SeatRequest::Observe,
        }
    } else {
        attach.seat
    };
    let interrupted = match request {
        SeatRequest::TakeOver => preempt_for_viewer(shared, viewer_id, &label, now),
        // A viewer that stops competing must not keep the turn it holds, or the
        // terminal would sit locked to a tab that has promised never to type
        // into it again. Nobody is interrupted by it either: giving a turn up is
        // not taking one away.
        SeatRequest::Observe => {
            shared
                .input_lock()
                .release(&Writer::Viewer(viewer_id.clone()));
            None
        }
        // Seating yourself as a writer buys the right to contend, and nothing
        // more: the lock is still claimed by typing.
        SeatRequest::Write => None,
    };

    *attached = true;
    let snapshot = shared.snapshot_for(viewer_id, seat, last_input_seq, protocol_version);
    if send_msg(sink, &ServerMsg::Snapshot(Box::new(snapshot)))
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
        resume_viewer: attach.resume_viewer,
        kind,
        protocol_version,
    });
    // Any attach can change the seat map — a first attach adds a row, a
    // re-attach may have just moved the input lock. Both travel as this
    // `Delta::Seats`, never as a `Shutdown`: nobody is disconnected by a
    // takeover under D14 as revised, and an interrupted writer keeps its seat
    // (2f). `interrupted` is `Some` only for the `TakeOver` arm above, so the
    // writer that was cut into is the one — the only one — whose frame says so.
    shared.announce_seats(interrupted.as_ref());
    // On a worker, who is connected changed too — and a takeover changed it
    // for everyone, including the background links that never see seats.
    shared.announce_connections();
    Flow::Continue
}

/// Take the input lock for one browser and report **whom that interrupted**, so
/// the fan-out behind it can tell that writer, and only that writer, that this
/// was deliberate (`Delta::Seats::you_were_preempted`).
///
/// Two details that are load-bearing rather than defensive:
///
/// * The holder is read **before** the preemption, because a moment later it is
///   the claimant and the fact is gone.
/// * A claimant that already holds the lock interrupts **nobody**. Confirming
///   `Take over` twice, or re-attaching as `TakeOver` while already mid-burst,
///   is a no-op on the lock and must be a no-op on the panel too — otherwise
///   the browser that pressed the button is shown 2f's evicted panel about
///   itself.
fn preempt_for_viewer(
    shared: &Arc<Shared>,
    viewer_id: &ViewerId,
    label: &str,
    now_ms: i64,
) -> Option<Writer> {
    let me = Writer::Viewer(viewer_id.clone());
    let mut lock = shared.input_lock();
    let interrupted = match lock.holder() {
        Some(held) if held != &me => Some(held.clone()),
        // Ours already, or free: an override that overrode nothing.
        Some(_) | None => None,
    };
    lock.preempt(&me, label, now_ms);
    interrupted
}

/// Keep a browser-supplied label renderable: printable characters only, no
/// control bytes that could move a terminal cursor when the desktop draws the
/// chip.
pub(crate) fn sanitize_label(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

/// Answer one [`ClientMsg::Command`](crate::web::protocol::ClientMsg::Command).
///
/// Every decision comes from [`crate::web::commands::INVENTORY`] — the same
/// table the browser's palette was built from — in a fixed order:
///
/// 1. **Unknown name** → [`ErrorCode::NotSupported`], socket kept. The M2 door's
///    failure mode: a newer browser asks for something this build does not have
///    and is told so.
/// 2. **Read-only seat** → [`ErrorCode::ReadOnly`], before anything else is
///    considered (D14). It comes first deliberately: an observer must not learn
///    which commands *would* have worked, and no command added later can slip
///    past the check by being handled earlier.
/// 3. **A refusal this build states statically** → answered here, and the frame
///    never reaches the TUI. That is what makes `quit` safe by construction
///    (D16): there is no path from a bare frame to a dispatch.
/// 4. **Anything else** → forwarded as [`WebInbound::Command`]. The TUI applies
///    it through its own palette path and sends the [`Ack`], for the same reason
///    input is not acked here: this module has forwarded a frame, which is not
///    the same claim as "the host did it".
///
/// Two v7 additions sit in front of and inside that order:
///
/// * A [`crate::web::protocol::worker_command`] is recognised first, by
///   [`WorkerOp::parse`], and answered by [`handle_worker_op`] — those names
///   are deliberately not in the palette's table.
/// * On a worker, a **background link** may send `request_snapshot` and
///   nothing else ([`ErrorCode::ReadOnly`] for the rest, checked where step 2
///   is), and the rows [`crate::web::commands::worker_refusal`] names are
///   answered `Rejected` with the worker's own reason at step 3.
async fn handle_command(
    shared: &Arc<Shared>,
    viewer_id: &ViewerId,
    command: crate::web::protocol::Command,
    sink: &mut Sink,
) {
    use crate::web::commands::{self, Route};
    use crate::web::protocol::command as names;

    let (seat, kind) = {
        let registry = shared.registry();
        (
            registry.seat_of(viewer_id).unwrap_or(Seat::Observing),
            registry.kind_of(viewer_id).unwrap_or_default(),
        )
    };

    if let Some(parsed) = WorkerOp::parse(&command.name, command.args.as_ref()) {
        handle_worker_op(shared, viewer_id, seat, kind, command.seq, parsed, sink).await;
        return;
    }

    let Some(spec) = commands::lookup(&command.name) else {
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
        return;
    };

    // A background link watches: it may resync, and nothing else — not even
    // the seat bookkeeping an observer window may do.
    let background_refused =
        kind == ConnectionKind::Background && spec.name != names::REQUEST_SNAPSHOT;
    if background_refused || (spec.requires_control() && seat != Seat::Writing) {
        // Read-only means read-only. D3 makes the selection shared with the
        // desktop, so letting an observer move it would be input by another
        // name — and the same holds for every command that changes anything.
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

    // A worker knows these rows and will not run them for a remote window
    // (design §2.4) — the same answer, in the same words, as the refusal its
    // inventory carries.
    if shared.is_worker() {
        if let Some(reason) = commands::worker_refusal(spec.name) {
            let _ = send_msg(
                sink,
                &ServerMsg::Ack(Ack {
                    seq: command.seq,
                    outcome: AckOutcome::Rejected,
                    detail: Some(reason.to_string()),
                }),
            )
            .await;
            return;
        }
    }

    match spec.route {
        // Answerable here, from the published state, for any seat: it is how a
        // viewer that believes it has drifted recovers.
        Route::Server if spec.name == names::REQUEST_SNAPSHOT => {
            let (last_input_seq, protocol_version) = {
                let registry = shared.registry();
                (
                    registry.input_cursor(viewer_id),
                    registry.protocol_version_of(viewer_id),
                )
            };
            let snapshot = shared.snapshot_for(viewer_id, seat, last_input_seq, protocol_version);
            let _ = send_msg(sink, &ServerMsg::Snapshot(Box::new(snapshot))).await;
            let _ = send_msg(sink, &applied(command.seq)).await;
        }
        // Seat bookkeeping is this module's job (D14), so it never travels to
        // the TUI and back.
        Route::Server if spec.name == names::TAKE_INPUT_LOCK => {
            // The browser's own explicit override, for a surface that is already
            // a writer and does not want to re-attach to interrupt. Same act as
            // `Attach { seat: TakeOver }`, same confirmation behind it (2f).
            let now = shared.now_ms();
            let label = shared
                .registry()
                .label_of(viewer_id)
                .unwrap_or_else(|| viewer_id.to_string());
            let interrupted = preempt_for_viewer(shared, viewer_id, &label, now);
            let _ = send_msg(sink, &applied(command.seq)).await;
            shared.announce_seats(interrupted.as_ref());
        }
        // Seat bookkeeping is this module's job (D14), so it never travels to
        // the TUI and back.
        Route::Server => {
            debug_assert_eq!(spec.name, names::RELEASE_SEAT);
            shared.registry().release(viewer_id);
            shared
                .input_lock()
                .release(&Writer::Viewer(viewer_id.clone()));
            let _ = send_msg(sink, &applied(command.seq)).await;
            // Giving a turn up interrupts nobody: the next writer to type is
            // claiming a lock that is free, not one taken from anyone.
            shared.announce_seats(None);
        }
        // D16: the host knows the command and will not run it for a browser.
        // Acked, not ignored — a `host only` action that silently did nothing
        // would be indistinguishable from one that worked.
        Route::Rejected(reason) => {
            let _ = send_msg(
                sink,
                &ServerMsg::Ack(Ack {
                    seq: command.seq,
                    outcome: AckOutcome::Rejected,
                    detail: Some(reason.to_string()),
                }),
            )
            .await;
        }
        // The action exists but the browser has no surface for what it opens.
        // Refused rather than half-opened (see `crate::web::commands`).
        Route::NotSupported(reason) => {
            let _ = send_msg(
                sink,
                &ServerMsg::Error(WireError {
                    seq: Some(command.seq),
                    ..WireError::new(ErrorCode::NotSupported, reason)
                }),
            )
            .await;
        }
        // D13: the dialog is app state on both surfaces, so answering it is the
        // TUI's job — it holds the prompt and synthesises the keypress. SPECS
        // §8's configuration manager forwards for the same kind of reason: the
        // two config files and the layer walk live with the TUI's own manager,
        // and this module has no business reading them a second time
        // (`remote-control-1p22`, §6.5 R22).
        Route::Selection(_)
        | Route::ActivityRead
        | Route::Palette(_)
        | Route::Dialog(_)
        | Route::Config => {
            shared.notify(WebInbound::Command {
                viewer_id: viewer_id.clone(),
                label: shared
                    .registry()
                    .label_of(viewer_id)
                    .unwrap_or_else(|| viewer_id.to_string()),
                command,
            });
        }
    }
}

/// Answer one worker command (v7), in a fixed order:
///
/// 1. **Not a worker** → [`ErrorCode::NotSupported`]. A TUI or Desktop has no
///    project picker and no worker shell to restart.
/// 2. **Not the window** → [`ErrorCode::ReadOnly`]: a background link only
///    watches, and a window that released its seat has said it will not drive.
/// 3. **Bad arguments** → [`ErrorCode::NotSupported`] with the parser's reason.
/// 4. **Anything else** → forwarded as [`WebInbound::WorkerOp`]; the host
///    applies it and sends the [`Ack`].
#[allow(clippy::too_many_arguments)]
async fn handle_worker_op(
    shared: &Arc<Shared>,
    viewer_id: &ViewerId,
    seat: Seat,
    kind: ConnectionKind,
    seq: u64,
    parsed: Result<WorkerOp, String>,
    sink: &mut Sink,
) {
    let refusal = if !shared.is_worker() {
        Some(WireError::new(
            ErrorCode::NotSupported,
            "This FlightDeck is not a worker.",
        ))
    } else if kind != ConnectionKind::Window || seat != Seat::Writing {
        Some(WireError::new(
            ErrorCode::ReadOnly,
            "this connection only watches; open the worker's window to drive it",
        ))
    } else {
        match parsed {
            Err(reason) => Some(WireError::new(ErrorCode::NotSupported, reason)),
            Ok(op) => {
                shared.notify(WebInbound::WorkerOp {
                    viewer_id: viewer_id.clone(),
                    seq,
                    op,
                });
                None
            }
        }
    };
    if let Some(error) = refusal {
        let _ = send_msg(
            sink,
            &ServerMsg::Error(WireError {
                seq: Some(seq),
                ..error
            }),
        )
        .await;
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
