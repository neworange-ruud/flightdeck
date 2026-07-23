//! The relay connection thread.
//!
//! A single detached `std::thread` owns a **blocking** [`tungstenite`] WebSocket
//! and runs the full client-side relay-plane state machine:
//!
//! ```text
//! connect → hello → auth_challenge → auth_response → auth_ok
//!         → resume (per pairing, from the last held seq)
//!         → pump: drain outbound / read inbound / periodic ping
//! ```
//!
//! On any drop or fatal frame it reports [`RemoteLinkState::Disconnected`] and
//! reconnects with exponential backoff + jitter (1s..60s). It has no async
//! runtime — deliberately, because the TUI is synchronous.
//!
//! ## Non-blocking reads without async
//!
//! The socket's underlying `TcpStream` gets a ~100 ms `SO_RCVTIMEO`. A read that
//! finds no data returns `WouldBlock`/`TimedOut`, which the pump treats as "idle
//! this tick" — so the same loop can also drain the outbound channel and fire
//! pings roughly every 100 ms, and notice [`RemoteHandle::stop`] promptly. The
//! timeout is set on a `try_clone`d handle *after* the (blocking) handshake, so
//! the upgrade itself is never cut short. tungstenite buffers partial frames, so
//! a mid-frame timeout resumes cleanly on the next read.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tungstenite::{Message, WebSocket};

use crate::contracts::real::RealFs;
use crate::contracts::RemoteConfig;
use crate::remote::state::{load_remote_state, remote_state_path, save_remote_state, RemoteState};
use crate::remote::{DeviceIdentity, RemoteInbound, RemoteOutbound};

use flightdeck_remote_protocol::relay::{
    ClientInfo, EncryptedEnvelope, RelayErrorCode, RelayFrame,
};
use flightdeck_remote_protocol::{DeviceId, PairingId, Role, PROTOCOL_VERSION};

// --- Tuning constants ------------------------------------------------------

/// How long a read blocks before yielding so the pump can also send/stop.
const READ_POLL: Duration = Duration::from_millis(100);
/// Generous timeout for the (blocking) TCP+TLS+WebSocket upgrade.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on a blocking connect so `stop()` is never delayed for long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Write timeout so a wedged socket surfaces as an error, not a hang.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Latency-probe interval.
const PING_INTERVAL: Duration = Duration::from_secs(20);
/// Liveness deadline: tear the session down and reconnect if no inbound frame
/// (Pong, Envelope, Ack — anything) arrives for this long. A half-open socket
/// (laptop sleep/wake, wifi↔cell handoff, relay redeploy, NAT idle-reap) stays
/// "open" with the tiny pings sitting in the kernel send buffer, so
/// [`WRITE_TIMEOUT`] never trips and idle reads loop forever — without this the
/// dead link is never noticed (remote-control-0ef.1). Coordinated with the
/// relay's own server-side idle sweep (both 60s) and a multiple of
/// [`PING_INTERVAL`] so a couple of lost pongs don't cause a spurious teardown.
const LIVENESS_TIMEOUT: Duration = Duration::from_secs(60);
/// Minimum time a session must stay authenticated before a clean drop is allowed
/// to reset the reconnect backoff. A session that reaches `auth_ok` then
/// immediately drops (relay crash/redeploy loop, authed-idle eviction) must NOT
/// reset the backoff to zero, or the client hammers the relay with a ~1s
/// reconnect loop forever (remote-control-0ef.2).
const MIN_STABLE_SESSION: Duration = Duration::from_secs(10);
/// Overall budget for completing the auth handshake before giving up.
const AUTH_DEADLINE: Duration = Duration::from_secs(15);
/// How long a fresh desktop (no persisted pairings) waits after the
/// `auth_challenge` for the app's pending `RequestPairing` to arrive on the
/// outbound channel, so it can offer during the pre-auth window (see
/// [`run_session`]). This closes the startup race where the app loop enqueues
/// the pairing bootstrap a beat after the session thread connects. If nothing
/// arrives in time the client falls back to a plain (offer-less) auth, so a
/// desktop with nothing to offer is never stranded. Kept well under
/// [`AUTH_DEADLINE`].
const PENDING_OFFER_WAIT: Duration = Duration::from_secs(1);

/// Backoff floor (first retry) in milliseconds.
const BACKOFF_BASE_MS: u64 = 1_000;
/// Backoff ceiling in milliseconds.
const BACKOFF_CAP_MS: u64 = 60_000;

/// How many *consecutive* auth rejections of a persisted pairing (the relay
/// answering our auth-first `auth_response` with `auth_failed`/`unknown_pairing`)
/// the supervisor tolerates before self-healing: dropping the stale pairing so
/// the next connect bootstraps a fresh offer instead of looping forever
/// (remote-control-1jy). Only explicit relay rejections count — a transient
/// outage ends the session some other way and resets the streak, so a flapping
/// relay is never mistaken for a wiped one.
const AUTH_REJECT_REOFFER_THRESHOLD: u32 = 3;

// --- Session tuning (test seam) --------------------------------------------

/// Timing knobs threaded through the session, injectable so tests can drive the
/// liveness-teardown (0ef.1) and backoff-reset-stability (0ef.2) logic with short
/// durations instead of real minute-long waits. Production always uses
/// [`ClientTuning::default`] — the real constants — via [`RemoteHandle::start`].
#[derive(Clone)]
struct ClientTuning {
    /// See [`LIVENESS_TIMEOUT`].
    liveness_timeout: Duration,
    /// See [`MIN_STABLE_SESSION`].
    min_stable_session: Duration,
    /// Test seam only: when `> 0`, the next N outbound envelope writes are forced
    /// to fail (the counter is decremented on each) so the failed-write re-send
    /// path (remote-control-0ef.9) can be exercised deterministically, without
    /// relying on OS-specific TCP RST timing. Production passes a zero counter, so
    /// the check never fires. Per-instance (not a global) to avoid contaminating
    /// other tests running in the same process.
    fail_next_envelope_writes: Arc<AtomicU32>,
}

impl Default for ClientTuning {
    fn default() -> Self {
        ClientTuning {
            liveness_timeout: LIVENESS_TIMEOUT,
            min_stable_session: MIN_STABLE_SESSION,
            fail_next_envelope_writes: Arc::new(AtomicU32::new(0)),
        }
    }
}

impl ClientTuning {
    /// Consume one forced-write-failure token, returning `true` when a write
    /// should be treated as failed. A no-op (always `false`) in production, where
    /// the counter is zero.
    fn take_forced_write_failure(&self) -> bool {
        self.fail_next_envelope_writes
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }
}

// --- Public link state -----------------------------------------------------

/// The relay connection state, pushed to the app over `RemoteInbound::Link`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteLinkState {
    /// Not connected (idle, or between reconnect attempts).
    Disconnected,
    /// A connection attempt / handshake is in progress.
    Connecting,
    /// Authenticated and live. `latency_ms` is the last measured round-trip to
    /// the relay (0 until the first pong).
    Connected {
        /// Last measured phone↔relay round-trip in milliseconds.
        latency_ms: u64,
    },
    /// The relay speaks a protocol version incompatible with this build — a
    /// **terminal** state: the client stops reconnecting (retrying can never
    /// succeed until the app is updated) instead of silently backoff-looping
    /// forever, so the UI can surface an actionable "update FlightDeck" prompt
    /// rather than an endless "reconnecting" (remote-control-0ef.20).
    Incompatible {
        /// The protocol version this build offered.
        our_version: u16,
        /// Oldest version the relay supports.
        relay_min: u16,
        /// Newest version the relay supports.
        relay_max: u16,
    },
}

// --- Persistence seam (so tests never touch ~/.flightdeck) -----------------

/// Where the client loads/saves its [`RemoteState`] (pairings + cursors). The
/// production impl uses the real `~/.flightdeck/remote.json`; tests inject an
/// in-memory store.
pub trait RemoteStore: Send {
    /// Load the current state (or a default on any error).
    fn load(&self) -> RemoteState;
    /// Persist the state (best-effort; errors are swallowed).
    fn save(&self, state: &RemoteState);
}

/// The production [`RemoteStore`], backed by `~/.flightdeck/remote.json`.
pub struct FileRemoteStore {
    path: Option<std::path::PathBuf>,
}

impl FileRemoteStore {
    /// A store at the default per-user path.
    pub fn new() -> Self {
        FileRemoteStore {
            path: remote_state_path(),
        }
    }
}

impl Default for FileRemoteStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteStore for FileRemoteStore {
    fn load(&self) -> RemoteState {
        match &self.path {
            Some(p) => load_remote_state(&RealFs, p).unwrap_or_default(),
            None => RemoteState::default(),
        }
    }
    fn save(&self, state: &RemoteState) {
        if let Some(p) = &self.path {
            let _ = save_remote_state(&RealFs, p, state);
        }
    }
}

// --- Handle ----------------------------------------------------------------

/// A running relay client. Dropping it (or calling [`Self::stop`]) tears the
/// connection thread down.
pub struct RemoteHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl RemoteHandle {
    /// Start the relay client against the default file-backed store.
    pub fn start(
        cfg: RemoteConfig,
        identity: DeviceIdentity,
        inbound_tx: Sender<RemoteInbound>,
        outbound_rx: Receiver<RemoteOutbound>,
    ) -> RemoteHandle {
        Self::start_with_store(
            cfg,
            identity,
            Box::new(FileRemoteStore::new()),
            inbound_tx,
            outbound_rx,
        )
    }

    /// Start with an explicit [`RemoteStore`] (dependency injection for tests).
    pub fn start_with_store(
        cfg: RemoteConfig,
        identity: DeviceIdentity,
        store: Box<dyn RemoteStore>,
        inbound_tx: Sender<RemoteInbound>,
        outbound_rx: Receiver<RemoteOutbound>,
    ) -> RemoteHandle {
        Self::start_tuned(
            cfg,
            identity,
            store,
            inbound_tx,
            outbound_rx,
            ClientTuning::default(),
        )
    }

    /// Start with an explicit [`RemoteStore`] and [`ClientTuning`]. The tuning
    /// lets tests drive liveness/stability logic with short durations and force
    /// write failures; production uses [`start_with_store`](Self::start_with_store).
    fn start_tuned(
        cfg: RemoteConfig,
        identity: DeviceIdentity,
        store: Box<dyn RemoteStore>,
        inbound_tx: Sender<RemoteInbound>,
        outbound_rx: Receiver<RemoteOutbound>,
        tuning: ClientTuning,
    ) -> RemoteHandle {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("flightdeck-remote".to_string())
            .spawn(move || {
                run(
                    cfg,
                    identity,
                    store,
                    inbound_tx,
                    outbound_rx,
                    stop_thread,
                    tuning,
                );
            })
            .ok();
        RemoteHandle { stop, join }
    }

    /// Signal the thread to shut down and wait for it to finish.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for RemoteHandle {
    fn drop(&mut self) {
        // If stop() was not called, at least signal the thread so it winds down
        // rather than holding the socket open past app exit.
        self.stop.store(true, Ordering::Relaxed);
    }
}

// --- Backoff (pure, unit-tested) -------------------------------------------

/// Backoff for retry `attempt` (0 = first retry). Exponential from
/// [`BACKOFF_BASE_MS`], capped at [`BACKOFF_CAP_MS`], plus up to +25% jitter.
/// `jitter_unit` is a value in `[0, 1)`; the delay always stays within
/// `[1s, 60s]`.
fn backoff_delay(attempt: u32, jitter_unit: f64) -> Duration {
    // Cap the shift so `1_000 << attempt` never overflows.
    let shift = attempt.min(6);
    let full = (BACKOFF_BASE_MS << shift).min(BACKOFF_CAP_MS);
    let jitter = (jitter_unit.clamp(0.0, 1.0) * (full as f64) * 0.25) as u64;
    Duration::from_millis((full + jitter).min(BACKOFF_CAP_MS))
}

/// A uniform value in `[0, 1)` from the OS CSPRNG, for backoff jitter.
fn jitter_unit() -> f64 {
    use rand_core::RngCore;
    let mut buf = [0u8; 8];
    rand_core::OsRng.fill_bytes(&mut buf);
    (u64::from_le_bytes(buf) as f64) / (u64::MAX as f64 + 1.0)
}

// --- Wall clock ------------------------------------------------------------

/// Wall-clock unix milliseconds (for envelope `sent_at_ms` and ping timing).
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn client_info() -> ClientInfo {
    ClientInfo {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: if cfg!(target_os = "macos") {
            "macos".to_string()
        } else if cfg!(windows) {
            "windows".to_string()
        } else {
            "linux".to_string()
        },
        os_version: None,
    }
}

// --- Socket abstraction (plain ws + optional wss) --------------------------

/// A connected relay socket. `wss` (rustls) is available on every platform
/// except Windows, mirroring the self-update crypto gating that keeps the
/// windows-msvc binary pure-Rust; Windows gets plain `ws://` only.
enum RelaySocket {
    Plain(Box<WebSocket<TcpStream>>),
    #[cfg(not(windows))]
    Tls(Box<WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>>),
}

impl RelaySocket {
    // tungstenite's `Error` is a deliberately large enum; propagating its
    // `Result` here is unavoidable, so silence `result_large_err`.
    #[allow(clippy::result_large_err)]
    fn read(&mut self) -> tungstenite::Result<Message> {
        match self {
            RelaySocket::Plain(ws) => ws.read(),
            #[cfg(not(windows))]
            RelaySocket::Tls(ws) => ws.read(),
        }
    }

    #[allow(clippy::result_large_err)]
    fn send(&mut self, msg: Message) -> tungstenite::Result<()> {
        match self {
            RelaySocket::Plain(ws) => ws.send(msg),
            #[cfg(not(windows))]
            RelaySocket::Tls(ws) => ws.send(msg),
        }
    }

    fn close(&mut self) {
        match self {
            RelaySocket::Plain(ws) => {
                let _ = ws.close(None);
            }
            #[cfg(not(windows))]
            RelaySocket::Tls(ws) => {
                let _ = ws.close(None);
            }
        }
    }

    /// Set the read timeout on the *actual* TCP socket tungstenite reads from.
    ///
    /// This must go through the live stream handle rather than a `try_clone()`d
    /// descriptor: `SO_RCVTIMEO` is shared across dup'd descriptors on Unix but
    /// not across a Windows `WSADuplicateSocket` handle, so retiming a clone left
    /// the pump reading at the 10s handshake timeout on Windows — making dropped
    /// connections take ~10s to notice and reconnects miss their deadline.
    fn set_read_timeout(&self, dur: Duration) {
        let _ = match self {
            RelaySocket::Plain(ws) => ws.get_ref().set_read_timeout(Some(dur)),
            #[cfg(not(windows))]
            RelaySocket::Tls(ws) => match ws.get_ref() {
                tungstenite::stream::MaybeTlsStream::Plain(s) => s.set_read_timeout(Some(dur)),
                tungstenite::stream::MaybeTlsStream::Rustls(s) => {
                    s.sock.set_read_timeout(Some(dur))
                }
                _ => Ok(()),
            },
        };
    }
}

/// Serialize a relay frame and write it as a WebSocket text message.
#[allow(clippy::result_large_err)]
fn send_frame(sock: &mut RelaySocket, frame: &RelayFrame) -> tungstenite::Result<()> {
    let json = serde_json::to_string(frame).expect("relay frame serializes");
    sock.send(Message::Text(json))
}

/// The outcome of one read attempt on the socket.
enum Incoming {
    /// A parsed relay frame.
    Frame(Box<RelayFrame>),
    /// No data within the poll timeout (or a control frame we ignore).
    Idle,
    /// The socket closed or errored — the connection is over.
    Closed,
}

fn is_would_block(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// Read one frame with the pump's poll timeout. Unknown/malformed text and
/// control frames are reported as [`Incoming::Idle`] so the pump keeps going.
fn read_frame(sock: &mut RelaySocket) -> Incoming {
    match sock.read() {
        Ok(Message::Text(s)) => match serde_json::from_str::<RelayFrame>(&s) {
            Ok(frame) => Incoming::Frame(Box::new(frame)),
            Err(_) => Incoming::Idle,
        },
        Ok(Message::Close(_)) => Incoming::Closed,
        Ok(_) => Incoming::Idle, // binary/ping/pong/raw — ignore (auto-pong handled)
        Err(tungstenite::Error::Io(e)) if is_would_block(&e) => Incoming::Idle,
        Err(_) => Incoming::Closed,
    }
}

// --- Connect ---------------------------------------------------------------

/// Resolve and open the relay socket, performing the (blocking) WebSocket
/// upgrade, then tighten the read timeout for the pump loop.
fn connect(url: &str) -> Result<RelaySocket, String> {
    use tungstenite::client::IntoClientRequest;

    let request = url
        .into_client_request()
        .map_err(|e| format!("bad relay url: {e}"))?;
    let uri = request.uri();
    let secure = uri
        .scheme_str()
        .map(|s| s.eq_ignore_ascii_case("wss"))
        .unwrap_or(false);
    let host = uri
        .host()
        .ok_or_else(|| "relay url has no host".to_string())?
        .to_string();
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });

    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("dns resolution failed: {e}"))?
        .next()
        .ok_or_else(|| "relay host resolved to no address".to_string())?;

    let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| format!("tcp connect failed: {e}"))?;
    // Generous read timeout for the (blocking) WebSocket upgrade; tightened to
    // the pump's poll cadence on the live socket once the handshake completes.
    tcp.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).ok();
    tcp.set_write_timeout(Some(WRITE_TIMEOUT)).ok();

    let sock = if secure {
        #[cfg(not(windows))]
        {
            let (ws, _resp) =
                tungstenite::client_tls(request, tcp).map_err(|e| format!("tls upgrade: {e}"))?;
            RelaySocket::Tls(Box::new(ws))
        }
        #[cfg(windows)]
        {
            return Err("wss is not supported on this build (use ws:// for local dev)".to_string());
        }
    } else {
        let (ws, _resp) =
            tungstenite::client(request, tcp).map_err(|e| format!("ws upgrade: {e}"))?;
        RelaySocket::Plain(Box::new(ws))
    };

    sock.set_read_timeout(READ_POLL);
    Ok(sock)
}

// --- The thread body -------------------------------------------------------

/// Why a single connection session ended.
enum SessionEnd {
    /// `stop()` was requested; do not reconnect.
    Stopped,
    /// The session ended; reconnect. `authed_for` is `Some(duration)` if we
    /// reached `auth_ok` (carrying how long we then stayed authenticated) or
    /// `None` if we never authenticated. Only a session that stayed authed for at
    /// least [`ClientTuning::min_stable_session`] resets the reconnect backoff
    /// (remote-control-0ef.2). `pending` carries an outbound envelope whose write
    /// failed mid-session, to be re-sent first on the next session so its `seq` is
    /// never skipped on the wire (remote-control-0ef.9).
    Ended {
        /// How long the session stayed authenticated, or `None` if it never did.
        authed_for: Option<Duration>,
        /// An in-flight envelope to re-send on the next session (0ef.9).
        pending: Option<RemoteOutbound>,
    },
    /// The relay explicitly rejected our auth-first `auth_response` for a
    /// persisted pairing (`auth_failed`/`unknown_pairing`) — it no longer knows
    /// this device/pairing. Distinct from a transient [`Self::Ended`] drop so
    /// the supervisor can self-heal after repeated rejections rather than loop
    /// forever on a dead pairing (remote-control-1jy).
    AuthRejected,
    /// The relay speaks a protocol version outside this build's supported range.
    /// Terminal: the supervisor reports [`RemoteLinkState::Incompatible`] and
    /// stops reconnecting (remote-control-0ef.20).
    VersionIncompatible {
        /// The version this build offered.
        our_version: u16,
        /// Oldest version the relay supports.
        relay_min: u16,
        /// Newest version the relay supports.
        relay_max: u16,
    },
}

/// A session that never authenticated: reconnect without resetting backoff and
/// with nothing to re-send.
fn ended_unauthed() -> SessionEnd {
    SessionEnd::Ended {
        authed_for: None,
        pending: None,
    }
}

/// Whether a just-ended session justifies resetting the reconnect backoff to
/// zero. Only a session that reached `auth_ok` **and** then stayed authenticated
/// for at least `min_stable` counts as healthy; a post-auth flap does not, so a
/// crash/redeploy loop keeps growing its backoff instead of hammering the relay
/// ~once/second (remote-control-0ef.2).
fn session_resets_backoff(authed_for: Option<Duration>, min_stable: Duration) -> bool {
    matches!(authed_for, Some(d) if d >= min_stable)
}

fn report(inbound_tx: &Sender<RemoteInbound>, state: RemoteLinkState) {
    let _ = inbound_tx.send(RemoteInbound::Link(state));
}

/// The reconnect supervisor: attempt after attempt with backoff until stopped.
fn run(
    cfg: RemoteConfig,
    identity: DeviceIdentity,
    store: Box<dyn RemoteStore>,
    inbound_tx: Sender<RemoteInbound>,
    outbound_rx: Receiver<RemoteOutbound>,
    stop: Arc<AtomicBool>,
    tuning: ClientTuning,
) {
    let mut attempt: u32 = 0;
    // Consecutive auth rejections of our persisted pairing (see
    // [`AUTH_REJECT_REOFFER_THRESHOLD`]). Any non-rejection outcome resets it.
    let mut auth_reject_streak: u32 = 0;
    // An outbound envelope whose write failed on the previous session, to re-send
    // first on the next one so its `seq` is not skipped on the wire (0ef.9).
    let mut pending: Option<RemoteOutbound> = None;
    // Keep persisted state authoritative for the private key regardless of what
    // was on disk when the thread started.
    let mut state = store.load();
    state.device_private_key = identity.private_key_base64();

    while !stop.load(Ordering::Relaxed) {
        report(&inbound_tx, RemoteLinkState::Connecting);
        let end = run_session(
            &cfg,
            &identity,
            &mut state,
            store.as_ref(),
            &inbound_tx,
            &outbound_rx,
            &stop,
            &tuning,
            pending.take(),
        );

        match end {
            SessionEnd::Stopped => {
                report(&inbound_tx, RemoteLinkState::Disconnected);
                break;
            }
            SessionEnd::VersionIncompatible {
                our_version,
                relay_min,
                relay_max,
            } => {
                // Terminal: retrying can never succeed until the app is updated,
                // so surface an actionable state and stop reconnecting rather than
                // backoff-loop forever in silence (0ef.20).
                eprintln!(
                    "flightdeck-remote: relay protocol version incompatible \
                     (we offer v{our_version}, relay supports v{relay_min}..=v{relay_max}); \
                     update FlightDeck. Not reconnecting."
                );
                report(
                    &inbound_tx,
                    RemoteLinkState::Incompatible {
                        our_version,
                        relay_min,
                        relay_max,
                    },
                );
                break;
            }
            SessionEnd::Ended {
                authed_for,
                pending: p,
            } => {
                // Carry any failed-write envelope into the next session (0ef.9).
                pending = p;
                report(&inbound_tx, RemoteLinkState::Disconnected);
                // A successful (or merely dropped) session breaks any rejection
                // streak — the relay is not persistently rejecting us.
                auth_reject_streak = 0;
                // Only a session that stayed healthily authenticated resets the
                // backoff; a post-auth flap keeps it growing (0ef.2).
                attempt = if session_resets_backoff(authed_for, tuning.min_stable_session) {
                    0
                } else {
                    attempt.saturating_add(1)
                };
            }
            SessionEnd::AuthRejected => {
                report(&inbound_tx, RemoteLinkState::Disconnected);
                auth_reject_streak = auth_reject_streak.saturating_add(1);
                attempt = attempt.saturating_add(1);
                if auth_reject_streak >= AUTH_REJECT_REOFFER_THRESHOLD {
                    // The relay has rejected our persisted pairing on every one
                    // of the last N connects — it no longer knows it (its store
                    // was almost certainly wiped). Self-heal: drop the stale
                    // pairing(s) so the next connect is a clean offer-first
                    // bootstrap, and tell the app so it can surface a re-pair
                    // prompt instead of an endless "reconnecting" (1jy).
                    let dropped: Vec<PairingId> = state
                        .pairing_ids()
                        .into_iter()
                        .map(PairingId::new)
                        .collect();
                    state.pairings.clear();
                    store.save(&state);
                    eprintln!(
                        "flightdeck-remote: relay rejected our pairing \
                         {AUTH_REJECT_REOFFER_THRESHOLD}x (no longer recognized); \
                         dropped {} stale pairing(s), will re-offer on next connect",
                        dropped.len()
                    );
                    let _ = inbound_tx.send(RemoteInbound::PairingRejected {
                        pairing_ids: dropped,
                    });
                    auth_reject_streak = 0;
                    attempt = 0;
                }
            }
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        interruptible_sleep(backoff_delay(attempt, jitter_unit()), &stop);
    }
}

/// Sleep up to `dur`, waking early (within ~100 ms) if `stop` is set.
fn interruptible_sleep(dur: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The effective relay URL: a per-device `remote.json` override wins over config.
fn effective_url(cfg: &RemoteConfig, state: &RemoteState) -> String {
    match &state.relay_url {
        Some(u) if !u.is_empty() => u.clone(),
        _ => cfg.relay_url.clone(),
    }
}

/// One connection session: connect, authenticate, resume, then pump.
#[allow(clippy::too_many_arguments)]
fn run_session(
    cfg: &RemoteConfig,
    identity: &DeviceIdentity,
    state: &mut RemoteState,
    store: &dyn RemoteStore,
    inbound_tx: &Sender<RemoteInbound>,
    outbound_rx: &Receiver<RemoteOutbound>,
    stop: &AtomicBool,
    tuning: &ClientTuning,
    pending_in: Option<RemoteOutbound>,
) -> SessionEnd {
    let url = effective_url(cfg, state);
    let mut sock = match connect(&url) {
        Ok(s) => s,
        Err(e) => {
            // Expose the connect-error detail for diagnostics instead of
            // silently discarding it — a bad relay URL / DNS failure otherwise
            // retries forever with the user seeing only "reconnecting" and zero
            // signal about why (remote-control-0ef.20).
            crate::remote::debuglog::log(&format!("client CONNECT failed url={url} err={e}"));
            eprintln!("flightdeck-remote: connect to {url} failed: {e}");
            return ended_unauthed();
        }
    };

    // hello.
    let hello = RelayFrame::Hello {
        protocol_version: PROTOCOL_VERSION,
        role: Role::Desktop,
        device_id: DeviceId::new(identity.device_id()),
        client: client_info(),
    };
    if send_frame(&mut sock, &hello).is_err() {
        return ended_unauthed();
    }

    // Drive hello_ok → auth_challenge → auth_response → auth_ok under a deadline.
    //
    // ## Offer-first bootstrap (spec §5.2)
    //
    // The relay's designed desktop bootstrap is *offer-first*: a pre-auth
    // `pairing_offer` self-registers this device's identity + key-agreement keys
    // (remote/relay `session.rs::on_pre_auth` / `on_pairing_offer`), and only
    // then will `on_auth_response` verify it — an unregistered device is
    // rejected with `AuthFailed "unknown device"` (session.rs ~:583). So a fresh
    // desktop (no persisted pairings) that the relay has never seen *cannot*
    // authenticate until it has offered. The relay's own `TestClient` proves the
    // sequence: `offer_pairing()` THEN `authenticate(vec![pairing_id])`.
    //
    // We therefore split the challenge response by whether a bootstrap is needed:
    //   - **Returning desktop** (persisted pairings): answer the challenge
    //     immediately with those pairing ids — auth-first, exactly as before, so
    //     reconnects never mint a spurious offer.
    //   - **Fresh desktop** (no pairings): defer the answer and watch the
    //     outbound channel for the app's pending `RequestPairing`. When it
    //     arrives, send the `pairing_offer` (registering the device), consume the
    //     `pairing_offer_ok` (surfacing the overlay code via `PairingOffered` and
    //     learning the new pairing id), and only then answer the challenge
    //     including that id. If no request arrives within `PENDING_OFFER_WAIT`,
    //     fall back to a plain auth so an idle desktop — and the offer-less
    //     mock-relay tests — behave as before.
    let deadline = Instant::now() + AUTH_DEADLINE;
    let mut saw_hello_ok = false;
    // The challenge nonce, captured until we decide to answer it.
    let mut challenge_nonce: Option<String> = None;
    // Whether we have already sent our `auth_response`.
    let mut sent_auth = false;
    // Fresh-desktop bootstrap bookkeeping.
    let mut offer_sent = false;
    let mut offer_wait_until: Option<Instant> = None;
    // Whether we answered the challenge auth-first as a returning desktop (i.e.
    // with persisted pairing ids, no pre-auth offer). Gates treating a relay
    // `auth_failed` as a pairing rejection worth self-healing (1jy) — a fresh
    // desktop's offer path is not a "stale pairing the relay forgot".
    let mut auth_first = false;
    // Outbound messages pulled off the queue while waiting to offer that are not
    // the `RequestPairing` we want; replayed once we reach the pump. Normally
    // empty — only `RequestPairing` is expected before a pairing exists.
    let mut deferred: Vec<RemoteOutbound> = Vec::new();

    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = send_frame(&mut sock, &RelayFrame::Bye { reason: None });
            sock.close();
            return SessionEnd::Stopped;
        }
        if Instant::now() > deadline {
            return ended_unauthed();
        }

        // Fresh-desktop pre-auth window: watch for the pending pairing request so
        // we can offer before authing (or fall back once the wait lapses).
        if let (Some(nonce), Some(wait_until)) = (challenge_nonce.as_ref(), offer_wait_until) {
            if !offer_sent && !sent_auth {
                match outbound_rx.try_recv() {
                    Ok(RemoteOutbound::RequestPairing { claim_token_hint }) => {
                        let offer = build_pairing_offer(identity, claim_token_hint);
                        if send_frame(&mut sock, &offer).is_err() {
                            return ended_unauthed();
                        }
                        offer_sent = true;
                    }
                    Ok(other) => deferred.push(other),
                    Err(TryRecvError::Empty) => {
                        if Instant::now() >= wait_until {
                            if !send_auth_response(&mut sock, identity, nonce, state) {
                                return ended_unauthed();
                            }
                            sent_auth = true;
                        }
                    }
                    Err(TryRecvError::Disconnected) => {
                        // The app dropped its sender (shutting down).
                        let _ = send_frame(&mut sock, &RelayFrame::Bye { reason: None });
                        sock.close();
                        return SessionEnd::Stopped;
                    }
                }
            }
        }

        match read_frame(&mut sock) {
            Incoming::Idle => continue,
            Incoming::Closed => return ended_unauthed(),
            Incoming::Frame(frame) => match *frame {
                RelayFrame::HelloOk { .. } => saw_hello_ok = true,
                RelayFrame::VersionIncompatible {
                    your_version,
                    min_supported,
                    max_supported,
                } => {
                    // Terminal condition (0ef.20): the relay's supported range
                    // does not include our version, so reconnecting can never
                    // succeed until the app updates. Surface it distinctly rather
                    // than treating it as a transient drop that backoff-loops.
                    return SessionEnd::VersionIncompatible {
                        our_version: your_version,
                        relay_min: min_supported,
                        relay_max: max_supported,
                    };
                }
                RelayFrame::AuthChallenge { nonce, .. }
                    if saw_hello_ok && challenge_nonce.is_none() =>
                {
                    if state.pairing_ids().is_empty() {
                        // Fresh desktop: defer auth until we have offered (or the
                        // pending-offer wait lapses above).
                        offer_wait_until = Some(Instant::now() + PENDING_OFFER_WAIT);
                        challenge_nonce = Some(nonce);
                    } else {
                        // Returning desktop: auth-first, exactly as before.
                        if !send_auth_response(&mut sock, identity, &nonce, state) {
                            return ended_unauthed();
                        }
                        sent_auth = true;
                        auth_first = true;
                        challenge_nonce = Some(nonce);
                    }
                }
                RelayFrame::PairingOfferOk {
                    pairing_id,
                    claim_token,
                    expires_at_ms,
                } if offer_sent && !sent_auth => {
                    // The pre-auth offer registered our device and minted the
                    // pairing; surface the code, then auth including the new id.
                    persist_pairing_offer(
                        state,
                        store,
                        inbound_tx,
                        pairing_id,
                        claim_token,
                        expires_at_ms,
                    );
                    match challenge_nonce.as_ref() {
                        Some(nonce) => {
                            if !send_auth_response(&mut sock, identity, nonce, state) {
                                return ended_unauthed();
                            }
                            sent_auth = true;
                        }
                        None => return ended_unauthed(),
                    }
                }
                RelayFrame::AuthOk { pairing_ids } if sent_auth => {
                    on_authenticated(&mut sock, state, inbound_tx, pairing_ids);
                    break;
                }
                RelayFrame::Error { code, .. } => {
                    // A returning desktop that authed-first and got rejected: the
                    // relay does not recognize our device/pairing (its store was
                    // likely wiped). Surface it as a distinct end so the
                    // supervisor can self-heal after repeated rejections instead
                    // of reconnecting on a dead pairing forever (1jy). Any other
                    // error (or a fresh-desktop offer failure) stays a plain end.
                    if auth_first
                        && matches!(
                            code,
                            RelayErrorCode::AuthFailed | RelayErrorCode::UnknownPairing
                        )
                    {
                        return SessionEnd::AuthRejected;
                    }
                    return ended_unauthed();
                }
                _ => continue, // unexpected pre-auth frame; ignore
            },
        }
    }

    // From here on we are authenticated; measure how long we stay up so a
    // sub-threshold flap does not reset the reconnect backoff (0ef.2).
    let authed_at = Instant::now();

    // Re-send an envelope whose write failed on the previous session BEFORE any
    // freshly-queued traffic, so its `seq` slots back into the stream contiguously
    // and the phone's dedup never stalls on a gap (0ef.9). If the write fails
    // again, hold it once more for the next session.
    if let Some(out) = pending_in {
        if let Sent::Broke { retry } =
            handle_outbound(&mut sock, identity, state, store, tuning, out)
        {
            return SessionEnd::Ended {
                authed_for: Some(authed_at.elapsed()),
                pending: retry,
            };
        }
    }

    // Replay anything the app queued during the pre-auth offer wait before the
    // steady-state pump takes over (normally nothing).
    for out in deferred {
        if let Sent::Broke { retry } =
            handle_outbound(&mut sock, identity, state, store, tuning, out)
        {
            return SessionEnd::Ended {
                authed_for: Some(authed_at.elapsed()),
                pending: retry,
            };
        }
    }

    // Authenticated. Pump until the socket drops or we are told to stop.
    pump(
        &mut sock,
        identity,
        state,
        store,
        inbound_tx,
        outbound_rx,
        stop,
        tuning,
        authed_at,
    )
}

/// Sign `nonce_b64` and send the `auth_response`, activating whatever pairings
/// the persisted state currently holds (empty for an offer-less fresh desktop,
/// or including a just-offered pairing once its `pairing_offer_ok` landed).
/// Returns `false` if signing or the socket write failed.
fn send_auth_response(
    sock: &mut RelaySocket,
    identity: &DeviceIdentity,
    nonce_b64: &str,
    state: &RemoteState,
) -> bool {
    let signature = match identity.sign_nonce_base64(nonce_b64) {
        Ok(sig) => sig,
        Err(_) => return false,
    };
    let resp = RelayFrame::AuthResponse {
        device_id: DeviceId::new(identity.device_id()),
        signature,
        pairing_ids: state
            .pairing_ids()
            .into_iter()
            .map(PairingId::new)
            .collect(),
        // Announce this Mac's display name on every connect so the phone's
        // per-pairing default auto-updates when the machine is renamed
        // (spec §10.1). Computed fresh each connect — never cached — so a rename
        // propagates on the next reconnect.
        machine_name: machine_name(),
    };
    send_frame(sock, &resp).is_ok()
}

/// This desktop's human-readable machine name for the phone's feed (spec §10.1).
///
/// Source order: an explicit `FLIGHTDECK_MACHINE_NAME` override (the "configured
/// display name" escape hatch), then the system hostname (via the `hostname`
/// command, which exists on macOS/Linux/Windows), then the `HOSTNAME` /
/// `COMPUTERNAME` env vars. Returns `None` if nothing is resolvable, in which
/// case the frame carries `null` and the phone keeps its previous/fallback name.
/// The result is length-bounded to 64 characters; the relay bounds it again and
/// the phone sanitizes it before display.
fn machine_name() -> Option<String> {
    fn clean(raw: &str) -> Option<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.chars().take(64).collect())
        }
    }

    if let Some(name) =
        std::env::var_os("FLIGHTDECK_MACHINE_NAME").and_then(|v| clean(&v.to_string_lossy()))
    {
        return Some(name);
    }
    if let Ok(out) = std::process::Command::new("hostname").output() {
        if out.status.success() {
            if let Some(name) = clean(&String::from_utf8_lossy(&out.stdout)) {
                return Some(name);
            }
        }
    }
    for var in ["HOSTNAME", "COMPUTERNAME"] {
        if let Some(name) = std::env::var_os(var).and_then(|v| clean(&v.to_string_lossy())) {
            return Some(name);
        }
    }
    None
}

/// Build the desktop's `pairing_offer` (spec §5.2). The desktop reuses its
/// identity key as its key-agreement key (its keystore key is usable for ECDH),
/// so both public keys are the same X9.63 point — one less key to manage. The
/// relay honors a free 4-digit `claim_token_hint`. Shared by the pre-auth
/// bootstrap in [`run_session`] and the post-auth [`handle_outbound`] path.
fn build_pairing_offer(identity: &DeviceIdentity, claim_token_hint: Option<String>) -> RelayFrame {
    let public_key = identity.public_key_base64();
    RelayFrame::PairingOffer {
        device_id: DeviceId::new(identity.device_id()),
        device_public_key: public_key.clone(),
        key_agreement_public_key: public_key,
        role: Role::Desktop,
        claim_token_hint,
    }
}

/// Record a `pairing_offer_ok`: persist the pairing so it is activated on the
/// next connect and store the claim token (its bytes are the E2E salt, spec
/// §7.1), then surface the code to the app via [`RemoteInbound::PairingOffered`]
/// (drives the overlay). Shared by the pre-auth bootstrap in [`run_session`] and
/// the post-auth [`handle_frame`] path.
fn persist_pairing_offer(
    state: &mut RemoteState,
    store: &dyn RemoteStore,
    inbound_tx: &Sender<RemoteInbound>,
    pairing_id: PairingId,
    claim_token: String,
    expires_at_ms: i64,
) {
    let key = pairing_id.as_str().to_string();
    if state.pairing(&key).is_none() {
        state
            .pairings
            .push(crate::remote::Pairing::new(key.clone()));
    }
    if let Some(p) = state.pairing_mut(&key) {
        p.claim_token = Some(claim_token.clone());
    }
    store.save(state);
    let _ = inbound_tx.send(RemoteInbound::PairingOffered {
        pairing_id,
        claim_token,
        expires_at_ms,
    });
}

/// After `auth_ok`: report Connected, then `resume` each active pairing from the
/// highest seq we already hold, and surface the pairings to the app.
fn on_authenticated(
    sock: &mut RelaySocket,
    state: &RemoteState,
    inbound_tx: &Sender<RemoteInbound>,
    pairing_ids: Vec<PairingId>,
) {
    report(inbound_tx, RemoteLinkState::Connected { latency_ms: 0 });
    for pid in pairing_ids {
        let from_seq = state
            .pairing(pid.as_str())
            .map(|p| p.last_received_seq)
            .unwrap_or(0);
        let _ = send_frame(
            sock,
            &RelayFrame::Resume {
                pairing_id: pid.clone(),
                from_seq,
            },
        );
        // Only surface `Paired` — which drives the outbound bridge to send a
        // fresh snapshot — for a pairing whose phone has already joined (i.e. an
        // *established* one, so the E2E channel is live and the snapshot is
        // sealed to the peer). A freshly-offered pairing (this happens right
        // after the pre-auth bootstrap above, since the relay activates the new
        // pairing in `auth_ok`) has no peer and only the passthrough sealer:
        // snapshotting it now would enqueue an unopenable envelope and burn
        // seq 1 before the real channel is derived on `pairing_claimed`, which
        // the relay would then reject as a non-monotonic seq. Such a pairing
        // reaches the bridge later via `PairingClaimed` instead.
        if state
            .pairing(pid.as_str())
            .map(|p| p.established)
            .unwrap_or(false)
        {
            let peer_device_id = state
                .pairing(pid.as_str())
                .and_then(|p| p.peer_device_id.clone())
                .map(DeviceId::new);
            let _ = inbound_tx.send(RemoteInbound::Paired {
                pairing_id: pid,
                peer_device_id,
            });
        }
    }
}

/// The outcome of handing one app→relay message to the socket.
enum Sent {
    /// Delivered (or applied locally); the session continues.
    Ok,
    /// The socket broke while sending; the session must end. `retry` carries the
    /// envelope that failed to write (a `SendEnvelope`), so the supervisor can
    /// re-send it on the next session and never skip its `seq` on the wire
    /// (remote-control-0ef.9). `None` for a non-envelope send failure.
    Broke { retry: Option<RemoteOutbound> },
}

/// The steady-state loop: drain outbound, fire pings, read inbound frames.
#[allow(clippy::too_many_arguments)]
fn pump(
    sock: &mut RelaySocket,
    identity: &DeviceIdentity,
    state: &mut RemoteState,
    store: &dyn RemoteStore,
    inbound_tx: &Sender<RemoteInbound>,
    outbound_rx: &Receiver<RemoteOutbound>,
    stop: &AtomicBool,
    tuning: &ClientTuning,
    authed_at: Instant,
) -> SessionEnd {
    let mut last_ping = Instant::now();
    // Last time ANY inbound frame arrived (Pong, Envelope, Ack, presence …). A
    // half-open socket delivers nothing yet never errors on our tiny pinging, so
    // we tear the session down once this exceeds the liveness deadline instead of
    // looping on idle reads forever (remote-control-0ef.1). Seeded at auth so a
    // silent socket is caught even if the very first frame never arrives.
    let mut last_inbound = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = send_frame(sock, &RelayFrame::Bye { reason: None });
            sock.close();
            return SessionEnd::Stopped;
        }

        // Half-open detection: no inbound frame for the liveness window → the link
        // is silently dead; tear it down so the supervisor reconnects (0ef.1).
        if last_inbound.elapsed() >= tuning.liveness_timeout {
            crate::remote::debuglog::log(&format!(
                "client LIVENESS timeout ({}s) — tearing down half-open session",
                tuning.liveness_timeout.as_secs()
            ));
            return SessionEnd::Ended {
                authed_for: Some(authed_at.elapsed()),
                pending: None,
            };
        }

        // Drain everything the app queued for us.
        loop {
            match outbound_rx.try_recv() {
                Ok(out) => {
                    if let Sent::Broke { retry } =
                        handle_outbound(sock, identity, state, store, tuning, out)
                    {
                        return SessionEnd::Ended {
                            authed_for: Some(authed_at.elapsed()),
                            pending: retry,
                        };
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The app dropped its sender (shutting down).
                    let _ = send_frame(sock, &RelayFrame::Bye { reason: None });
                    sock.close();
                    return SessionEnd::Stopped;
                }
            }
        }

        if last_ping.elapsed() >= PING_INTERVAL {
            let _ = send_frame(
                sock,
                &RelayFrame::Ping {
                    client_time_ms: now_ms(),
                },
            );
            last_ping = Instant::now();
        }

        match read_frame(sock) {
            Incoming::Idle => {}
            Incoming::Closed => {
                return SessionEnd::Ended {
                    authed_for: Some(authed_at.elapsed()),
                    pending: None,
                }
            }
            Incoming::Frame(frame) => {
                // Any inbound frame proves the link is alive — reset the deadline.
                last_inbound = Instant::now();
                if !handle_frame(sock, state, store, inbound_tx, *frame) {
                    return SessionEnd::Ended {
                        authed_for: Some(authed_at.elapsed()),
                        pending: None,
                    };
                }
            }
        }
    }
}

/// Handle one app→relay message. Returns [`Sent::Broke`] if the socket broke.
fn handle_outbound(
    sock: &mut RelaySocket,
    identity: &DeviceIdentity,
    state: &mut RemoteState,
    store: &dyn RemoteStore,
    tuning: &ClientTuning,
    out: RemoteOutbound,
) -> Sent {
    match out {
        RemoteOutbound::SendEnvelope {
            pairing_id,
            seq,
            sent_at_ms,
            nonce,
            ciphertext,
        } => {
            let key = pairing_id.as_str().to_string();
            // Ensure the pairing exists to persist the outbound high-water mark.
            if state.pairing(&key).is_none() {
                state
                    .pairings
                    .push(crate::remote::Pairing::new(key.clone()));
            }
            // The bridge owns and assigns the gapless `seq` (it must seal under
            // the exact header, spec §7.1); the client sends it verbatim. Clone the
            // header fields into the wire frame so the originals can rebuild the
            // envelope for re-send if the write fails (0ef.9).
            let envelope = EncryptedEnvelope {
                pairing_id: pairing_id.clone(),
                seq,
                sender: Role::Desktop,
                sent_at_ms,
                nonce: nonce.clone(),
                ciphertext: ciphertext.clone(),
            };
            crate::remote::debuglog::log(&format!(
                "client SEND envelope pairing={} seq={} bytes={}",
                key,
                seq,
                envelope.ciphertext.len()
            ));
            // A forced failure (test seam) short-circuits the real write so it is
            // never delivered; production always evaluates the real send.
            if tuning.take_forced_write_failure()
                || send_frame(sock, &RelayFrame::Envelope(envelope)).is_err()
            {
                crate::remote::debuglog::log(&format!(
                    "client SEND envelope FAILED (socket) pairing={key} seq={seq} — holding to re-send"
                ));
                // Hold the exact envelope so the next session re-sends it before
                // any newer traffic — the bridge already advanced its `out_seq`
                // past this `seq`, so dropping it would leave a wire gap the phone
                // stalls on (0ef.9). The high-water mark is deliberately NOT
                // committed (below), keeping the persisted cursor consistent.
                return Sent::Broke {
                    retry: Some(RemoteOutbound::SendEnvelope {
                        pairing_id,
                        seq,
                        sent_at_ms,
                        nonce,
                        ciphertext,
                    }),
                };
            }
            // Commit the high-water mark only once the send succeeded so a failed
            // write never leaves a gap the peer's dedup would stall on.
            if let Some(p) = state.pairing_mut(&key) {
                if seq > p.last_sent_seq {
                    p.last_sent_seq = seq;
                }
            }
            store.save(state);
            Sent::Ok
        }
        RemoteOutbound::Ack { pairing_id, cursor } => {
            if send_frame(sock, &RelayFrame::Ack { pairing_id, cursor }).is_ok() {
                Sent::Ok
            } else {
                Sent::Broke { retry: None }
            }
        }
        RemoteOutbound::RequestPairing { claim_token_hint } => {
            // Desktop-initiated pairing bootstrap (spec §5.2). For a returning
            // desktop this rides the post-auth pump; a fresh desktop offers
            // pre-auth instead (see [`run_session`]). Same offer either way.
            let offer = build_pairing_offer(identity, claim_token_hint);
            if send_frame(sock, &offer).is_ok() {
                Sent::Ok
            } else {
                Sent::Broke { retry: None }
            }
        }
        RemoteOutbound::Unpair { pairing_id } => {
            // Local clear only (no relay-plane unpair frame in v1): drop the
            // pairing so it is never resumed/activated again.
            let key = pairing_id.as_str().to_string();
            state.pairings.retain(|p| p.pairing_id != key);
            store.save(state);
            Sent::Ok
        }
    }
}

/// Handle one relay→client frame. Returns `false` on a fatal frame (reconnect).
fn handle_frame(
    sock: &mut RelaySocket,
    state: &mut RemoteState,
    store: &dyn RemoteStore,
    inbound_tx: &Sender<RemoteInbound>,
    frame: RelayFrame,
) -> bool {
    match frame {
        RelayFrame::Envelope(env) => {
            let key = env.pairing_id.as_str().to_string();
            crate::remote::debuglog::log(&format!(
                "client RECV envelope pairing={} seq={} sender={:?}",
                key, env.seq, env.sender
            ));
            if state.pairing(&key).is_none() {
                state
                    .pairings
                    .push(crate::remote::Pairing::new(key.clone()));
            }
            let last = state
                .pairing(&key)
                .map(|p| p.last_received_seq)
                .unwrap_or(0);
            if env.seq > last {
                if let Some(p) = state.pairing_mut(&key) {
                    p.last_received_seq = env.seq;
                }
                store.save(state);
                let seq = env.seq;
                let pairing_id = env.pairing_id.clone();
                let _ = inbound_tx.send(RemoteInbound::Envelope(env));
                // Auto-ack contiguous receipt so the relay can trim its queue.
                let _ = send_frame(
                    sock,
                    &RelayFrame::Ack {
                        pairing_id,
                        cursor: seq,
                    },
                );
            }
            // else: a duplicate (redelivery) — silently drop (spec §6.4).
            true
        }
        RelayFrame::Ack { pairing_id, cursor } => {
            crate::remote::debuglog::log(&format!(
                "client RECV ack pairing={} cursor={}",
                pairing_id.as_str(),
                cursor
            ));
            if let Some(p) = state.pairing_mut(pairing_id.as_str()) {
                if cursor > p.last_acked_by_peer {
                    p.last_acked_by_peer = cursor;
                    store.save(state);
                }
            }
            true
        }
        RelayFrame::Pong { client_time_ms, .. } => {
            let latency = (now_ms() - client_time_ms).max(0) as u64;
            report(
                inbound_tx,
                RemoteLinkState::Connected {
                    latency_ms: latency,
                },
            );
            true
        }
        RelayFrame::PeerPresence {
            pairing_id,
            peer,
            state: presence,
            ..
        } => {
            crate::remote::debuglog::log(&format!(
                "client RECV presence pairing={} peer={:?} state={:?}",
                pairing_id.as_str(),
                peer,
                presence
            ));
            let _ = inbound_tx.send(RemoteInbound::Presence {
                pairing_id,
                peer,
                state: presence,
            });
            true
        }
        RelayFrame::PairingOfferOk {
            pairing_id,
            claim_token,
            expires_at_ms,
        } => {
            // Post-auth offer (a returning desktop adding a pairing). A fresh
            // desktop consumes this during the pre-auth bootstrap instead; both
            // route through the same persist + surface helper.
            persist_pairing_offer(
                state,
                store,
                inbound_tx,
                pairing_id,
                claim_token,
                expires_at_ms,
            );
            true
        }
        RelayFrame::PairingClaimed {
            pairing_id,
            peer_device_id,
            peer_key_agreement_public_key,
        } => {
            // The phone joined: record the peer id + its key-agreement key and
            // mark the pairing established so the E2E channel can be derived now
            // and reconstructed on the next launch (spec §5.2 / §7.1).
            if let Some(p) = state.pairing_mut(pairing_id.as_str()) {
                if let Some(id) = &peer_device_id {
                    p.peer_device_id = Some(id.as_str().to_string());
                }
                if let Some(ka) = &peer_key_agreement_public_key {
                    p.peer_key_agreement_public_key = Some(ka.clone());
                    p.established = true;
                }
                store.save(state);
            }
            let _ = inbound_tx.send(RemoteInbound::PairingClaimed {
                pairing_id,
                peer_device_id,
                peer_key_agreement_public_key,
            });
            true
        }
        RelayFrame::PairingRevoked { pairing_id } => {
            // The phone unpaired this Mac (spec §10.2). Drop the pairing locally
            // so it is never resumed/activated again — mirroring the local
            // `Unpair` clear — then tell the app so it tears down that pairing's
            // E2E channel and returns to an unpaired, re-pairable state. Other
            // pairings are untouched.
            crate::remote::debuglog::log(&format!(
                "client RECV pairing_revoked pairing={}",
                pairing_id.as_str()
            ));
            let key = pairing_id.as_str().to_string();
            state.pairings.retain(|p| p.pairing_id != key);
            store.save(state);
            let _ = inbound_tx.send(RemoteInbound::PairingRevoked { pairing_id });
            true
        }
        RelayFrame::Error {
            code: RelayErrorCode::SeqViolation,
            pairing_id,
            ..
        } => {
            crate::remote::debuglog::log(&format!(
                "client RECV error seq_violation pairing={:?}",
                pairing_id.as_ref().map(|p| p.as_str())
            ));
            // The relay is ahead-of-us on this pairing's outbound seq — it lost
            // its in-memory watermark (restart/redeploy) while we kept ours. Do
            // NOT tear the connection down (that just reconnects into the same
            // rejection forever). Re-sync: zero this pairing's persisted outbound
            // cursor and tell the bridge to restart its stream from seq 1 with a
            // fresh snapshot (remote-control-bbf). A `seq_violation` without a
            // pairing id can't be targeted, so it is ignored (non-fatal).
            if let Some(pid) = pairing_id {
                if let Some(p) = state.pairing_mut(pid.as_str()) {
                    p.last_sent_seq = 0;
                    p.last_acked_by_peer = 0;
                    store.save(state);
                }
                let _ = inbound_tx.send(RemoteInbound::SeqResync { pairing_id: pid });
            }
            true
        }
        RelayFrame::Error {
            code,
            ref message,
            ref pairing_id,
        } => {
            crate::remote::debuglog::log(&format!(
                "client RECV error code={:?} pairing={:?} fatal={} msg={}",
                code,
                pairing_id.as_ref().map(|p| p.as_str()),
                is_fatal_error(code),
                message
            ));
            !is_fatal_error(code)
        }
        RelayFrame::Bye { .. } => false,
        // Post-auth restatements of handshake frames or unused directions: ignore.
        _ => true,
    }
}

/// Whether a relay error tears the connection down (vs. an advisory notice).
fn is_fatal_error(code: RelayErrorCode) -> bool {
    matches!(
        code,
        RelayErrorCode::AuthFailed
            | RelayErrorCode::UnsupportedVersion
            | RelayErrorCode::NotAuthenticated
            | RelayErrorCode::BadFrame
            | RelayErrorCode::Internal
    )
}

#[cfg(test)]
mod tests;
