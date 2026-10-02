//! The link: one WebSocket to the controlled instance, on the shared runtime
//! (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md` §2.2, modelled on
//! [`crate::remote::client`]).
//!
//! `Attach` → `Snapshot` → pump, with the browser's reconnect backoff
//! (250 ms → 8 s, `webui/src/wire/socket.ts`). The front-end stays
//! synchronous: frames come out on a `std::sync::mpsc` channel as
//! [`LinkEvent`]s, and everything going in — keystrokes, commands, seat
//! requests — goes through [`Outbound`], which a remote terminal can hold.
//!
//! Three things happen here rather than on the front-end's side, because they
//! must happen *in order with the socket*:
//!
//! * **The replay after a snapshot.** [`InputQueue::on_snapshot`] runs before
//!   the snapshot is forwarded, so nothing typed during the hand-over can
//!   overtake a held keystroke (the host drops a seq at or below its
//!   watermark — an overtaken keystroke would be lost, not late).
//! * **Byte cursors.** Every terminal's `next_offset` is tracked here and sent
//!   on the next `Attach`, and a frame overlapping bytes already delivered is
//!   trimmed, so a reconnect never prints a line twice.
//! * **Stream resets.** A cursor past a terminal's `byte_len` means the host's
//!   stream started over (the host restarted); the cursor goes back to zero,
//!   the front-end is told ([`LinkEvent::StreamReset`]) and the client
//!   re-attaches so the host replays it from the start.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::web::protocol::{
    Ack, Attach, ClientInfo, ClientMsg, ConfigView, Delta, ErrorCode, GitStatusView, SeatRequest,
    ServerMsg, ShutdownReason, Snapshot, TermBytes, TermCursor, TerminalId, ViewerId, WireError,
    PROTOCOL_VERSION,
};
use crate::web::server::COOKIE_NAME;

use super::exchange::AccessToken;
use super::input::{AckFor, InputQueue};

/// The browser's reconnect delays, capped at the last.
pub const RETRY_DELAYS_MS: [u64; 6] = [250, 500, 1000, 2000, 4000, 8000];

/// How long a connect attempt may take before it counts as failed.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// A ping goes out this often; its pong is the latency the status bar shows.
const PING_EVERY: Duration = Duration::from_secs(5);

/// No frame and no pong for this long means the link is dead even though the
/// socket has not said so (a laptop lid closed, Wi-Fi gone). It is dropped and
/// redialled rather than left looking live.
const SILENCE_LIMIT: Duration = Duration::from_secs(20);

/// How long [`LinkHandle::stop`] waits for the task to close the socket.
const STOP_GRACE: Duration = Duration::from_millis(500);

/// Everything the link needs to dial.
#[derive(Clone, Debug)]
pub struct LinkConfig {
    /// `host:port` of the controlled instance's web interface.
    pub address: String,
    /// The access token from [`super::exchange::exchange_code`].
    pub token: AccessToken,
    /// The seat to ask for: [`SeatRequest::Write`] (R5's default) or
    /// [`SeatRequest::Observe`].
    pub seat: SeatRequest,
    /// Sent as `User-Agent` and in `Attach.client`, so the controlled instance
    /// can tell this client from a browser and revoke it by name.
    pub user_agent: String,
}

/// The link's state, for the title and status bars.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// Dialling for the first time.
    Connecting,
    /// Attached. `latency_ms` is the last round trip measured, if any yet.
    Live { latency_ms: Option<u64> },
    /// The link dropped; the next attempt is `attempt`, in `retry_in_ms`.
    Reconnecting { attempt: u32, retry_in_ms: u64 },
    /// The link will not come back by itself.
    Ended(LinkEnd),
}

impl LinkState {
    /// Whether frames can flow right now.
    pub fn is_live(&self) -> bool {
        matches!(self, LinkState::Live { .. })
    }
}

/// Why a link ended for good.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkEnd {
    /// The controlled instance withdrew this client's access.
    Revoked,
    /// The token is not one the host knows (a different host, or its
    /// credentials were reset). A new code is needed.
    Unauthorized,
    /// The person at the host quit FlightDeck.
    HostQuit,
    /// The host stopped its web interface.
    ServerStopped,
    /// The host speaks a different protocol version; one side must update.
    VersionMismatch(String),
    /// This client closed the link.
    Stopped,
}

/// One thing the link has to say, in socket order.
#[derive(Clone, Debug)]
pub enum LinkEvent {
    State(LinkState),
    Snapshot(Box<Snapshot>),
    Delta(Delta),
    /// Terminal bytes, already trimmed against what was delivered before.
    Bytes(TermBytes),
    /// The host's stream for this terminal started over; discard what was
    /// rendered for it. Replayed bytes follow as [`LinkEvent::Bytes`].
    StreamReset(TerminalId),
    Ack(Ack, AckFor),
    GitStatus(GitStatusView),
    Configuration(ConfigView),
    Error(WireError),
}

/// What the client asks the link task to send.
#[derive(Debug)]
pub enum LinkOut {
    /// An already-sequenced frame.
    Frame(ClientMsg),
    /// Re-attach asking for this seat (`take_over` takes the input lock).
    Attach(SeatRequest),
}

impl LinkOut {
    /// The frame, if this is one.
    pub fn into_frame(self) -> Option<ClientMsg> {
        match self {
            LinkOut::Frame(frame) => Some(frame),
            LinkOut::Attach(_) => None,
        }
    }
}

/// The way in: keystrokes, commands and seat requests. Cheap to clone — every
/// remote terminal holds one.
#[derive(Clone, Debug)]
pub struct Outbound {
    queue: Arc<Mutex<InputQueue>>,
    tx: UnboundedSender<LinkOut>,
}

impl Outbound {
    fn new() -> (Outbound, UnboundedReceiver<LinkOut>) {
        let (tx, rx) = unbounded_channel();
        (
            Outbound {
                queue: Arc::new(Mutex::new(InputQueue::new())),
                tx,
            },
            rx,
        )
    }

    /// An outbound with no task behind it — for tests that read what would
    /// have been sent off the receiver.
    pub fn detached() -> (Outbound, UnboundedReceiver<LinkOut>) {
        Outbound::new()
    }

    /// Mark the queue live as a snapshot would, for tests of [`Outbound::detached`].
    pub fn go_live_for_test(&self) {
        self.lock().on_snapshot(0);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, InputQueue> {
        self.queue.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Keystrokes for `terminal_id`: sent now when live, held otherwise and
    /// replayed after the next snapshot.
    pub fn input(&self, terminal_id: TerminalId, data: Vec<u8>) {
        // The lock is held across the send so channel order is seq order.
        let mut queue = self.lock();
        if let Some(frame) = queue.push(terminal_id, data) {
            let _ = self.tx.send(LinkOut::Frame(frame));
        }
    }

    /// A command. Returns the seq it went out with, or `None` when the link is
    /// down (commands are not held — see [`InputQueue::command`]).
    pub fn command(&self, name: &str, args: Option<serde_json::Value>) -> Option<u64> {
        let mut queue = self.lock();
        let frame = queue.command(name, args)?;
        let seq = queue.last_seq();
        let _ = self.tx.send(LinkOut::Frame(frame));
        Some(seq)
    }

    /// Ask for a different seat.
    pub fn attach(&self, seat: SeatRequest) {
        let _ = self.tx.send(LinkOut::Attach(seat));
    }

    /// Keystroke bursts still waiting for an ack.
    pub fn held_len(&self) -> usize {
        self.lock().held_len()
    }
}

/// The task → client channel, `Sync` for the same reason as
/// [`crate::remote::client`]'s `InboundTx`: a `&Sender` held across an await
/// would make the task's future non-`Send`.
struct EventTx(Mutex<Sender<LinkEvent>>);

impl EventTx {
    fn send(&self, event: LinkEvent) {
        if let Ok(tx) = self.0.lock() {
            let _ = tx.send(event);
        }
    }
}

/// A running link. Dropping it, or [`LinkHandle::stop`], closes the socket.
pub struct LinkHandle {
    stop: Option<watch::Sender<bool>>,
    done: Option<std::sync::mpsc::Receiver<()>>,
}

impl LinkHandle {
    /// Dial `config` on the shared runtime. Events arrive on `events`; send
    /// through the returned [`Outbound`].
    pub fn start(config: LinkConfig, events: Sender<LinkEvent>) -> (LinkHandle, Outbound) {
        let (outbound, out_rx) = Outbound::new();
        let Some(runtime) = crate::remote::runtime::try_shared() else {
            let _ = events.send(LinkEvent::State(LinkState::Ended(LinkEnd::Stopped)));
            return (
                LinkHandle {
                    stop: None,
                    done: None,
                },
                outbound,
            );
        };
        let (stop_tx, stop_rx) = watch::channel(false);
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let queue = Arc::clone(&outbound.queue);
        runtime.spawn(async move {
            let _done = done_tx;
            run(config, queue, out_rx, EventTx(Mutex::new(events)), stop_rx).await;
        });
        (
            LinkHandle {
                stop: Some(stop_tx),
                done: Some(done_rx),
            },
            outbound,
        )
    }

    /// Close the link and wait briefly for the socket to be closed.
    pub fn stop(mut self) {
        self.stop_now();
    }

    fn stop_now(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(true);
        }
        if let Some(done) = self.done.take() {
            let _ = done.recv_timeout(STOP_GRACE);
        }
    }
}

impl Drop for LinkHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(true);
        }
    }
}

/// The delay before reconnect attempt `attempt` (1-based).
pub fn retry_delay_ms(attempt: u32) -> u64 {
    let index = (attempt.saturating_sub(1) as usize).min(RETRY_DELAYS_MS.len() - 1);
    RETRY_DELAYS_MS[index]
}

// ---------------------------------------------------------------------------
// The task
// ---------------------------------------------------------------------------

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Why a dial failed.
enum DialError {
    /// 401 on the upgrade: the token is no good, and redialling will not help.
    Refused(LinkEnd),
    /// 429: wait this long.
    RateLimited(u64),
    /// Anything else — unreachable, reset, timed out. Retried.
    Network,
}

/// How one attached session ended.
enum SessionEnd {
    /// For good.
    Ended(LinkEnd),
    /// The host is restarting: redial straight away.
    Restarting,
    /// The socket dropped: redial with backoff.
    Dropped,
}

/// The state that outlives one socket.
struct Carry {
    cursors: HashMap<TerminalId, u64>,
    viewer: Option<ViewerId>,
    seat: SeatRequest,
}

async fn run(
    config: LinkConfig,
    queue: Arc<Mutex<InputQueue>>,
    mut out_rx: UnboundedReceiver<LinkOut>,
    events: EventTx,
    mut stop: watch::Receiver<bool>,
) {
    let mut carry = Carry {
        cursors: HashMap::new(),
        viewer: None,
        seat: settle_seat(config.seat),
    };
    let mut attempt: u32 = 0;
    events.send(LinkEvent::State(LinkState::Connecting));
    loop {
        let dialled = tokio::select! {
            dialled = dial(&config) => dialled,
            _ = stopped(&mut stop) => {
                events.send(LinkEvent::State(LinkState::Ended(LinkEnd::Stopped)));
                return;
            }
        };
        let delay_ms = match dialled {
            Err(DialError::Refused(end)) => {
                events.send(LinkEvent::State(LinkState::Ended(end)));
                return;
            }
            Err(DialError::RateLimited(retry_after_ms)) => {
                attempt += 1;
                retry_after_ms.max(retry_delay_ms(attempt))
            }
            Err(DialError::Network) => {
                attempt += 1;
                retry_delay_ms(attempt)
            }
            Ok(ws) => {
                let end = session(
                    ws,
                    &config,
                    &queue,
                    &mut out_rx,
                    &events,
                    &mut stop,
                    &mut carry,
                    &mut attempt,
                )
                .await;
                lock(&queue).on_disconnect();
                // Whatever was still queued for the dead socket is stale: held
                // keystrokes live in the queue and are replayed after the next
                // snapshot, and commands are deliberately not carried over.
                while let Ok(out) = out_rx.try_recv() {
                    if let LinkOut::Attach(seat) = out {
                        carry.seat = settle_seat(seat);
                    }
                }
                match end {
                    SessionEnd::Ended(end) => {
                        events.send(LinkEvent::State(LinkState::Ended(end)));
                        return;
                    }
                    SessionEnd::Restarting => {
                        attempt = 1;
                        RETRY_DELAYS_MS[0]
                    }
                    SessionEnd::Dropped => {
                        attempt += 1;
                        retry_delay_ms(attempt)
                    }
                }
            }
        };
        events.send(LinkEvent::State(LinkState::Reconnecting {
            attempt,
            retry_in_ms: delay_ms,
        }));
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => {}
            _ = stopped(&mut stop) => {
                events.send(LinkEvent::State(LinkState::Ended(LinkEnd::Stopped)));
                return;
            }
        }
    }
}

/// `take_over` is a one-shot act; the seat it leaves behind is a writer.
fn settle_seat(requested: SeatRequest) -> SeatRequest {
    match requested {
        SeatRequest::TakeOver => SeatRequest::Write,
        seat => seat,
    }
}

fn lock(queue: &Arc<Mutex<InputQueue>>) -> std::sync::MutexGuard<'_, InputQueue> {
    queue.lock().unwrap_or_else(|p| p.into_inner())
}

async fn dial(config: &LinkConfig) -> Result<Ws, DialError> {
    let mut request = format!("ws://{}/ws", config.address)
        .into_client_request()
        .map_err(|_| DialError::Network)?;
    let headers = request.headers_mut();
    let cookie = format!("{COOKIE_NAME}={}", config.token.reveal());
    headers.insert(
        "cookie",
        cookie
            .parse()
            .map_err(|_| DialError::Refused(LinkEnd::Unauthorized))?,
    );
    if let Ok(agent) = config.user_agent.parse() {
        headers.insert("user-agent", agent);
    }
    match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request)).await {
        Err(_) => Err(DialError::Network),
        Ok(Ok((ws, _response))) => Ok(ws),
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) => {
            let body: serde_json::Value = response
                .body()
                .as_deref()
                .and_then(|b| serde_json::from_slice(b).ok())
                .unwrap_or(serde_json::Value::Null);
            match response.status().as_u16() {
                401 | 403 => Err(DialError::Refused(
                    if body["reason"].as_str() == Some("token_revoked") {
                        LinkEnd::Revoked
                    } else {
                        LinkEnd::Unauthorized
                    },
                )),
                429 => Err(DialError::RateLimited(
                    body["retry_after_ms"].as_u64().unwrap_or(1_000),
                )),
                _ => Err(DialError::Network),
            }
        }
        Ok(Err(_)) => Err(DialError::Network),
    }
}

fn attach_frame(config: &LinkConfig, carry: &Carry, seat: SeatRequest) -> ClientMsg {
    let mut cursors: Vec<TermCursor> = carry
        .cursors
        .iter()
        .map(|(terminal_id, next_offset)| TermCursor {
            terminal_id: terminal_id.clone(),
            next_offset: *next_offset,
        })
        .collect();
    cursors.sort_by(|a, b| a.terminal_id.cmp(&b.terminal_id));
    ClientMsg::Attach(Attach {
        protocol_version: PROTOCOL_VERSION,
        seat,
        cursors,
        resume_viewer: carry.viewer.clone(),
        viewport: None,
        client: Some(ClientInfo {
            user_agent: Some(config.user_agent.clone()),
            label: Some(config.user_agent.clone()),
        }),
    })
}

async fn send_frame(
    sink: &mut futures_util::stream::SplitSink<Ws, Message>,
    frame: &ClientMsg,
) -> bool {
    match serde_json::to_string(frame) {
        Ok(json) => sink.send(Message::Text(json.into())).await.is_ok(),
        Err(_) => true,
    }
}

#[allow(clippy::too_many_arguments)]
async fn session(
    ws: Ws,
    config: &LinkConfig,
    queue: &Arc<Mutex<InputQueue>>,
    out_rx: &mut UnboundedReceiver<LinkOut>,
    events: &EventTx,
    stop: &mut watch::Receiver<bool>,
    carry: &mut Carry,
    attempt: &mut u32,
) -> SessionEnd {
    let (mut sink, mut stream) = ws.split();
    if !send_frame(&mut sink, &attach_frame(config, carry, carry.seat)).await {
        return SessionEnd::Dropped;
    }
    let mut attach_sent = Some(Instant::now());
    let mut last_heard = Instant::now();
    let mut ping_sent: Option<Instant> = None;
    let mut latency_ms: Option<u64> = None;
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;

    loop {
        tokio::select! {
            _ = stopped(stop) => {
                let _ = sink.send(Message::Close(None)).await;
                return SessionEnd::Ended(LinkEnd::Stopped);
            }
            message = stream.next() => {
                let text = match message {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return SessionEnd::Dropped,
                    Some(Ok(Message::Pong(_))) => {
                        last_heard = Instant::now();
                        if let Some(sent) = ping_sent.take() {
                            latency_ms = Some(sent.elapsed().as_millis() as u64);
                            if lock(queue).is_live() {
                                events.send(LinkEvent::State(LinkState::Live { latency_ms }));
                            }
                        }
                        continue;
                    }
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(_)) => {
                        last_heard = Instant::now();
                        continue;
                    }
                };
                last_heard = Instant::now();
                let Ok(frame) = serde_json::from_str::<ServerMsg>(text.as_str()) else {
                    continue;
                };
                match frame {
                    ServerMsg::Snapshot(snapshot) => {
                        if snapshot.protocol_version != PROTOCOL_VERSION {
                            return SessionEnd::Ended(LinkEnd::VersionMismatch(format!(
                                "The host speaks web protocol v{}; this app speaks v{}.",
                                snapshot.protocol_version, PROTOCOL_VERSION
                            )));
                        }
                        let resets = reconcile_cursors(&mut carry.cursors, &snapshot);
                        carry.viewer = Some(snapshot.viewer_id.clone());
                        if let Some(sent) = attach_sent.take() {
                            latency_ms = Some(sent.elapsed().as_millis() as u64);
                        }
                        *attempt = 0;
                        let replay = lock(queue).on_snapshot(snapshot.last_input_seq);
                        for frame in &replay {
                            if !send_frame(&mut sink, frame).await {
                                return SessionEnd::Dropped;
                            }
                        }
                        events.send(LinkEvent::State(LinkState::Live { latency_ms }));
                        events.send(LinkEvent::Snapshot(snapshot));
                        if !resets.is_empty() {
                            for id in resets {
                                events.send(LinkEvent::StreamReset(id));
                            }
                            // Ask again with the corrected cursors, so the host
                            // replays the restarted streams from their start.
                            attach_sent = Some(Instant::now());
                            if !send_frame(&mut sink, &attach_frame(config, carry, carry.seat)).await {
                                return SessionEnd::Dropped;
                            }
                        }
                    }
                    ServerMsg::TermBytes(bytes) => {
                        if let Some(bytes) = trim_to_cursor(&mut carry.cursors, bytes) {
                            events.send(LinkEvent::Bytes(bytes));
                        }
                    }
                    ServerMsg::Delta(delta) => events.send(LinkEvent::Delta(delta)),
                    ServerMsg::Ack(ack) => {
                        let what = lock(queue).on_ack(ack.seq, ack.outcome);
                        events.send(LinkEvent::Ack(ack, what));
                    }
                    ServerMsg::GitStatus(view) => events.send(LinkEvent::GitStatus(view)),
                    ServerMsg::Configuration(view) => events.send(LinkEvent::Configuration(view)),
                    ServerMsg::Error(error) => {
                        if error.code == ErrorCode::VersionMismatch {
                            return SessionEnd::Ended(LinkEnd::VersionMismatch(error.message));
                        }
                        if let Some(seq) = error.seq {
                            lock(queue).forget_command(seq);
                        }
                        events.send(LinkEvent::Error(error));
                    }
                    ServerMsg::Shutdown { reason, .. } => {
                        return match reason {
                            ShutdownReason::Restarting | ShutdownReason::Unknown => {
                                SessionEnd::Restarting
                            }
                            ShutdownReason::TokenRevoked => SessionEnd::Ended(LinkEnd::Revoked),
                            ShutdownReason::HostQuit => SessionEnd::Ended(LinkEnd::HostQuit),
                            ShutdownReason::ServerStopped => {
                                SessionEnd::Ended(LinkEnd::ServerStopped)
                            }
                        };
                    }
                    ServerMsg::Unrecognized => {}
                }
            }
            out = out_rx.recv() => {
                let sent = match out {
                    // The client dropped its sender: it is gone.
                    None => {
                        let _ = sink.send(Message::Close(None)).await;
                        return SessionEnd::Ended(LinkEnd::Stopped);
                    }
                    Some(LinkOut::Frame(frame)) => send_frame(&mut sink, &frame).await,
                    Some(LinkOut::Attach(seat)) => {
                        let frame = attach_frame(config, carry, seat);
                        carry.seat = settle_seat(seat);
                        attach_sent = Some(Instant::now());
                        send_frame(&mut sink, &frame).await
                    }
                };
                if !sent {
                    return SessionEnd::Dropped;
                }
            }
            _ = ping.tick() => {
                if last_heard.elapsed() > SILENCE_LIMIT {
                    return SessionEnd::Dropped;
                }
                ping_sent = Some(Instant::now());
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return SessionEnd::Dropped;
                }
            }
        }
    }
}

/// Drop cursors for terminals the host no longer has, and reset any cursor
/// that is past its terminal's `byte_len` — that stream started over.
fn reconcile_cursors(
    cursors: &mut HashMap<TerminalId, u64>,
    snapshot: &Snapshot,
) -> Vec<TerminalId> {
    let mut lens = HashMap::new();
    for terminal in snapshot
        .projects
        .iter()
        .flat_map(|p| p.sessions.iter())
        .flat_map(|s| s.terminals.iter())
    {
        lens.insert(terminal.terminal_id.clone(), terminal.byte_len);
    }
    cursors.retain(|id, _| lens.contains_key(id));
    let mut resets: Vec<TerminalId> = cursors
        .iter()
        .filter(|(id, cursor)| lens.get(*id).is_some_and(|len| **cursor > *len))
        .map(|(id, _)| id.clone())
        .collect();
    resets.sort();
    for id in &resets {
        cursors.insert(id.clone(), 0);
    }
    resets
}

/// Advance `terminal_id`'s cursor past `bytes`, trimming any prefix already
/// delivered. `None` when every byte was a repeat.
fn trim_to_cursor(
    cursors: &mut HashMap<TerminalId, u64>,
    mut bytes: TermBytes,
) -> Option<TermBytes> {
    let end = bytes.next_offset();
    let cursor = cursors.get(&bytes.terminal_id).copied().unwrap_or(0);
    if end <= cursor && !bytes.data.is_empty() && cursors.contains_key(&bytes.terminal_id) {
        return None;
    }
    if bytes.offset < cursor && cursors.contains_key(&bytes.terminal_id) {
        let skip = (cursor - bytes.offset) as usize;
        bytes.data.drain(..skip.min(bytes.data.len()));
        bytes.offset = cursor;
    }
    cursors.insert(bytes.terminal_id.clone(), end.max(cursor));
    (!bytes.data.is_empty()).then_some(bytes)
}

/// Resolve once shutdown has been requested, or the handle dropped.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(id: &str, offset: u64, data: &[u8]) -> TermBytes {
        TermBytes::live(id.into(), offset, data.to_vec())
    }

    #[test]
    fn a_resumed_frame_overlapping_delivered_bytes_is_trimmed() {
        let mut cursors = HashMap::new();
        assert_eq!(
            trim_to_cursor(&mut cursors, bytes("t", 0, b"hello"))
                .unwrap()
                .data,
            b"hello"
        );
        let trimmed = trim_to_cursor(&mut cursors, bytes("t", 3, b"lo world")).unwrap();
        assert_eq!(
            (trimmed.offset, trimmed.data.as_slice()),
            (5, &b" world"[..])
        );
        assert!(trim_to_cursor(&mut cursors, bytes("t", 0, b"hello")).is_none());
        assert_eq!(cursors[&TerminalId::from("t")], 11);
    }

    #[test]
    fn a_truncated_replay_jumps_the_cursor_forward() {
        let mut cursors = HashMap::new();
        trim_to_cursor(&mut cursors, bytes("t", 0, b"ab"));
        let mut frame = bytes("t", 100, b"cd");
        frame.truncated = true;
        let delivered = trim_to_cursor(&mut cursors, frame).unwrap();
        assert!(delivered.truncated);
        assert_eq!(cursors[&TerminalId::from("t")], 102);
    }

    #[test]
    fn the_backoff_matches_the_browser_and_caps_at_eight_seconds() {
        let delays: Vec<u64> = (1..=8).map(retry_delay_ms).collect();
        assert_eq!(delays, vec![250, 500, 1000, 2000, 4000, 8000, 8000, 8000]);
    }
}
