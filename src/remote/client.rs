//! The relay connection task.
//!
//! One [`tokio`] task owns an **async** [`tokio_tungstenite`] WebSocket and runs
//! the full client-side relay-plane state machine. It runs on the process's
//! single shared runtime ([`crate::remote::runtime`]) — the same runtime
//! `src/web/server.rs` (axum) spawns onto — so the desktop has exactly one async
//! runtime and one set of I/O idioms for both remote transports
//! (`specs/WEB_INTERFACE.md` D6).
//!
//! ```text
//!   supervisor (one task, restarted sessions, backoff 1s..60s + jitter)
//!   │
//!   ├─▶ connect ──▶ hello ──▶ hello_ok ──▶ auth_challenge
//!   │                                        │
//!   │             (returning desktop)        │        (fresh desktop)
//!   │            persisted pairing ids       │      no pairings yet: wait
//!   │                    │                   │      ≤1s for the app's
//!   │                    ▼                   ▼      RequestPairing, then
//!   │              auth_response ◀──── pairing_offer ──▶ pairing_offer_ok
//!   │                    │
//!   │                    ▼
//!   │                 auth_ok ──▶ resume(pairing, from last held seq) …
//!   │                    │
//!   │                    ▼
//!   │   ┌── pump: one `tokio::select!` per event ───────────────────┐
//!   │   │  stop signal ....... Bye + close, do not reconnect       │
//!   │   │  liveness elapsed .. tear down a half-open socket        │
//!   │   │  outbound channel .. envelope / ack / offer / unpair      │
//!   │   │  inbound frame ..... envelope, ack, pong, presence, …    │
//!   │   │  ping tick ......... latency probe every 20 s            │
//!   │   │  flush tick ........ debounced cursor persist            │
//!   │   └──────────────────────────────────────────────────────────┘
//!   │                    │
//!   └────── session ended ┘  (backoff, then reconnect)
//! ```
//!
//! ## What the tokio port changed, and what it deliberately did not
//!
//! The **public surface is unchanged**: [`RemoteHandle::start`] still takes a
//! `std::sync::mpsc` pair, [`RemoteHandle::stop`] still blocks briefly until the
//! link is down, and the TUI event loop stays fully synchronous — it never sees
//! a future. `src/lib.rs` did not change for this port.
//!
//! What went away is the machinery that *substituted* for async. The old client
//! was a blocking `std::thread` that set a ~100 ms `SO_RCVTIMEO` on the TCP
//! socket so a read could time out, letting one loop also drain the outbound
//! channel, fire pings and notice `stop()`. That trick cost a per-platform
//! `set_read_timeout` walk over every `MaybeTlsStream` variant (a wildcard arm
//! there silently left the pump reading at the 10 s handshake timeout — the
//! Windows bug in remote-control-2jy), and it woke the thread ten times a second
//! forever. Every one of those wakeups is now an event in the pump's
//! `tokio::select!`, so the task sleeps until something actually happens.
//!
//! Where each behaviour moved:
//!
//! | Behaviour | Was | Now |
//! | --- | --- | --- |
//! | non-blocking reads | `SO_RCVTIMEO` + `WouldBlock` = idle | `select!` on `stream.next()` |
//! | prompt `stop()` | `AtomicBool` polled every ~100 ms | [`watch`] channel, a `select!` branch |
//! | ping every 20 s | `last_ping.elapsed()` per tick | `PING_INTERVAL` `interval` tick |
//! | liveness deadline | `last_inbound.elapsed()` per tick | a pinned `Sleep`, reset on each frame |
//! | wedged writes | `SO_SNDTIMEO` | `WRITE_TIMEOUT` around every `send` |
//! | debounced cursor persist | `maybe_flush` per tick | `maybe_flush` per event + a flush tick |
//! | backoff sleep | `interruptible_sleep` poll loop | `select!` on `sleep` vs. stop |
//!
//! Unchanged by design: the session state machine and every fault it handles
//! (backoff-reset stability, resume-from-last-held-seq, auth-rejection
//! self-heal, seq realign/resync, superseded-pairing retirement, terminal
//! version-incompatible), the persistence seam, and the wire frames.
//!
//! ## Crossing the sync/async boundary
//!
//! Two adapters, both local to this module, keep the public channel types as
//! they were:
//!
//! * **app → task**: a small named thread blocks on the `std::sync::mpsc`
//!   receiver and forwards into an unbounded tokio channel the session can
//!   `select!` on. See `RemoteHandle::start_tuned`.
//! * **task → app**: `InboundTx`, a `Mutex` around the `std::sync::mpsc`
//!   sender. `Sender` is `Send` but deliberately not `Sync`, so a `&Sender`
//!   held across an `.await` would make the session future non-`Send` and
//!   therefore unspawnable on a multi-thread runtime.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use tokio::time::{interval_at, sleep, sleep_until, timeout, Instant as Deadline};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::contracts::real::RealFs;
use crate::contracts::RemoteConfig;
use crate::remote::state::{load_remote_state, remote_state_path, save_remote_state, RemoteState};
use crate::remote::{DeviceIdentity, RemoteInbound, RemoteOutbound};

use flightdeck_remote_protocol::relay::{
    ClientInfo, EncryptedEnvelope, RelayErrorCode, RelayFrame,
};
use flightdeck_remote_protocol::{DeviceId, PairingId, Role, PROTOCOL_VERSION};

// --- Tuning constants ------------------------------------------------------

/// Bound on the TCP connect (including DNS) so `stop()` is never delayed long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Generous timeout for the TLS + WebSocket upgrade, which follows the connect.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Write timeout so a wedged socket surfaces as an error, not a hang. In the
/// blocking client this was `SO_SNDTIMEO` on the TCP socket; async writes have no
/// such knob, and an `await` that never completes would also park the pump's
/// `select!` — starving the very liveness deadline meant to catch a dead link —
/// so every send is wrapped in this timeout instead.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Latency-probe interval.
const PING_INTERVAL: Duration = Duration::from_secs(20);
/// Liveness deadline: tear the session down and reconnect if no inbound frame
/// (Pong, Envelope, Ack — anything) arrives for this long. A half-open socket
/// (laptop sleep/wake, wifi↔cell handoff, relay redeploy, NAT idle-reap) stays
/// "open" with the tiny pings sitting in the kernel send buffer, so
/// [`WRITE_TIMEOUT`] never trips and the socket never yields a read — without
/// this the dead link is never noticed (remote-control-0ef.1). Coordinated with
/// the relay's own server-side idle sweep (both 60s) and a multiple of
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
/// the pairing bootstrap a beat after the session task connects. If nothing
/// arrives in time the client falls back to a plain (offer-less) auth, so a
/// desktop with nothing to offer is never stranded. Kept well under
/// [`AUTH_DEADLINE`].
const PENDING_OFFER_WAIT: Duration = Duration::from_secs(1);
/// How long [`RemoteHandle::stop`] waits for the task to send its `Bye` and
/// close the socket. The blocking client joined its thread unbounded; a bound is
/// safer here because the caller is the TUI thread on its way out, and every
/// teardown step the task still has to take is itself capped by
/// [`WRITE_TIMEOUT`].
const STOP_GRACE: Duration = Duration::from_secs(2);

/// Backoff floor (first retry) in milliseconds.
const BACKOFF_BASE_MS: u64 = 1_000;
/// Backoff ceiling in milliseconds.
const BACKOFF_CAP_MS: u64 = 60_000;

/// How often the debounced cursor persist actually hits disk during a live
/// session. Every streamed outbound envelope, inbound envelope, and peer ack
/// bumps a monotonic cursor in [`RemoteState`]; without coalescing each bump
/// rewrote the entire pretty-printed `remote.json` and re-`chmod`'d it 0600 —
/// many full-file rewrites per second under shell streaming, for a counter tick
/// (remote-control-0ef.11). The [`CursorFlushGate`] marks those bumps dirty and
/// flushes at most once per this interval; pairing-lifecycle changes still
/// persist immediately, and a dirty gate is always flushed on session end so a
/// clean teardown never loses a cursor. Well under [`LIVENESS_TIMEOUT`] so at
/// most a couple of seconds of cursor progress is ever at risk on a hard crash —
/// and those cursors are resume/dedup optimizations the relay's at-least-once
/// redelivery already tolerates (a rewound outbound cursor self-heals via the
/// `seq_violation` resync, remote-control-bbf).
const CURSOR_FLUSH_INTERVAL: Duration = Duration::from_secs(2);

/// Floor for the pump's flush tick. The blocking pump re-checked the gate on
/// every ~100 ms poll, so an idle-but-dirty gate always reached disk; an async
/// pump has no idle ticks, so it schedules one explicitly. A floor is required
/// because `tokio::time::interval` panics on a zero period, and tests set
/// [`ClientTuning::cursor_flush_interval`] to zero to disable the debounce.
const CURSOR_FLUSH_TICK_FLOOR: Duration = Duration::from_millis(25);

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
    /// See [`CURSOR_FLUSH_INTERVAL`]. Tests that assert a debounced cursor has
    /// reached the store set this to [`Duration::ZERO`] so every pump event
    /// flushes a dirty gate immediately; production uses the real interval.
    cursor_flush_interval: Duration,
}

impl Default for ClientTuning {
    fn default() -> Self {
        ClientTuning {
            liveness_timeout: LIVENESS_TIMEOUT,
            min_stable_session: MIN_STABLE_SESSION,
            fail_next_envelope_writes: Arc::new(AtomicU32::new(0)),
            cursor_flush_interval: CURSOR_FLUSH_INTERVAL,
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
///
/// `Sync` is required (on top of `Send`) because the session holds a
/// `&dyn RemoteStore` across `.await` points, and a future is only `Send` — and
/// so only spawnable on the shared multi-thread runtime — if the references it
/// holds are. Every implementation in the tree already satisfies it.
///
/// The two methods stay **synchronous**, so a save is a blocking file write on a
/// runtime worker. That is deliberate: the writes are small, already coalesced
/// by `CursorFlushGate`, and keeping the trait sync is what lets tests
/// implement it with a plain `Mutex`.
pub trait RemoteStore: Send + Sync {
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

// --- task → app channel ----------------------------------------------------

/// The app-bound `std::sync::mpsc::Sender`, wrapped so the session task can hold
/// a shared reference to it across `.await` points.
///
/// `Sender<T>` is `Send` but deliberately **not** `Sync`, so a `&Sender` living
/// inside a future makes that future non-`Send` — and a non-`Send` future cannot
/// be spawned on the shared multi-thread runtime. The public API must keep
/// handing the TUI a plain `std::sync::mpsc` pair (nothing in `src/lib.rs`
/// changes), so the fix is local and cheap: one `Mutex` whose only user is this
/// task, never held across an await, and never contended.
struct InboundTx(std::sync::Mutex<Sender<RemoteInbound>>);

impl InboundTx {
    fn new(tx: Sender<RemoteInbound>) -> Self {
        InboundTx(std::sync::Mutex::new(tx))
    }

    /// Best-effort send; a closed channel (the app is gone) is ignored, exactly
    /// as every `let _ = tx.send(..)` in the blocking client was.
    fn send(&self, msg: RemoteInbound) {
        if let Ok(tx) = self.0.lock() {
            let _ = tx.send(msg);
        }
    }
}

// --- Handle ----------------------------------------------------------------

/// A running relay client. Dropping it (or calling [`Self::stop`]) tears the
/// connection down: the stop [`watch`] sender is dropped, which the session task
/// observes as a shutdown request.
///
/// The handle owns no runtime — the runtime is process-wide and shared with the
/// web server ([`crate::remote::runtime`]), so stopping the relay client must
/// never shut a runtime down under the other consumer.
pub struct RemoteHandle {
    /// Dropped or set to `true` to request shutdown. `None` when no task was
    /// started (no runtime available), which makes [`Self::stop`] a no-op.
    stop: Option<watch::Sender<bool>>,
    /// Held by the task for its lifetime; when the task ends the sender drops and
    /// this receiver reports `Disconnected`, which is how [`Self::stop`] waits
    /// for the socket to be closed without needing a runtime context.
    done: Option<Receiver<()>>,
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
        // No runtime means no relay client — and no panic. This mirrors the
        // blocking client, which returned a handle with no `JoinHandle` when
        // `std::thread::spawn` failed: the app runs exactly as before, minus
        // FlightDeck Remote.
        let Some(runtime) = crate::remote::runtime::try_shared() else {
            crate::remote::debuglog::log("client START skipped — no async runtime");
            return RemoteHandle {
                stop: None,
                done: None,
            };
        };

        // app → task. The app keeps its plain `std::sync::mpsc::Sender`; a small
        // named thread blocks on the receiving end and forwards into a tokio
        // channel the session can `select!` on. Blocking `recv()` (rather than a
        // polling `recv_timeout`) is the point: the thread parks with zero
        // wakeups until the app actually sends something. It exits when the app
        // drops its sender (shutdown) or when the session task has gone away and
        // the forward fails, so it holds nothing open past the app's lifetime.
        let (async_out_tx, async_out_rx) = unbounded_channel::<RemoteOutbound>();
        let forwarder = std::thread::Builder::new()
            .name("flightdeck-remote-outbound".to_string())
            .spawn(move || {
                while let Ok(msg) = outbound_rx.recv() {
                    if async_out_tx.send(msg).is_err() {
                        break;
                    }
                }
            });
        if forwarder.is_err() {
            crate::remote::debuglog::log("client START failed — no outbound forwarder thread");
            return RemoteHandle {
                stop: None,
                done: None,
            };
        }

        let (stop_tx, stop_rx) = watch::channel(false);
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        runtime.spawn(async move {
            // Owned by the task: dropped when it returns, which is what unblocks
            // `stop()`. Never sent on — its drop is the signal.
            let _done = done_tx;
            run(
                cfg,
                identity,
                store,
                InboundTx::new(inbound_tx),
                async_out_rx,
                stop_rx,
                tuning,
            )
            .await;
        });
        RemoteHandle {
            stop: Some(stop_tx),
            done: Some(done_rx),
        }
    }

    /// Signal the task to shut down and wait (briefly) for it to finish, so the
    /// socket is closed and the relay has seen our `Bye` before the process exits.
    ///
    /// Callable from any thread — including from inside an async context, which
    /// is why it waits on a plain channel rather than `Handle::block_on` (that
    /// panics when called from within a runtime).
    pub fn stop(mut self) {
        // Dropping the watch sender is itself a shutdown signal, so a failed
        // `send` (no receivers left — the task already ended) is fine.
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(true);
        }
        if let Some(done) = self.done.take() {
            // `Err` = the task's `done_tx` dropped (it finished) or the grace
            // period elapsed; either way there is nothing left to wait for.
            let _ = done.recv_timeout(STOP_GRACE);
        }
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

// --- Shutdown signalling ---------------------------------------------------

/// Resolve as soon as shutdown has been requested — either explicitly, or by the
/// [`RemoteHandle`] being dropped (which drops the sender).
///
/// Cancel-safe: `watch::Receiver::changed` may be dropped and re-created freely,
/// so this can sit in a `select!` that another branch wins, as often as needed.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            // The handle was dropped without `stop()`; wind down anyway rather
            // than holding the socket open past app exit.
            return;
        }
    }
}

/// Non-awaiting check, for the supervisor's loop conditions.
fn stop_requested(stop: &watch::Receiver<bool>) -> bool {
    *stop.borrow()
}

// --- Socket ----------------------------------------------------------------

/// A connected relay socket. `wss` works on every platform, but through a
/// different TLS backend per target: rustls off Windows, SChannel (via
/// `native-tls`) on Windows, where rustls' aws-lc dependency would cost the
/// release runner a C toolchain. See the two tokio-tungstenite entries in
/// Cargo.toml.
///
/// Unlike the blocking client this is a single concrete type rather than a
/// plain/TLS enum: `MaybeTlsStream` already implements `AsyncRead`/`AsyncWrite`
/// for every backend, so nothing here has to know which one it got. That also
/// retires the per-variant `set_read_timeout` walk the blocking pump needed —
/// the source of remote-control-2jy's Windows bug, where the catch-all arm
/// silently left the pump reading at the handshake timeout.
type RelaySocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The write half. Split from the read half so the pump can `select!` on
/// incoming frames while its handlers write acks, resumes and pings.
type RelaySink = SplitSink<RelaySocket, Message>;
/// The read half.
type RelayRx = SplitStream<RelaySocket>;

/// Serialize a relay frame and write it as a WebSocket text message, bounded by
/// [`WRITE_TIMEOUT`] so a wedged socket cannot park the pump forever.
async fn send_frame(sink: &mut RelaySink, frame: &RelayFrame) -> Result<(), String> {
    let json = serde_json::to_string(frame).expect("relay frame serializes");
    match timeout(WRITE_TIMEOUT, sink.send(Message::Text(json.into()))).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("write failed: {e}")),
        Err(_) => Err(format!(
            "write timed out after {}s",
            WRITE_TIMEOUT.as_secs()
        )),
    }
}

/// Send the WebSocket close frame, best-effort and bounded.
async fn close(sink: &mut RelaySink) {
    let _ = timeout(WRITE_TIMEOUT, sink.close()).await;
}

/// The outcome of one read on the socket.
enum Incoming {
    /// A parsed relay frame.
    Frame(Box<RelayFrame>),
    /// A message we do not act on: malformed/unknown text, or a binary/ping/pong
    /// control frame (tungstenite answers WebSocket pings itself).
    Ignored,
    /// The socket closed or errored — the connection is over.
    Closed,
}

/// Await the next message and classify it. Unknown/malformed text and control
/// frames are reported as [`Incoming::Ignored`] so the session keeps going.
///
/// Cancel-safe: the framing state lives in the `WebSocketStream`, so dropping
/// this future mid-frame (a `select!` another branch won) resumes cleanly on the
/// next call — the same property the blocking client relied on when a
/// `SO_RCVTIMEO` read timed out mid-frame.
async fn read_next(stream: &mut RelayRx) -> Incoming {
    match stream.next().await {
        Some(Ok(Message::Text(text))) => match serde_json::from_str::<RelayFrame>(text.as_str()) {
            Ok(frame) => Incoming::Frame(Box::new(frame)),
            Err(_) => Incoming::Ignored,
        },
        Some(Ok(Message::Close(_))) => Incoming::Closed,
        Some(Ok(_)) => Incoming::Ignored,
        Some(Err(_)) => Incoming::Closed,
        // The stream ended.
        None => Incoming::Closed,
    }
}

// --- Connect ---------------------------------------------------------------

/// Resolve and open the relay socket: TCP connect, then the TLS + WebSocket
/// upgrade. Both phases are bounded and reported separately, so an unreachable
/// relay never looks like a TLS fault (and vice versa) in the message that
/// reaches the pairing overlay.
async fn connect(url: &str) -> Result<RelaySocket, String> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

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

    // tokio resolves the host on its blocking pool, so DNS shares the connect
    // budget instead of blocking a runtime worker.
    let tcp = match timeout(CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), port))).await {
        Ok(Ok(tcp)) => tcp,
        Ok(Err(e)) => return Err(format!("tcp connect failed: {e}")),
        Err(_) => {
            return Err(format!(
                "tcp connect timed out after {}s",
                CONNECT_TIMEOUT.as_secs()
            ))
        }
    };

    // Same call on every platform: with exactly one TLS backend enabled for this
    // target, `client_async_tls`'s no-connector path picks it up and hands back
    // the matching `MaybeTlsStream` variant. A `ws://` url takes the plain path
    // through the same call.
    let phase = if secure { "tls" } else { "ws" };
    match timeout(
        HANDSHAKE_TIMEOUT,
        tokio_tungstenite::client_async_tls(request, tcp),
    )
    .await
    {
        Ok(Ok((sock, _resp))) => Ok(sock),
        Ok(Err(e)) => Err(format!("{phase} upgrade: {e}")),
        Err(_) => Err(format!(
            "{phase} upgrade timed out after {}s",
            HANDSHAKE_TIMEOUT.as_secs()
        )),
    }
}

// --- The task body ---------------------------------------------------------

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

/// Tell the app *why* a handshake never reached `auth_ok`, so the pairing
/// overlay can explain itself instead of spinning on "Requesting a pairing
/// code…" while the supervisor backoff-loops invisibly (the Windows-pairing
/// report: a relay that refuses the connection looked identical to a relay that
/// was simply slow). Best-effort — a closed channel means the app is gone.
fn report_handshake_failure(inbound_tx: &InboundTx, reason: String, retrying: bool) {
    crate::remote::debuglog::log(&format!(
        "client HANDSHAKE failed retrying={retrying} reason={reason}"
    ));
    inbound_tx.send(RemoteInbound::HandshakeFailed { reason, retrying });
}

/// Whether a just-ended session justifies resetting the reconnect backoff to
/// zero. Only a session that reached `auth_ok` **and** then stayed authenticated
/// for at least `min_stable` counts as healthy; a post-auth flap does not, so a
/// crash/redeploy loop keeps growing its backoff instead of hammering the relay
/// ~once/second (remote-control-0ef.2).
fn session_resets_backoff(authed_for: Option<Duration>, min_stable: Duration) -> bool {
    matches!(authed_for, Some(d) if d >= min_stable)
}

fn report(inbound_tx: &InboundTx, state: RemoteLinkState) {
    inbound_tx.send(RemoteInbound::Link(state));
}

/// The reconnect supervisor: attempt after attempt with backoff until stopped.
async fn run(
    cfg: RemoteConfig,
    identity: DeviceIdentity,
    store: Box<dyn RemoteStore>,
    inbound_tx: InboundTx,
    mut outbound_rx: UnboundedReceiver<RemoteOutbound>,
    mut stop: watch::Receiver<bool>,
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
    // was on disk when the task started.
    let mut state = store.load();
    state.device_private_key = identity.private_key_base64();

    while !stop_requested(&stop) {
        report(&inbound_tx, RemoteLinkState::Connecting);
        let end = run_session(
            &cfg,
            &identity,
            &mut state,
            store.as_ref(),
            &inbound_tx,
            &mut outbound_rx,
            &mut stop,
            &tuning,
            pending.take(),
        )
        .await;

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
                crate::remote::debuglog::log(&format!(
                    "client VERSION incompatible ours={our_version} relay={relay_min}..={relay_max} \
                     — not reconnecting"
                ));
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
                    crate::remote::debuglog::log(&format!(
                        "client AUTH rejected {AUTH_REJECT_REOFFER_THRESHOLD}x — dropped {} stale \
                         pairing(s), will re-offer on next connect",
                        dropped.len()
                    ));
                    inbound_tx.send(RemoteInbound::PairingRejected {
                        pairing_ids: dropped,
                    });
                    auth_reject_streak = 0;
                    attempt = 0;
                }
            }
        }
        if stop_requested(&stop) {
            break;
        }
        // Backoff, woken early by `stop()` instead of the blocking client's
        // 100 ms poll loop.
        tokio::select! {
            _ = stopped(&mut stop) => break,
            _ = sleep(backoff_delay(attempt, jitter_unit())) => {}
        }
    }
}

/// The effective relay URL: a per-device `remote.json` override wins over config.
fn effective_url(cfg: &RemoteConfig, state: &RemoteState) -> String {
    match &state.relay_url {
        Some(u) if !u.is_empty() => u.clone(),
        _ => cfg.relay_url.clone(),
    }
}

/// The shared relay password to present in `hello`, or `None` to present none
/// (remote-control-uq7). Precedence, highest first:
///   1. the `FLIGHTDECK_RELAY_PASSWORD` environment variable, when set and
///      non-empty — lets a deployment inject the secret without persisting it to
///      `config.toml`, and is the exact source the iOS/relay sides mirror;
///   2. `[remote].relay_password` from the effective (merged global→project)
///      `config.toml`.
///
/// An empty/whitespace-only value at either layer is treated as unset, so a
/// local/dev relay with no password configured still connects (the relay stays
/// open when it has no password). The value is sent verbatim — never trimmed —
/// so it byte-matches the relay's constant-time compare.
fn effective_relay_password(cfg: &RemoteConfig) -> Option<String> {
    resolve_relay_password(
        std::env::var("FLIGHTDECK_RELAY_PASSWORD").ok(),
        &cfg.relay_password,
    )
}

/// Pure precedence logic behind [`effective_relay_password`], split out so it can
/// be unit-tested without mutating the process-global `FLIGHTDECK_RELAY_PASSWORD`
/// env var (which would race across parallel test threads). `env` is the raw env
/// value; `cfg_value` is `[remote].relay_password`. Empty/whitespace-only at
/// either layer is treated as unset; a real env value wins over config.
fn resolve_relay_password(env: Option<String>, cfg_value: &str) -> Option<String> {
    if let Some(env) = env.filter(|s| !s.trim().is_empty()) {
        return Some(env);
    }
    Some(cfg_value.to_string()).filter(|s| !s.trim().is_empty())
}

/// A deadline far enough out to stand in for "never" in a `select!` branch whose
/// precondition is false. Used instead of unwrapping an absent deadline, so the
/// branch is inert rather than panicking if the macro ever evaluates it.
fn never() -> Deadline {
    Deadline::now() + Duration::from_secs(3_600)
}

/// One connection session: connect, authenticate, resume, then pump.
#[allow(clippy::too_many_arguments)]
async fn run_session(
    cfg: &RemoteConfig,
    identity: &DeviceIdentity,
    state: &mut RemoteState,
    store: &dyn RemoteStore,
    inbound_tx: &InboundTx,
    outbound_rx: &mut UnboundedReceiver<RemoteOutbound>,
    stop: &mut watch::Receiver<bool>,
    tuning: &ClientTuning,
    pending_in: Option<RemoteOutbound>,
) -> SessionEnd {
    let url = effective_url(cfg, state);
    let sock = match connect(&url).await {
        Ok(s) => s,
        Err(e) => {
            // Expose the connect-error detail for diagnostics instead of
            // silently discarding it — a bad relay URL / DNS failure otherwise
            // retries forever with the user seeing only "reconnecting" and zero
            // signal about why (remote-control-0ef.20).
            crate::remote::debuglog::log(&format!("client CONNECT failed url={url} err={e}"));
            // NOT `eprintln!`: this runs under a full-screen TUI on the alternate
            // screen, where a stray stderr line lands on top of the rendered
            // frame. The reason now reaches the UI over the inbound channel
            // instead, which is where a user can actually act on it.
            report_handshake_failure(inbound_tx, format!("cannot reach the relay: {e}"), true);
            return ended_unauthed();
        }
    };
    let (mut sink, mut stream) = sock.split();

    // This machine's display name, announced on `auth_response` (spec §10.1).
    // Resolved once per connect — which is exactly the "computed fresh each
    // connect, never cached" rule — and on the blocking pool, because it may
    // spawn `hostname` and a runtime worker must not be parked on a subprocess.
    let machine = tokio::task::spawn_blocking(machine_name)
        .await
        .unwrap_or(None);

    // hello.
    let hello = RelayFrame::Hello {
        protocol_version: PROTOCOL_VERSION,
        role: Role::Desktop,
        device_id: DeviceId::new(identity.device_id()),
        client: client_info(),
        relay_password: effective_relay_password(cfg),
    };
    if send_frame(&mut sink, &hello).await.is_err() {
        report_handshake_failure(
            inbound_tx,
            "the connection dropped while greeting the relay".to_string(),
            true,
        );
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
    let deadline = sleep(AUTH_DEADLINE);
    tokio::pin!(deadline);
    let mut saw_hello_ok = false;
    // The challenge nonce, captured until we decide to answer it.
    let mut challenge_nonce: Option<String> = None;
    // Whether we have already sent our `auth_response`.
    let mut sent_auth = false;
    // Fresh-desktop bootstrap bookkeeping.
    let mut offer_sent = false;
    let mut offer_wait_until: Option<Deadline> = None;
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
        // The fresh-desktop pre-auth window: open only while we hold a challenge
        // nonce we have not answered and have not yet offered. Outside it the
        // outbound channel is deliberately NOT drained — a returning desktop's
        // traffic waits for the pump, exactly as under the blocking client.
        let offer_window =
            offer_wait_until.is_some() && challenge_nonce.is_some() && !offer_sent && !sent_auth;
        let offer_lapses_at = offer_wait_until.unwrap_or_else(never);

        tokio::select! {
            // Ordered: shutdown and the handshake deadline outrank protocol
            // progress, and each branch is a single bounded step, so nothing here
            // can starve anything else.
            biased;

            _ = stopped(stop) => {
                let _ = send_frame(&mut sink, &RelayFrame::Bye { reason: None }).await;
                close(&mut sink).await;
                return SessionEnd::Stopped;
            }

            _ = &mut deadline => {
                report_handshake_failure(
                    inbound_tx,
                    "the relay did not finish the handshake in time".to_string(),
                    true,
                );
                return ended_unauthed();
            }

            queued = outbound_rx.recv(), if offer_window => {
                match queued {
                    Some(RemoteOutbound::RequestPairing { claim_token_hint }) => {
                        let offer = build_pairing_offer(identity, claim_token_hint);
                        if send_frame(&mut sink, &offer).await.is_err() {
                            report_handshake_failure(
                                inbound_tx,
                                "the connection dropped while requesting a pairing code"
                                    .to_string(),
                                true,
                            );
                            return ended_unauthed();
                        }
                        offer_sent = true;
                    }
                    Some(other) => deferred.push(other),
                    None => {
                        // The app dropped its sender (shutting down).
                        let _ = send_frame(&mut sink, &RelayFrame::Bye { reason: None }).await;
                        close(&mut sink).await;
                        return SessionEnd::Stopped;
                    }
                }
            }

            _ = sleep_until(offer_lapses_at), if offer_window => {
                // Nothing to offer in time: fall back to a plain auth so an idle
                // desktop is never stranded mid-handshake.
                let Some(nonce) = challenge_nonce.as_ref() else {
                    return ended_unauthed();
                };
                if !send_auth_response(&mut sink, identity, nonce, state, machine.clone()).await {
                    return ended_unauthed();
                }
                sent_auth = true;
            }

            incoming = read_next(&mut stream) => match incoming {
                Incoming::Ignored => continue,
                Incoming::Closed => {
                    report_handshake_failure(
                        inbound_tx,
                        "the relay closed the connection during the handshake".to_string(),
                        true,
                    );
                    return ended_unauthed();
                }
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
                            offer_wait_until = Some(Deadline::now() + PENDING_OFFER_WAIT);
                            challenge_nonce = Some(nonce);
                        } else {
                            // Returning desktop: auth-first, exactly as before.
                            if !send_auth_response(
                                &mut sink, identity, &nonce, state, machine.clone(),
                            )
                            .await
                            {
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
                        match challenge_nonce.clone() {
                            Some(nonce) => {
                                if !send_auth_response(
                                    &mut sink, identity, &nonce, state, machine.clone(),
                                )
                                .await
                                {
                                    return ended_unauthed();
                                }
                                sent_auth = true;
                            }
                            None => return ended_unauthed(),
                        }
                    }
                    RelayFrame::AuthOk { pairing_ids } if sent_auth => {
                        on_authenticated(&mut sink, state, inbound_tx, pairing_ids).await;
                        break;
                    }
                    RelayFrame::Error { code, message, .. } => {
                        // The relay told us why, so pass that on verbatim-ish: a
                        // refusal here (a missing/wrong `relay_password`, an unknown
                        // device) is a *configuration* fault that no amount of
                        // reconnecting clears, and the pairing overlay used to sit on
                        // "Requesting a pairing code…" through every silent retry.
                        report_handshake_failure(
                            inbound_tx,
                            format!("the relay refused the connection: {message}"),
                            // `rate_limited` and friends do clear on their own, so
                            // only an outright rejection of this device counts as
                            // terminal.
                            !matches!(
                                code,
                                RelayErrorCode::AuthFailed | RelayErrorCode::UnknownPairing
                            ),
                        );
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
            },
        }
    }

    // From here on we are authenticated; measure how long we stay up so a
    // sub-threshold flap does not reset the reconnect backoff (0ef.2).
    let authed_at = Instant::now();

    // One gate spans the whole authenticated session (pre-pump re-sends + pump),
    // coalescing the streamed cursor persists (0ef.11). Flushed on every exit
    // below so a clean end never loses a cursor.
    let mut gate = CursorFlushGate::new(tuning.cursor_flush_interval);

    // Re-send an envelope whose write failed on the previous session BEFORE any
    // freshly-queued traffic, so its `seq` slots back into the stream contiguously
    // and the phone's dedup never stalls on a gap (0ef.9). If the write fails
    // again, hold it once more for the next session.
    if let Some(out) = pending_in {
        if let Sent::Broke { retry } =
            handle_outbound(&mut sink, identity, state, store, &mut gate, tuning, out).await
        {
            gate.flush(store, state);
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
            handle_outbound(&mut sink, identity, state, store, &mut gate, tuning, out).await
        {
            gate.flush(store, state);
            return SessionEnd::Ended {
                authed_for: Some(authed_at.elapsed()),
                pending: retry,
            };
        }
    }

    // Authenticated. Pump until the socket drops or we are told to stop.
    pump(
        &mut sink,
        &mut stream,
        identity,
        state,
        store,
        inbound_tx,
        outbound_rx,
        stop,
        tuning,
        &mut gate,
        authed_at,
    )
    .await
}

/// Sign `nonce_b64` and send the `auth_response`, activating whatever pairings
/// the persisted state currently holds (empty for an offer-less fresh desktop,
/// or including a just-offered pairing once its `pairing_offer_ok` landed).
/// Returns `false` if signing or the socket write failed.
async fn send_auth_response(
    sink: &mut RelaySink,
    identity: &DeviceIdentity,
    nonce_b64: &str,
    state: &RemoteState,
    machine_name: Option<String>,
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
        // (spec §10.1). Resolved once per connect by [`run_session`] — never
        // cached across connects — so a rename propagates on the next reconnect.
        machine_name,
    };
    send_frame(sink, &resp).await.is_ok()
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
///
/// Blocking (it may spawn a subprocess), so callers run it on the blocking pool.
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
    inbound_tx: &InboundTx,
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
    inbound_tx.send(RemoteInbound::PairingOffered {
        pairing_id,
        claim_token,
        expires_at_ms,
    });
}

/// After `auth_ok`: report Connected, then `resume` each active pairing from the
/// highest seq we already hold, and surface the pairings to the app.
async fn on_authenticated(
    sink: &mut RelaySink,
    state: &RemoteState,
    inbound_tx: &InboundTx,
    pairing_ids: Vec<PairingId>,
) {
    report(inbound_tx, RemoteLinkState::Connected { latency_ms: 0 });
    for pid in pairing_ids {
        let from_seq = state
            .pairing(pid.as_str())
            .map(|p| p.last_received_seq)
            .unwrap_or(0);
        let _ = send_frame(
            sink,
            &RelayFrame::Resume {
                pairing_id: pid.clone(),
                from_seq,
            },
        )
        .await;
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
            inbound_tx.send(RemoteInbound::Paired {
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

/// Coalesces the high-frequency cursor persists — outbound high-water
/// (`last_sent_seq`), peer ack (`last_acked_by_peer`), and inbound receipt
/// (`last_received_seq`) — into at most one `remote.json` rewrite per
/// [`ClientTuning::cursor_flush_interval`], instead of a full pretty-printed
/// rewrite + `chmod 0600` per streamed envelope/ack (remote-control-0ef.11).
///
/// Only monotonic cursor bumps are debounced; a pairing-lifecycle change
/// (offer/claim/revoke/unpair/seq-resync) still calls [`RemoteStore::save`]
/// directly and then [`Self::mark_clean`] to fold any pending bump into that
/// same durable write. A dirty gate is always flushed on session end (and before
/// a `Bye`), so a clean teardown never loses a cursor — only a hard crash can,
/// and only the last interval's worth, which the relay's at-least-once
/// redelivery tolerates.
struct CursorFlushGate {
    /// A cursor advanced in memory since the last disk write.
    dirty: bool,
    /// When the last flush (or [`Self::mark_clean`]) happened.
    last_flush: Instant,
    /// Minimum spacing between debounced flushes.
    interval: Duration,
}

impl CursorFlushGate {
    fn new(interval: Duration) -> Self {
        CursorFlushGate {
            dirty: false,
            last_flush: Instant::now(),
            interval,
        }
    }

    /// Record that a cursor advanced in memory; the disk write is deferred to the
    /// next [`Self::maybe_flush`] whose interval has elapsed (or the session-end
    /// [`Self::flush`]).
    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Note that the full state was just persisted out-of-band (a lifecycle
    /// `store.save`), so any pending cursor bump is now on disk too: clear the
    /// dirty flag and restart the debounce window.
    fn mark_clean(&mut self) {
        self.dirty = false;
        self.last_flush = Instant::now();
    }

    /// Flush if dirty and the debounce interval has elapsed — called after every
    /// pump event, and on the pump's flush tick so an idle-but-dirty gate still
    /// reaches disk.
    fn maybe_flush(&mut self, store: &dyn RemoteStore, state: &RemoteState) {
        if self.dirty && self.last_flush.elapsed() >= self.interval {
            store.save(state);
            self.mark_clean();
        }
    }

    /// Force a flush now if dirty, regardless of the interval — called on every
    /// session-end path so no cursor is lost on a clean teardown.
    fn flush(&mut self, store: &dyn RemoteStore, state: &RemoteState) {
        if self.dirty {
            store.save(state);
            self.mark_clean();
        }
    }
}

/// The steady-state loop: one `select!` per event — outbound traffic, inbound
/// frames, the ping tick, the liveness deadline, the debounced-persist tick, and
/// the stop signal. The blocking pump did the same work on a ~100 ms poll; here
/// the task sleeps until one of those actually happens.
#[allow(clippy::too_many_arguments)]
async fn pump(
    sink: &mut RelaySink,
    stream: &mut RelayRx,
    identity: &DeviceIdentity,
    state: &mut RemoteState,
    store: &dyn RemoteStore,
    inbound_tx: &InboundTx,
    outbound_rx: &mut UnboundedReceiver<RemoteOutbound>,
    stop: &mut watch::Receiver<bool>,
    tuning: &ClientTuning,
    gate: &mut CursorFlushGate,
    authed_at: Instant,
) -> SessionEnd {
    // `interval_at` (not `interval`) so the first tick is one full period away:
    // `interval` fires immediately, which would ping the instant we authenticate.
    let mut ping = interval_at(Deadline::now() + PING_INTERVAL, PING_INTERVAL);
    // Skip missed ticks rather than bursting through a backlog after a suspend.
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let flush_period = tuning.cursor_flush_interval.max(CURSOR_FLUSH_TICK_FLOOR);
    let mut flush = interval_at(Deadline::now() + flush_period, flush_period);
    flush.set_missed_tick_behavior(MissedTickBehavior::Delay);

    // Liveness: a pinned timer reset by every inbound frame. A half-open socket
    // delivers nothing yet never errors on our tiny pings, so we tear the session
    // down once this elapses instead of waiting on a read that never comes
    // (remote-control-0ef.1). Armed at auth so a silent socket is caught even if
    // the very first frame never arrives.
    let liveness = sleep(tuning.liveness_timeout);
    tokio::pin!(liveness);

    let end = loop {
        // Set when an inbound frame proved the link alive; the timer is reset
        // after the `select!` rather than inside a branch, so nothing borrows it
        // twice.
        let mut alive = false;

        tokio::select! {
            // Ordered rather than random so the pump keeps the blocking client's
            // priorities: shutdown first, then the liveness verdict, then drain
            // what the app queued, then read. Each branch is one bounded step.
            biased;

            _ = stopped(stop) => {
                gate.flush(store, state);
                let _ = send_frame(sink, &RelayFrame::Bye { reason: None }).await;
                close(sink).await;
                return SessionEnd::Stopped;
            }

            _ = &mut liveness => {
                crate::remote::debuglog::log(&format!(
                    "client LIVENESS timeout ({}s) — tearing down half-open session",
                    tuning.liveness_timeout.as_secs()
                ));
                break SessionEnd::Ended {
                    authed_for: Some(authed_at.elapsed()),
                    pending: None,
                };
            }

            queued = outbound_rx.recv() => match queued {
                Some(out) => {
                    if let Sent::Broke { retry } =
                        handle_outbound(sink, identity, state, store, gate, tuning, out).await
                    {
                        break SessionEnd::Ended {
                            authed_for: Some(authed_at.elapsed()),
                            pending: retry,
                        };
                    }
                }
                None => {
                    // The app dropped its sender (shutting down).
                    gate.flush(store, state);
                    let _ = send_frame(sink, &RelayFrame::Bye { reason: None }).await;
                    close(sink).await;
                    return SessionEnd::Stopped;
                }
            },

            incoming = read_next(stream) => match incoming {
                Incoming::Ignored => {}
                Incoming::Closed => {
                    break SessionEnd::Ended {
                        authed_for: Some(authed_at.elapsed()),
                        pending: None,
                    }
                }
                Incoming::Frame(frame) => {
                    // Any *relay* frame proves the link is alive. A WebSocket-level
                    // pong deliberately does not: the blocking pump only ever reset
                    // its deadline on a parsed frame, and a relay that answered
                    // nothing but ws pings is exactly the half-open case 0ef.1 is
                    // about.
                    alive = true;
                    if !handle_frame(sink, state, store, gate, inbound_tx, *frame).await {
                        break SessionEnd::Ended {
                            authed_for: Some(authed_at.elapsed()),
                            pending: None,
                        };
                    }
                }
            },

            _ = ping.tick() => {
                let _ = send_frame(sink, &RelayFrame::Ping { client_time_ms: now_ms() }).await;
            }

            // Nothing to do beyond the coalesced persist below; this tick exists
            // only so an idle-but-dirty gate still reaches disk.
            _ = flush.tick() => {}
        }

        if alive {
            liveness
                .as_mut()
                .reset(Deadline::now() + tuning.liveness_timeout);
        }

        // Coalesced cursor persist: at most one `remote.json` rewrite per
        // interval, folding all the bumps since the last flush (0ef.11).
        gate.maybe_flush(store, state);
    };
    // A reconnect-ending session: persist the final cursor before returning.
    gate.flush(store, state);
    end
}

/// Handle one app→relay message. Returns [`Sent::Broke`] if the socket broke.
async fn handle_outbound(
    sink: &mut RelaySink,
    identity: &DeviceIdentity,
    state: &mut RemoteState,
    store: &dyn RemoteStore,
    gate: &mut CursorFlushGate,
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
                || send_frame(sink, &RelayFrame::Envelope(envelope))
                    .await
                    .is_err()
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
            // write never leaves a gap the peer's dedup would stall on. The disk
            // write is debounced — under shell streaming this bumps many times a
            // second, and a rewound outbound cursor self-heals via `seq_violation`
            // resync, so it need not be durable per envelope (0ef.11).
            if let Some(p) = state.pairing_mut(&key) {
                if seq > p.last_sent_seq {
                    p.last_sent_seq = seq;
                }
            }
            gate.mark_dirty();
            Sent::Ok
        }
        RemoteOutbound::Ack { pairing_id, cursor } => {
            if send_frame(sink, &RelayFrame::Ack { pairing_id, cursor })
                .await
                .is_ok()
            {
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
            if send_frame(sink, &offer).await.is_ok() {
                Sent::Ok
            } else {
                Sent::Broke { retry: None }
            }
        }
        RemoteOutbound::Unpair { pairing_id } => {
            // Local clear only (no relay-plane unpair frame in v1): drop the
            // pairing so it is never resumed/activated again. A lifecycle change —
            // persist immediately and fold any pending cursor bump into it (0ef.11).
            let key = pairing_id.as_str().to_string();
            state.pairings.retain(|p| p.pairing_id != key);
            store.save(state);
            gate.mark_clean();
            Sent::Ok
        }
    }
}

/// Remove every pairing that `keeping` supersedes — any OTHER pairing to the
/// SAME phone — from `state`, and return their ids so the caller can tell the
/// relay to revoke them (remote-control-4wk).
///
/// A second pairing to one phone is dead weight the moment a newer one is
/// claimed: [`RemoteBridge`](crate::remote::bridge::RemoteBridge) feeds exactly
/// one pairing, so the older one is served nothing while still authenticating at
/// the relay — and the phone still fans a client out to it and can end up
/// showing ITS stale, unrefreshable session list. Retiring it here is what stops
/// the duplicate accumulating in the first place.
///
/// Identity is `peer_device_id`: `None` retires nothing, because without knowing
/// which phone claimed this pairing we cannot tell a duplicate of the same phone
/// from a legitimate SECOND phone. Pairings whose `peer_device_id` is unknown are
/// likewise left alone.
fn retire_superseded_pairings(
    state: &mut RemoteState,
    keeping: &str,
    peer_device_id: Option<&str>,
) -> Vec<String> {
    let Some(device) = peer_device_id else {
        return Vec::new();
    };
    let superseded: Vec<String> = state
        .pairings
        .iter()
        .filter(|p| p.pairing_id != keeping && p.peer_device_id.as_deref() == Some(device))
        .map(|p| p.pairing_id.clone())
        .collect();
    state
        .pairings
        .retain(|p| !superseded.contains(&p.pairing_id));
    superseded
}

/// Handle one relay→client frame. Returns `false` on a fatal frame (reconnect).
async fn handle_frame(
    sink: &mut RelaySink,
    state: &mut RemoteState,
    store: &dyn RemoteStore,
    gate: &mut CursorFlushGate,
    inbound_tx: &InboundTx,
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
            // Accept a strictly-newer seq (normal dedup, spec §6.4), OR an
            // explicit stream restart: seq 1 while we hold a higher cursor is the
            // peer having lost its outbound cursor and started over, which the
            // relay adopts rather than rejecting (remote-control-arg). A healthy
            // stream is monotonic and never re-emits seq 1, so this can only be a
            // genuine restart — dedup it away and the recovered feed is dropped
            // silently, forever. Mirrors the phone's rule in `TransportClient`.
            let is_reset = env.seq == 1 && last >= 1;
            if env.seq > last || is_reset {
                if let Some(p) = state.pairing_mut(&key) {
                    p.last_received_seq = env.seq;
                }
                // Debounce the receipt-cursor persist (0ef.11). The auto-ack below
                // is sent immediately, so the relay trims its queue regardless of
                // when this cursor reaches disk: a hard-crash rewind just asks the
                // relay (on resume) for envelopes it has already dropped, never
                // re-delivering a duplicate to the app.
                gate.mark_dirty();
                let seq = env.seq;
                let pairing_id = env.pairing_id.clone();
                inbound_tx.send(RemoteInbound::Envelope(env));
                // Auto-ack contiguous receipt so the relay can trim its queue.
                let _ = send_frame(
                    sink,
                    &RelayFrame::Ack {
                        pairing_id,
                        cursor: seq,
                    },
                )
                .await;
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
                    // Debounced: peer-ack high-water is informational (relay queue
                    // trimming), safe to lose the last interval on a crash (0ef.11).
                    gate.mark_dirty();
                }
            }
            // Surface it to the app. `last_acked_by_peer` used to be written here
            // and read NOWHERE — the desktop had no end-to-end evidence that the
            // phone was receiving anything, so it fed a dark phone for 17 days
            // behind a green (relay-`pong`-driven) link indicator
            // (remote-control-5qu). The bridge turns these into an ack deadline.
            // Sent even when the cursor did not advance: an un-advanced ack still
            // proves this relay forwards peer acks, which is what arms the guard.
            inbound_tx.send(RemoteInbound::PeerAck { pairing_id, cursor });
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
            inbound_tx.send(RemoteInbound::Presence {
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
            // persist_pairing_offer wrote the whole state; fold in any pending bump.
            gate.mark_clean();
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
                gate.mark_clean();
            }
            // Retire any OLDER pairing to this same phone (remote-control-4wk).
            // The bridge feeds exactly one pairing, so a second pairing to the
            // same device is dead weight the moment this one is claimed: it
            // still authenticates at the relay and the phone still fans a client
            // out to it, which then displays a session list nothing will ever
            // refresh. Revoking is membership-checked, not role-checked, so a
            // desktop may revoke its own pairing (relay `on_revoke`), and the
            // relay tells the phone via `pairing_revoked` so it drops its record
            // too. Idempotent and best-effort: the local drop below is what
            // matters for THIS desktop, and a failed send is retried by the next
            // claim.
            let superseded = retire_superseded_pairings(
                state,
                pairing_id.as_str(),
                peer_device_id.as_ref().map(|d| d.as_str()),
            );
            if !superseded.is_empty() {
                for stale in &superseded {
                    crate::remote::debuglog::log(&format!(
                        "client REVOKE superseded pairing={stale} (same phone as {})",
                        pairing_id.as_str()
                    ));
                    let _ = send_frame(
                        sink,
                        &RelayFrame::Revoke {
                            pairing_id: PairingId::new(stale.clone()),
                        },
                    )
                    .await;
                }
                store.save(state);
                gate.mark_clean();
            }
            inbound_tx.send(RemoteInbound::PairingClaimed {
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
            gate.mark_clean();
            inbound_tx.send(RemoteInbound::PairingRevoked { pairing_id });
            true
        }
        RelayFrame::Error {
            code: RelayErrorCode::SeqViolation,
            pairing_id,
            expected_seq: Some(next_seq),
            ..
        } => {
            crate::remote::debuglog::log(&format!(
                "client RECV error seq_violation (realign) pairing={:?} next_seq={next_seq}",
                pairing_id.as_ref().map(|p| p.as_str())
            ));
            // The relay is telling us our OUTBOUND numbering ran ahead of its
            // watermark and naming the seq it will accept next. This is the
            // sender-side fault, so nothing about our inbound cursor is wrong —
            // do not touch it, and do not `Resume`. Just realign and re-send a
            // full snapshot, because the peer missed everything we emitted while
            // we were ahead.
            //
            // Rewinding here is safe precisely because `next_seq` comes from the
            // relay: it is `high_water + 1`, so the very next envelope matches
            // and the stream advances. The livelock this used to cause
            // (remote-control-arg) came from rewinding blindly to 1 against a
            // relay whose watermark was already higher — that rewind could never
            // be accepted, so it repeated forever (remote-control-zv3).
            if let Some(pid) = pairing_id {
                // Persist the realigned cursor durably, and NOT through the
                // monotonic bump in `SendEnvelope` (which would refuse to lower
                // it): otherwise `remote.json` keeps the runaway value, and the
                // next launch floors `out_seq` right back to it via
                // `install_channel`, re-entering the rejected state and paying a
                // needless reject→realign round trip on every start.
                if let Some(p) = state.pairing_mut(pid.as_str()) {
                    p.last_sent_seq = next_seq.saturating_sub(1);
                    store.save(state);
                    gate.mark_clean();
                }
                inbound_tx.send(RemoteInbound::SeqRealign {
                    pairing_id: pid,
                    next_seq,
                });
            }
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
            // `seq_violation` means exactly one thing: our INBOUND cursor for
            // this pairing is stale — the peer restarted its outbound stream (or
            // the relay shed envelopes we still needed), so the seqs now arriving
            // sit below what we last saw and our dedup would throw them away.
            // Drop the cursor, re-resume from scratch to pull whatever the relay
            // still holds, and ask for a fresh snapshot. Never fatal: tearing the
            // connection down just reconnects into the same advisory forever.
            //
            // It does NOT mean "rewind your outbound cursor". It used to
            // (remote-control-bbf, when a restarted relay came back with an empty
            // in-memory watermark), and that is precisely what livelocked a phone
            // against a *persistent* relay: the relay's watermark was at 60, the
            // endpoint restarted at 1, and each rejection drove another rewind to
            // 1 (remote-control-arg). The relay now adopts an unknown stream's
            // starting seq and absorbs a genuine rewind itself, so an endpoint
            // never needs to renumber a stream it is successfully sending.
            //
            // A `seq_violation` without a pairing id can't be targeted → ignored.
            if let Some(pid) = pairing_id {
                if let Some(p) = state.pairing_mut(pid.as_str()) {
                    p.last_received_seq = 0;
                    store.save(state);
                    gate.mark_clean();
                }
                let _ = send_frame(
                    sink,
                    &RelayFrame::Resume {
                        pairing_id: pid.clone(),
                        from_seq: 0,
                    },
                )
                .await;
                inbound_tx.send(RemoteInbound::SeqResync { pairing_id: pid });
            }
            true
        }
        // The relay's per-pairing queue overflowed and it shed the oldest
        // un-acked envelope (spec §6 amendment). The queue holds ~1000 un-acked
        // envelopes, so this only happens when the peer has stopped draining
        // ours: it is direct, deployed-today evidence that the phone is not
        // receiving, and it was previously swallowed as a non-fatal advisory
        // while the desktop kept sealing (remote-control-5qu). Not fatal — the
        // connection is fine, the *peer* is not — so the session continues and
        // the bridge closes its per-tick feed instead.
        RelayFrame::Error {
            code: RelayErrorCode::RateLimited,
            pairing_id: Some(pairing_id),
            ref message,
            ..
        } => {
            crate::remote::debuglog::log(&format!(
                "client RECV error rate_limited (peer backlog) pairing={} msg={}",
                pairing_id.as_str(),
                message
            ));
            inbound_tx.send(RemoteInbound::PeerBacklog { pairing_id });
            true
        }
        RelayFrame::Error {
            code,
            ref message,
            ref pairing_id,
            ..
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
