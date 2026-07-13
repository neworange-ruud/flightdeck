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
use std::sync::atomic::{AtomicBool, Ordering};
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
/// Overall budget for completing the auth handshake before giving up.
const AUTH_DEADLINE: Duration = Duration::from_secs(15);

/// Backoff floor (first retry) in milliseconds.
const BACKOFF_BASE_MS: u64 = 1_000;
/// Backoff ceiling in milliseconds.
const BACKOFF_CAP_MS: u64 = 60_000;

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
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("flightdeck-remote".to_string())
            .spawn(move || {
                run(cfg, identity, store, inbound_tx, outbound_rx, stop_thread);
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
    tcp.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).ok();
    tcp.set_write_timeout(Some(WRITE_TIMEOUT)).ok();
    // A second handle onto the same socket: SO_RCVTIMEO is a socket-level option
    // shared by dup'd descriptors, so tightening it here retimes tungstenite's
    // reads after we hand it the original stream.
    let ctl = tcp
        .try_clone()
        .map_err(|e| format!("socket clone failed: {e}"))?;

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

    ctl.set_read_timeout(Some(READ_POLL)).ok();
    Ok(sock)
}

// --- The thread body -------------------------------------------------------

/// Why a single connection session ended.
enum SessionEnd {
    /// `stop()` was requested; do not reconnect.
    Stopped,
    /// The session ended; reconnect. `authed` is whether we ever reached
    /// `auth_ok` (a good session that merely dropped resets the backoff).
    Ended { authed: bool },
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
) {
    let mut attempt: u32 = 0;
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
        );
        report(&inbound_tx, RemoteLinkState::Disconnected);

        match end {
            SessionEnd::Stopped => break,
            SessionEnd::Ended { authed } => {
                attempt = if authed { 0 } else { attempt.saturating_add(1) };
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
fn run_session(
    cfg: &RemoteConfig,
    identity: &DeviceIdentity,
    state: &mut RemoteState,
    store: &dyn RemoteStore,
    inbound_tx: &Sender<RemoteInbound>,
    outbound_rx: &Receiver<RemoteOutbound>,
    stop: &AtomicBool,
) -> SessionEnd {
    let url = effective_url(cfg, state);
    let mut sock = match connect(&url) {
        Ok(s) => s,
        Err(_e) => return SessionEnd::Ended { authed: false },
    };

    // hello.
    let hello = RelayFrame::Hello {
        protocol_version: PROTOCOL_VERSION,
        role: Role::Desktop,
        device_id: DeviceId::new(identity.device_id()),
        client: client_info(),
    };
    if send_frame(&mut sock, &hello).is_err() {
        return SessionEnd::Ended { authed: false };
    }

    // Drive hello_ok → auth_challenge → auth_response → auth_ok under a deadline.
    let deadline = Instant::now() + AUTH_DEADLINE;
    let mut saw_hello_ok = false;
    let mut challenged = false;

    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = send_frame(&mut sock, &RelayFrame::Bye { reason: None });
            sock.close();
            return SessionEnd::Stopped;
        }
        if Instant::now() > deadline {
            return SessionEnd::Ended { authed: false };
        }
        match read_frame(&mut sock) {
            Incoming::Idle => continue,
            Incoming::Closed => return SessionEnd::Ended { authed: false },
            Incoming::Frame(frame) => match *frame {
                RelayFrame::HelloOk { .. } => saw_hello_ok = true,
                RelayFrame::VersionIncompatible { .. } => {
                    return SessionEnd::Ended { authed: false };
                }
                RelayFrame::AuthChallenge { nonce, .. } if saw_hello_ok => {
                    let signature = match identity.sign_nonce_base64(&nonce) {
                        Ok(sig) => sig,
                        Err(_) => return SessionEnd::Ended { authed: false },
                    };
                    let resp = RelayFrame::AuthResponse {
                        device_id: DeviceId::new(identity.device_id()),
                        signature,
                        pairing_ids: state
                            .pairing_ids()
                            .into_iter()
                            .map(PairingId::new)
                            .collect(),
                    };
                    if send_frame(&mut sock, &resp).is_err() {
                        return SessionEnd::Ended { authed: false };
                    }
                    challenged = true;
                }
                RelayFrame::AuthOk { pairing_ids } if challenged => {
                    on_authenticated(&mut sock, state, inbound_tx, pairing_ids);
                    break;
                }
                RelayFrame::Error { .. } => return SessionEnd::Ended { authed: false },
                _ => continue, // unexpected pre-auth frame; ignore
            },
        }
    }

    // Authenticated. Pump until the socket drops or we are told to stop.
    pump(&mut sock, state, store, inbound_tx, outbound_rx, stop)
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

/// The steady-state loop: drain outbound, fire pings, read inbound frames.
fn pump(
    sock: &mut RelaySocket,
    state: &mut RemoteState,
    store: &dyn RemoteStore,
    inbound_tx: &Sender<RemoteInbound>,
    outbound_rx: &Receiver<RemoteOutbound>,
    stop: &AtomicBool,
) -> SessionEnd {
    let mut last_ping = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = send_frame(sock, &RelayFrame::Bye { reason: None });
            sock.close();
            return SessionEnd::Stopped;
        }

        // Drain everything the app queued for us.
        loop {
            match outbound_rx.try_recv() {
                Ok(out) => {
                    if !handle_outbound(sock, state, store, out) {
                        return SessionEnd::Ended { authed: true };
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
            Incoming::Closed => return SessionEnd::Ended { authed: true },
            Incoming::Frame(frame) => {
                if !handle_frame(sock, state, store, inbound_tx, *frame) {
                    return SessionEnd::Ended { authed: true };
                }
            }
        }
    }
}

/// Handle one app→relay message. Returns `false` if the socket broke.
fn handle_outbound(
    sock: &mut RelaySocket,
    state: &mut RemoteState,
    store: &dyn RemoteStore,
    out: RemoteOutbound,
) -> bool {
    match out {
        RemoteOutbound::SendEnvelope {
            pairing_id,
            nonce,
            ciphertext,
        } => {
            let key = pairing_id.as_str().to_string();
            // Ensure the pairing exists so we can assign a gapless seq.
            if state.pairing(&key).is_none() {
                state
                    .pairings
                    .push(crate::remote::Pairing::new(key.clone()));
            }
            let next = state
                .pairing(&key)
                .map(|p| p.last_sent_seq + 1)
                .unwrap_or(1);
            let envelope = EncryptedEnvelope {
                pairing_id: pairing_id.clone(),
                seq: next,
                sender: Role::Desktop,
                sent_at_ms: now_ms(),
                nonce,
                ciphertext,
            };
            if send_frame(sock, &RelayFrame::Envelope(envelope)).is_err() {
                return false;
            }
            // Commit the seq only once the send succeeded so a failed write never
            // leaves a gap the peer's dedup would stall on.
            if let Some(p) = state.pairing_mut(&key) {
                p.last_sent_seq = next;
            }
            store.save(state);
            true
        }
        RemoteOutbound::Ack { pairing_id, cursor } => {
            send_frame(sock, &RelayFrame::Ack { pairing_id, cursor }).is_ok()
        }
        RemoteOutbound::RequestPairing => {
            // No desktop-initiated pairing frame exists in v1 (the phone redeems
            // the shown code). Accepted and ignored until that layer lands.
            true
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
            let _ = inbound_tx.send(RemoteInbound::Presence {
                pairing_id,
                peer,
                state: presence,
            });
            true
        }
        RelayFrame::PairingClaimed {
            pairing_id,
            peer_device_id,
        } => {
            if let Some(id) = &peer_device_id {
                if let Some(p) = state.pairing_mut(pairing_id.as_str()) {
                    p.peer_device_id = Some(id.as_str().to_string());
                    store.save(state);
                }
            }
            let _ = inbound_tx.send(RemoteInbound::Paired {
                pairing_id,
                peer_device_id,
            });
            true
        }
        RelayFrame::Error { code, .. } => !is_fatal_error(code),
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
