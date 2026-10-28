//! The direct link's listeners and per-connection sessions (spec §5.10).
//!
//! A session is the relay's role, played by the Mac for its own pairings, over
//! the same `RelayFrame` JSON:
//!
//! ```text
//!   phone → hello { role: phone, … }        mac → hello_ok { relay_mode omitted }
//!                                           mac → auth_challenge { nonce }
//!   phone → auth_response { signature over "flightdeck-remote-direct-v1:" ‖ nonce }
//!                                           mac → auth_ok
//!                                           mac → machine_name, peer_presence (desktop connected)
//!   phone → resume { from_seq }             mac → envelope … (replay, then live)
//!   envelope ⇄ / ack / ping→pong / revoke→pairing_revoked / bye
//! ```
//!
//! Bootstrap and push frames are not served here (`bad_frame`): pairing and
//! pushes stay on the relay (spec §5.9). Every failure to authenticate is
//! `error { code: auth_failed }` followed by a close.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rand_core::{OsRng, RngCore};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{http, Message};
use tokio_tungstenite::WebSocketStream;

use flightdeck_remote_protocol::relay::DIRECT_LINK_PATH;
use flightdeck_remote_protocol::{
    negotiate_version, DirectEndpoint, PairingId, PresenceState, RelayErrorCode, RelayFrame, Role,
    MAX_SUPPORTED_VERSION, MIN_SUPPORTED_VERSION,
};

use super::net::{advertised_host, select_bind_addresses, tailscale_addresses, AddressOrigin};
use super::outbox::ResumeOutcome;
use super::{BindPlan, Shared, FLUSH_INTERVAL};
use crate::remote::runtime::SharedRuntime;
use crate::remote::RemoteInbound;

/// How often the Tailscale plan re-enumerates interfaces.
pub(super) const REBIND_INTERVAL: Duration = Duration::from_secs(30);
/// Budget for the WebSocket upgrade.
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(10);
/// Budget from upgrade to a verified `auth_response`.
const AUTH_TIMEOUT: Duration = Duration::from_secs(15);
/// Tear a session down when nothing at all arrives for this long. The phone
/// pings every ~20 s (spec §5.4); matches the relay's idle sweep.
const LIVENESS_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound on every write so a wedged socket cannot park a session.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Concurrent connections served, authenticated or not. One phone needs one;
/// the cap only stops a misbehaving peer on the tailnet from exhausting tasks.
const MAX_CONNECTIONS: usize = 16;
/// Largest inbound WebSocket message. Phone→desktop envelopes are small
/// commands; desktop→phone ones are not bounded by this.
const MAX_INBOUND_MESSAGE: usize = 1024 * 1024;

type Ws = WebSocketStream<TcpStream>;
type Sink = SplitSink<Ws, Message>;
type Stream = SplitStream<Ws>;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Spawn the listener supervisor and the debounced outbox flusher.
pub(super) fn start(
    runtime: &'static SharedRuntime,
    shared: Arc<Shared>,
    plan: BindPlan,
    stop: watch::Receiver<bool>,
) {
    // Fixed addresses bind synchronously so the caller can read the bound
    // ports right away (tests bind port 0).
    let fixed = match &plan {
        BindPlan::Fixed(addrs) => addrs
            .iter()
            .filter_map(|a| bind_std(*a).ok())
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    if !fixed.is_empty() {
        let addrs: Vec<SocketAddr> = fixed.iter().filter_map(|l| l.local_addr().ok()).collect();
        if let Ok(mut b) = shared.bound.lock() {
            *b = addrs.clone();
        }
        shared.set_advert(addrs.first().map(|a| DirectEndpoint {
            host: a.ip().to_string(),
            port: a.port(),
        }));
    }
    let flusher = shared.clone();
    let mut flush_stop = stop.clone();
    runtime.spawn(async move {
        let mut tick = tokio::time::interval(FLUSH_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = stopped(&mut flush_stop) => break,
                _ = tick.tick() => {
                    let f = flusher.clone();
                    // The write is blocking file I/O; keep it off the workers.
                    let _ = tokio::task::spawn_blocking(move || f.flush(false)).await;
                }
            }
        }
    });
    runtime.spawn(supervise(shared, plan, fixed, stop));
}

/// Bind one address with `std` (non-blocking, ready for tokio). `std` sets
/// `SO_REUSEADDR` on Unix, so a quick restart can rebind the same port.
fn bind_std(addr: SocketAddr) -> std::io::Result<std::net::TcpListener> {
    let l = std::net::TcpListener::bind(addr)?;
    l.set_nonblocking(true)?;
    Ok(l)
}

/// Resolve as soon as shutdown is requested (or the link is dropped).
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

/// Run the listeners for `plan` until stopped.
async fn supervise(
    shared: Arc<Shared>,
    plan: BindPlan,
    fixed: Vec<std::net::TcpListener>,
    mut stop: watch::Receiver<bool>,
) {
    let conns = Arc::new(AtomicUsize::new(0));
    let mut listeners: HashMap<IpAddr, JoinHandle<()>> = HashMap::new();
    match plan {
        BindPlan::None => {}
        BindPlan::Fixed(_) => {
            for l in fixed {
                if let Ok(addr) = l.local_addr() {
                    if let Ok(tl) = TcpListener::from_std(l) {
                        listeners.insert(
                            addr.ip(),
                            tokio::spawn(accept_loop(
                                tl,
                                shared.clone(),
                                conns.clone(),
                                stop.clone(),
                            )),
                        );
                    }
                }
            }
        }
        BindPlan::Tailscale {
            port,
            host_override,
            source,
            probe,
        } => {
            let mut last_origin: Option<AddressOrigin> = None;
            loop {
                // Tailscale's own word on its addresses first (a carrier-grade
                // NAT 100.x address is in the same range), the range check only
                // when the CLI cannot be asked; see `net` on why.
                let src = source.clone();
                let p = probe.clone();
                let (wanted, origin, magic) = tokio::task::spawn_blocking(move || {
                    let status = p.status();
                    let (wanted, origin) = select_bind_addresses(
                        &src.addresses(),
                        status.as_ref().map(|s| s.ips.as_slice()),
                    );
                    (wanted, origin, status.and_then(|s| s.magic_dns))
                })
                .await
                .unwrap_or_else(|_| (Vec::new(), AddressOrigin::RangeFallback, None));
                if last_origin != Some(origin) {
                    crate::remote::debuglog::log(&format!(
                        "direct ADDRESSES from {} → {wanted:?}",
                        origin.label()
                    ));
                    last_origin = Some(origin);
                }
                let before: HashSet<IpAddr> = listeners.keys().copied().collect();
                listeners.retain(|ip, task| {
                    let keep = wanted.contains(ip);
                    if !keep {
                        task.abort();
                    }
                    keep
                });
                for ip in &wanted {
                    if listeners.contains_key(ip) {
                        continue;
                    }
                    match bind_std(SocketAddr::new(*ip, port)).and_then(TcpListener::from_std) {
                        Ok(tl) => {
                            crate::remote::debuglog::log(&format!("direct LISTEN {ip}:{port}"));
                            listeners.insert(
                                *ip,
                                tokio::spawn(accept_loop(
                                    tl,
                                    shared.clone(),
                                    conns.clone(),
                                    stop.clone(),
                                )),
                            );
                        }
                        Err(e) => crate::remote::debuglog::log(&format!(
                            "direct LISTEN failed {ip}:{port} err={e}"
                        )),
                    }
                }
                let now: HashSet<IpAddr> = listeners.keys().copied().collect();
                if now != before {
                    // Re-sorted into the stable IPv4-first order the advert picks from.
                    let bound = tailscale_addresses(&now.iter().copied().collect::<Vec<_>>());
                    if let Ok(mut b) = shared.bound.lock() {
                        *b = bound.iter().map(|ip| SocketAddr::new(*ip, port)).collect();
                    }
                    let magic = if bound.is_empty() || !host_override.trim().is_empty() {
                        None
                    } else {
                        magic.clone()
                    };
                    shared.set_advert(
                        advertised_host(&host_override, magic.as_deref(), &bound)
                            .map(|host| DirectEndpoint { host, port }),
                    );
                }
                tokio::select! {
                    _ = stopped(&mut stop) => break,
                    _ = sleep(REBIND_INTERVAL) => {}
                }
            }
        }
    }
    stopped(&mut stop).await;
    for (_, task) in listeners {
        task.abort();
    }
}

/// Accept connections on one listener until stopped.
async fn accept_loop(
    listener: TcpListener,
    shared: Arc<Shared>,
    conns: Arc<AtomicUsize>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        let accepted = tokio::select! {
            _ = stopped(&mut stop) => return,
            a = listener.accept() => a,
        };
        let Ok((tcp, peer)) = accepted else {
            // Transient accept errors (EMFILE, a reset in the backlog): back off
            // briefly instead of spinning.
            sleep(Duration::from_millis(100)).await;
            continue;
        };
        if conns.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            conns.fetch_sub(1, Ordering::SeqCst);
            drop(tcp);
            continue;
        }
        let _ = tcp.set_nodelay(true);
        let shared = shared.clone();
        let conns = conns.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            crate::remote::debuglog::log(&format!("direct ACCEPT {peer}"));
            run_connection(shared, tcp, stop).await;
            conns.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

async fn send(sink: &mut Sink, frame: &RelayFrame) -> bool {
    let json = match serde_json::to_string(frame) {
        Ok(j) => j,
        Err(_) => return false,
    };
    matches!(
        timeout(WRITE_TIMEOUT, sink.send(Message::Text(json.into()))).await,
        Ok(Ok(()))
    )
}

async fn send_error(
    sink: &mut Sink,
    code: RelayErrorCode,
    message: &str,
    pairing_id: Option<PairingId>,
) -> bool {
    send(
        sink,
        &RelayFrame::Error {
            code,
            message: message.to_string(),
            pairing_id,
            expected_seq: None,
        },
    )
    .await
}

async fn close(sink: &mut Sink) {
    let _ = timeout(WRITE_TIMEOUT, sink.close()).await;
}

/// One read: a parsed frame, an unparsable message (`Malformed`), noise we
/// skip (control/binary frames), or the end of the connection.
enum Incoming {
    Frame(Box<RelayFrame>),
    Malformed,
    Skip,
    Closed,
}

async fn read_next(stream: &mut Stream) -> Incoming {
    match stream.next().await {
        Some(Ok(Message::Text(text))) => match serde_json::from_str::<RelayFrame>(text.as_str()) {
            Ok(f) => Incoming::Frame(Box::new(f)),
            Err(_) => Incoming::Malformed,
        },
        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => Incoming::Closed,
        Some(Ok(_)) => Incoming::Skip,
    }
}

/// [`read_next`] bounded by an absolute deadline (a timeout reads as closed).
async fn read_until(stream: &mut Stream, deadline: tokio::time::Instant) -> Incoming {
    tokio::time::timeout_at(deadline, read_next(stream))
        .await
        .unwrap_or(Incoming::Closed)
}

/// Frames that belong to pairing bootstrap or push, which the direct link does
/// not serve (spec §5.10): answered `bad_frame`.
fn is_bootstrap_or_push(frame: &RelayFrame) -> bool {
    matches!(
        frame,
        RelayFrame::PairingOffer { .. }
            | RelayFrame::PairingClaim { .. }
            | RelayFrame::RegisterPushToken { .. }
            | RelayFrame::UnregisterPushToken { .. }
            | RelayFrame::PushNotify(_)
    )
}

/// The upgrade callback: only `DIRECT_LINK_PATH` upgrades; anything else is a
/// 404. A trait impl rather than a closure because the signature (and its large
/// error type) is tungstenite's, not ours.
struct PathCheck;

impl Callback for PathCheck {
    fn on_request(self, request: &Request, response: Response) -> Result<Response, ErrorResponse> {
        if request.uri().path() == DIRECT_LINK_PATH {
            Ok(response)
        } else {
            let mut err = ErrorResponse::new(Some("not found".to_string()));
            *err.status_mut() = http::StatusCode::NOT_FOUND;
            Err(err)
        }
    }
}

/// Upgrade, authenticate, then pump one phone connection.
async fn run_connection(shared: Arc<Shared>, tcp: TcpStream, stop: watch::Receiver<bool>) {
    let config = WebSocketConfig::default().max_message_size(Some(MAX_INBOUND_MESSAGE));
    let ws = match timeout(
        UPGRADE_TIMEOUT,
        tokio_tungstenite::accept_hdr_async_with_config(tcp, PathCheck, Some(config)),
    )
    .await
    {
        Ok(Ok(ws)) => ws,
        _ => return,
    };
    let (mut sink, mut stream) = ws.split();
    let Some((device_id, pairings)) = handshake(&shared, &mut sink, &mut stream).await else {
        close(&mut sink).await;
        return;
    };
    let session = shared.new_session_id();
    // Watch the registry before `attach` (which bumps it), so the pump's first
    // turn re-checks every pairing: an unpair landing while we announce is
    // still answered with `pairing_revoked` instead of slipping past unseen.
    let ticks = Ticks {
        outbox: shared.outbox_tick.subscribe(),
        registry: shared.registry_tick.subscribe(),
    };
    crate::remote::debuglog::log(&format!(
        "direct AUTH ok session={session} pairings={:?}",
        pairings.iter().map(|p| p.as_str()).collect::<Vec<_>>()
    ));
    // Announce, then take over the pairings (the app hears Paired/Presence).
    let machine = match shared.machine_name.clone() {
        Some(name) => Some(name),
        None => tokio::task::spawn_blocking(crate::remote::client::machine_name)
            .await
            .unwrap_or(None),
    };
    for pid in &pairings {
        if let Some(name) = &machine {
            send(
                &mut sink,
                &RelayFrame::MachineName {
                    pairing_id: pid.clone(),
                    machine_name: name.clone(),
                },
            )
            .await;
        }
        send(
            &mut sink,
            &RelayFrame::PeerPresence {
                pairing_id: pid.clone(),
                peer: Role::Desktop,
                state: PresenceState::Connected,
                at_ms: now_ms(),
            },
        )
        .await;
    }
    shared.attach(session, &pairings, &device_id);
    let active = pump(
        &shared,
        session,
        pairings,
        ticks,
        &mut sink,
        &mut stream,
        stop,
    )
    .await;
    shared.detach(session, &active);
    close(&mut sink).await;
    crate::remote::debuglog::log(&format!("direct END session={session}"));
}

/// `hello` → `hello_ok` → `auth_challenge` → verified `auth_response` →
/// `auth_ok`. Returns the phone's device id and the pairings it activated, or
/// `None` after answering the failure.
async fn handshake(
    shared: &Shared,
    sink: &mut Sink,
    stream: &mut Stream,
) -> Option<(String, Vec<PairingId>)> {
    let deadline = tokio::time::Instant::now() + AUTH_TIMEOUT;

    // hello
    let hello = loop {
        match read_until(stream, deadline).await {
            Incoming::Frame(f) => break *f,
            Incoming::Skip => continue,
            Incoming::Malformed => {
                send_error(sink, RelayErrorCode::BadFrame, "malformed frame", None).await;
                return None;
            }
            Incoming::Closed => return None,
        }
    };
    let RelayFrame::Hello {
        protocol_version,
        role,
        device_id,
        ..
    } = hello
    else {
        send_error(sink, RelayErrorCode::BadFrame, "expected hello", None).await;
        return None;
    };
    let Some(version) = negotiate_version(
        MIN_SUPPORTED_VERSION,
        MAX_SUPPORTED_VERSION,
        protocol_version,
    ) else {
        send(
            sink,
            &RelayFrame::VersionIncompatible {
                your_version: protocol_version,
                min_supported: MIN_SUPPORTED_VERSION,
                max_supported: MAX_SUPPORTED_VERSION,
            },
        )
        .await;
        return None;
    };
    if role != Role::Phone {
        send_error(
            sink,
            RelayErrorCode::AuthFailed,
            "the direct link serves phones only",
            None,
        )
        .await;
        return None;
    }
    let mut connection = [0u8; 8];
    OsRng.fill_bytes(&mut connection);
    let mut nonce = [0u8; 32];
    OsRng.fill_bytes(&mut nonce);
    let greeted = send(
        sink,
        &RelayFrame::HelloOk {
            protocol_version: version,
            server_time_ms: now_ms(),
            connection_id: format!(
                "direct-{}",
                connection
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ),
            relay_mode: None,
        },
    )
    .await
        && send(
            sink,
            &RelayFrame::AuthChallenge {
                nonce: STANDARD.encode(nonce),
                server_time_ms: now_ms(),
            },
        )
        .await;
    if !greeted {
        return None;
    }

    // auth_response
    let (claimed, signature, pairing_ids) = loop {
        match read_until(stream, deadline).await {
            Incoming::Frame(f) => match *f {
                RelayFrame::AuthResponse {
                    device_id,
                    signature,
                    pairing_ids,
                    ..
                } => break (device_id, signature, pairing_ids),
                RelayFrame::Ping { client_time_ms } => {
                    send(
                        sink,
                        &RelayFrame::Pong {
                            client_time_ms,
                            server_time_ms: now_ms(),
                        },
                    )
                    .await;
                }
                other if is_bootstrap_or_push(&other) => {
                    send_error(
                        sink,
                        RelayErrorCode::BadFrame,
                        "not served on the direct link",
                        None,
                    )
                    .await;
                }
                _ => {
                    send_error(
                        sink,
                        RelayErrorCode::NotAuthenticated,
                        "authenticate first",
                        None,
                    )
                    .await;
                }
            },
            Incoming::Skip => continue,
            Incoming::Malformed => {
                send_error(sink, RelayErrorCode::BadFrame, "malformed frame", None).await;
            }
            Incoming::Closed => return None,
        }
    };
    let verdict = if claimed != device_id {
        Err("device id does not match hello")
    } else {
        shared.authenticate(device_id.as_str(), &pairing_ids, &nonce, &signature)
    };
    if let Err(why) = verdict {
        crate::remote::debuglog::log(&format!(
            "direct AUTH failed device={} why={why}",
            device_id.as_str()
        ));
        send_error(sink, RelayErrorCode::AuthFailed, why, None).await;
        return None;
    }
    if !send(
        sink,
        &RelayFrame::AuthOk {
            pairing_ids: pairing_ids.clone(),
        },
    )
    .await
    {
        return None;
    }
    Some((device_id.as_str().to_string(), pairing_ids))
}

/// The change feeds a session's pump wakes on, subscribed before it attaches.
struct Ticks {
    outbox: watch::Receiver<u64>,
    registry: watch::Receiver<u64>,
}

/// The authenticated steady state. Returns the pairings the session still
/// served when it ended (for presence).
async fn pump(
    shared: &Shared,
    session: u64,
    pairings: Vec<PairingId>,
    ticks: Ticks,
    sink: &mut Sink,
    stream: &mut Stream,
    mut stop: watch::Receiver<bool>,
) -> Vec<PairingId> {
    let mut active: Vec<PairingId> = pairings;
    // Per pairing: the highest seq delivered; `None` until the phone resumes.
    let mut delivered: HashMap<String, Option<u64>> = active
        .iter()
        .map(|p| (p.as_str().to_string(), None))
        .collect();
    let mut revoked_here: HashSet<String> = HashSet::new();
    let Ticks {
        outbox: mut outbox_rx,
        registry: mut registry_rx,
    } = ticks;
    let liveness = sleep(LIVENESS_TIMEOUT);
    tokio::pin!(liveness);

    loop {
        let mut alive = false;
        tokio::select! {
            biased;
            _ = stopped(&mut stop) => {
                send(sink, &RelayFrame::Bye { reason: Some("desktop shutting down".into()) }).await;
                break;
            }
            _ = &mut liveness => break,
            changed = registry_rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let mut keep = Vec::new();
                for pid in active.drain(..) {
                    match shared.still_serves(session, pid.as_str()) {
                        Ok(()) => keep.push(pid),
                        Err(true) => {
                            // Revoked or unpaired on this Mac: tell the phone,
                            // as the relay tells a former peer.
                            send(sink, &RelayFrame::PairingRevoked { pairing_id: pid.clone() }).await;
                            revoked_here.insert(pid.as_str().to_string());
                        }
                        Err(false) => {} // a newer connection took it over
                    }
                }
                active = keep;
                delivered.retain(|k, _| active.iter().any(|p| p.as_str() == k));
                if active.is_empty() {
                    send(sink, &RelayFrame::Bye { reason: None }).await;
                    break;
                }
            }
            changed = outbox_rx.changed() => {
                if changed.is_err() || !deliver(shared, sink, &mut delivered).await {
                    break;
                }
            }
            incoming = read_next(stream) => match incoming {
                Incoming::Closed => break,
                Incoming::Skip => {}
                Incoming::Malformed => {
                    alive = true;
                    send_error(sink, RelayErrorCode::BadFrame, "malformed frame", None).await;
                }
                Incoming::Frame(frame) => {
                    alive = true;
                    match on_frame(shared, sink, &mut active, &mut delivered, &mut revoked_here, *frame).await {
                        Flow::Continue => {}
                        Flow::End => break,
                    }
                }
            },
        }
        if alive {
            liveness
                .as_mut()
                .reset(tokio::time::Instant::now() + LIVENESS_TIMEOUT);
        }
    }
    active
}

/// Send every outbox envelope past what each resumed pairing has been given.
async fn deliver(
    shared: &Shared,
    sink: &mut Sink,
    delivered: &mut HashMap<String, Option<u64>>,
) -> bool {
    for (pid, cursor) in delivered.iter_mut() {
        let Some(from) = *cursor else { continue };
        for env in shared.after(pid, from) {
            let seq = env.seq;
            if !send(sink, &RelayFrame::Envelope(env)).await {
                return false;
            }
            *cursor = Some(seq);
        }
    }
    true
}

enum Flow {
    Continue,
    End,
}

/// Handle one authenticated frame.
async fn on_frame(
    shared: &Shared,
    sink: &mut Sink,
    active: &mut Vec<PairingId>,
    delivered: &mut HashMap<String, Option<u64>>,
    revoked_here: &mut HashSet<String>,
    frame: RelayFrame,
) -> Flow {
    let is_active = |active: &Vec<PairingId>, pid: &PairingId| active.iter().any(|p| p == pid);
    match frame {
        RelayFrame::Envelope(env) => {
            if !is_active(active, &env.pairing_id) {
                send_error(
                    sink,
                    RelayErrorCode::UnknownPairing,
                    "envelope for an inactive pairing",
                    Some(env.pairing_id),
                )
                .await;
            } else if env.sender != Role::Phone {
                send_error(sink, RelayErrorCode::BadFrame, "sender must be phone", None).await;
            } else {
                let pid = env.pairing_id.clone();
                let cursor = match shared.accept_inbound(pid.as_str(), env.seq) {
                    Ok(seq) => {
                        shared.inbound.send(RemoteInbound::Envelope(env));
                        seq
                    }
                    // A duplicate (reconnect race, retry): never re-applied, but
                    // re-acked so the phone can trim its own queue (§6.4).
                    Err(cursor) => cursor,
                };
                send(
                    sink,
                    &RelayFrame::Ack {
                        pairing_id: pid,
                        cursor,
                    },
                )
                .await;
            }
        }
        RelayFrame::Resume {
            pairing_id,
            from_seq,
        } => {
            if !is_active(active, &pairing_id) {
                send_error(
                    sink,
                    RelayErrorCode::UnknownPairing,
                    "resume for an inactive pairing",
                    Some(pairing_id),
                )
                .await;
                return Flow::Continue;
            }
            match shared.resume(pairing_id.as_str(), from_seq) {
                ResumeOutcome::Resync => {
                    delivered.insert(pairing_id.as_str().to_string(), None);
                    send_error(
                        sink,
                        RelayErrorCode::SeqViolation,
                        "resume precedes the oldest retained envelope; resync required",
                        Some(pairing_id),
                    )
                    .await;
                }
                ResumeOutcome::Replay(envs) => {
                    let mut cursor = from_seq;
                    for env in envs {
                        cursor = env.seq;
                        if !send(sink, &RelayFrame::Envelope(env)).await {
                            return Flow::End;
                        }
                    }
                    delivered.insert(pairing_id.as_str().to_string(), Some(cursor));
                    // Anything pushed while replaying goes out now.
                    if !deliver(shared, sink, delivered).await {
                        return Flow::End;
                    }
                }
            }
        }
        RelayFrame::Ack { pairing_id, cursor } => {
            if is_active(active, &pairing_id) {
                shared.ack(pairing_id.as_str(), cursor);
                shared
                    .inbound
                    .send(RemoteInbound::PeerAck { pairing_id, cursor });
            } else {
                send_error(
                    sink,
                    RelayErrorCode::UnknownPairing,
                    "ack for an inactive pairing",
                    Some(pairing_id),
                )
                .await;
            }
        }
        RelayFrame::Ping { client_time_ms } => {
            send(
                sink,
                &RelayFrame::Pong {
                    client_time_ms,
                    server_time_ms: now_ms(),
                },
            )
            .await;
        }
        RelayFrame::Revoke { pairing_id } => {
            if is_active(active, &pairing_id) {
                // Exactly what a relay-delivered `pairing_revoked` does to this
                // Mac: the record goes, the outbox goes, the app is told.
                shared.forget(pairing_id.as_str());
                shared.inbound.send(RemoteInbound::PairingRevoked {
                    pairing_id: pairing_id.clone(),
                });
                active.retain(|p| p != &pairing_id);
                delivered.remove(pairing_id.as_str());
                revoked_here.insert(pairing_id.as_str().to_string());
                send(sink, &RelayFrame::PairingRevoked { pairing_id }).await;
                if active.is_empty() {
                    return Flow::End;
                }
            } else if revoked_here.contains(pairing_id.as_str()) {
                // Idempotent: already revoked on this connection.
                send(sink, &RelayFrame::PairingRevoked { pairing_id }).await;
            } else {
                send_error(
                    sink,
                    RelayErrorCode::UnknownPairing,
                    "not a member of this pairing",
                    Some(pairing_id),
                )
                .await;
            }
        }
        RelayFrame::Bye { .. } => return Flow::End,
        RelayFrame::Error { .. } => {}
        other if is_bootstrap_or_push(&other) => {
            send_error(
                sink,
                RelayErrorCode::BadFrame,
                "not served on the direct link",
                None,
            )
            .await;
        }
        _ => {
            send_error(sink, RelayErrorCode::BadFrame, "unexpected frame", None).await;
        }
    }
    Flow::Continue
}
