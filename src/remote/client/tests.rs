//! Client tests: pure backoff schedule plus a full client state-machine drill
//! against an in-process **mock relay** (a `std::net::TcpListener` +
//! `tungstenite::accept` on a worker thread — no async runtime). The mock proves
//! protocol compliance without the real relay: it verifies the auth signature
//! with the client's real public key, exercises resume/ack/envelope echo, and
//! the auth-failure and reconnect-after-drop paths.

use super::*;

use std::net::TcpListener;
use std::sync::mpsc::channel;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};

use flightdeck_remote_protocol::relay::{EncryptedEnvelope, RelayErrorCode, RelayFrame};
use flightdeck_remote_protocol::{PairingId, Role};

use crate::remote::state::Pairing;
use crate::remote::{RemoteInbound, RemoteOutbound};

// --- Backoff schedule ------------------------------------------------------

#[test]
fn backoff_starts_at_one_second_and_caps_at_sixty() {
    // First retry: exactly 1s with zero jitter, up to +25% with full jitter.
    assert_eq!(backoff_delay(0, 0.0), Duration::from_millis(1_000));
    assert_eq!(backoff_delay(0, 1.0), Duration::from_millis(1_250));
    // Doubling each attempt.
    assert_eq!(backoff_delay(1, 0.0), Duration::from_millis(2_000));
    assert_eq!(backoff_delay(2, 0.0), Duration::from_millis(4_000));
    assert_eq!(backoff_delay(3, 0.0), Duration::from_millis(8_000));
    // Capped at 60s no matter how large the attempt or the jitter.
    assert_eq!(backoff_delay(100, 0.0), Duration::from_millis(60_000));
    assert_eq!(backoff_delay(100, 1.0), Duration::from_millis(60_000));
}

// --- Backoff reset only after a stable authed session (0ef.2) --------------

/// A session that reached `auth_ok` then immediately dropped (relay crash/
/// redeploy loop) must NOT reset the reconnect backoff to zero, or the client
/// hammers the relay ~once/second forever. Only a session that stayed
/// authenticated for at least `min_stable` resets it (remote-control-0ef.2).
#[test]
fn backoff_resets_only_after_a_stable_authed_session() {
    let min = Duration::from_secs(10);
    // Never authenticated → never resets.
    assert!(!session_resets_backoff(None, min));
    // Authenticated but flapped under the stable threshold → no reset.
    assert!(!session_resets_backoff(Some(Duration::from_millis(0)), min));
    assert!(!session_resets_backoff(Some(Duration::from_secs(1)), min));
    assert!(!session_resets_backoff(
        Some(Duration::from_millis(9_999)),
        min
    ));
    // Authenticated and stayed healthy past the threshold → resets.
    assert!(session_resets_backoff(Some(Duration::from_secs(10)), min));
    assert!(session_resets_backoff(Some(Duration::from_secs(120)), min));
}

#[test]
fn backoff_is_monotonic_until_the_cap() {
    let mut prev = Duration::ZERO;
    for attempt in 0..7 {
        let d = backoff_delay(attempt, 0.0);
        assert!(d >= prev, "attempt {attempt} should not shrink");
        assert!(d <= Duration::from_millis(BACKOFF_CAP_MS));
        prev = d;
    }
}

// --- Cursor-persist debounce (0ef.11) --------------------------------------

/// A [`RemoteStore`] that only counts `save` calls, for asserting the gate
/// coalesces many cursor bumps into few disk writes.
struct CountingStore(std::sync::atomic::AtomicUsize);

impl RemoteStore for CountingStore {
    fn load(&self) -> RemoteState {
        RemoteState::default()
    }
    fn save(&self, _state: &RemoteState) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Many cursor bumps inside one interval must collapse to a single write once the
/// interval elapses — the whole point of the gate (remote-control-0ef.11).
#[test]
fn cursor_flush_gate_coalesces_dirty_bumps_into_one_write() {
    let store = CountingStore(std::sync::atomic::AtomicUsize::new(0));
    let state = RemoteState::default();
    let mut gate = CursorFlushGate::new(Duration::from_millis(50));

    // A burst of bumps within the interval writes nothing yet.
    for _ in 0..100 {
        gate.mark_dirty();
        gate.maybe_flush(&store, &state);
    }
    assert_eq!(
        store.0.load(Ordering::SeqCst),
        0,
        "bumps within the interval must not hit disk"
    );

    // Once the interval elapses, the next tick writes exactly once for the burst.
    std::thread::sleep(Duration::from_millis(60));
    gate.maybe_flush(&store, &state);
    assert_eq!(
        store.0.load(Ordering::SeqCst),
        1,
        "one coalesced write after the interval"
    );

    // A clean gate never writes, however long we wait.
    std::thread::sleep(Duration::from_millis(60));
    gate.maybe_flush(&store, &state);
    assert_eq!(
        store.0.load(Ordering::SeqCst),
        1,
        "a clean gate never writes"
    );
}

/// The session-end flush must persist a pending bump even when the debounce
/// interval has not elapsed, so a clean teardown never loses a cursor.
#[test]
fn cursor_flush_gate_flush_persists_pending_regardless_of_interval() {
    let store = CountingStore(std::sync::atomic::AtomicUsize::new(0));
    let state = RemoteState::default();
    let mut gate = CursorFlushGate::new(Duration::from_secs(3600));

    gate.mark_dirty();
    gate.maybe_flush(&store, &state); // interval not elapsed → no-op
    assert_eq!(store.0.load(Ordering::SeqCst), 0);

    gate.flush(&store, &state); // session end → force
    assert_eq!(
        store.0.load(Ordering::SeqCst),
        1,
        "session-end flush persists a dirty gate"
    );
    gate.flush(&store, &state); // now clean
    assert_eq!(store.0.load(Ordering::SeqCst), 1);
}

/// `mark_clean` (called after a lifecycle `store.save` wrote the whole state)
/// folds the pending cursor bump into that write, so the next flush is a no-op.
#[test]
fn cursor_flush_gate_mark_clean_folds_pending_bump() {
    let store = CountingStore(std::sync::atomic::AtomicUsize::new(0));
    let state = RemoteState::default();
    let mut gate = CursorFlushGate::new(Duration::ZERO);

    gate.mark_dirty();
    gate.mark_clean(); // stands in for an out-of-band lifecycle save
    gate.flush(&store, &state);
    assert_eq!(
        store.0.load(Ordering::SeqCst),
        0,
        "mark_clean folds the pending bump; no extra write"
    );
}

// --- Mock relay harness ----------------------------------------------------

/// In-memory [`RemoteStore`] so tests never touch `~/.flightdeck`.
struct MemStore(Mutex<RemoteState>);

impl RemoteStore for MemStore {
    fn load(&self) -> RemoteState {
        self.0.lock().unwrap().clone()
    }
    fn save(&self, state: &RemoteState) {
        *self.0.lock().unwrap() = state.clone();
    }
}

type Ws = WebSocket<TcpStream>;

/// How long a mock worker waits for the next client connection before giving up.
/// A test that self-heals (or otherwise stops the client) connects fewer times
/// than a reject/accept loop offers; the surplus `accept()` calls must time out
/// rather than park `mock.join()` forever. Comfortably longer than the client's
/// pre-self-heal backoff gaps (≤~3s) so it never fires mid-test.
const MOCK_ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);

/// Read timeout applied to every accepted mock socket, so a `ws.read()` against
/// a connected-but-silent client can never block forever either.
const MOCK_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Accept one connection within [`MOCK_ACCEPT_TIMEOUT`], returning `None` if none
/// arrives (the client has stopped reconnecting). The accepted stream is put back
/// into blocking mode with [`MOCK_READ_TIMEOUT`] so no downstream read can hang.
fn accept_within(listener: &TcpListener) -> Option<TcpStream> {
    listener
        .set_nonblocking(true)
        .expect("set_nonblocking on mock listener");
    let deadline = Instant::now() + MOCK_ACCEPT_TIMEOUT;
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    };
    // Accepted sockets don't inherit the listener's nonblocking flag on any
    // supported platform, but set it explicitly so the read timeout applies.
    stream
        .set_nonblocking(false)
        .expect("clear nonblocking on accepted mock stream");
    stream
        .set_read_timeout(Some(MOCK_READ_TIMEOUT))
        .expect("set read timeout on accepted mock stream");
    Some(stream)
}

/// Blocking read of the next relay frame from the mock's socket.
fn ws_recv(ws: &mut Ws) -> Option<RelayFrame> {
    loop {
        match ws.read() {
            Ok(Message::Text(s)) => return serde_json::from_str(&s).ok(),
            Ok(Message::Close(_)) => return None,
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

fn ws_send(ws: &mut Ws, frame: &RelayFrame) {
    let _ = ws.send(Message::Text(serde_json::to_string(frame).unwrap()));
}

/// Run the relay side of the handshake, verifying the auth signature against
/// `pubkey` exactly as the real relay would. Returns `true` on success.
fn mock_authenticate(ws: &mut Ws, pubkey: &[u8], pairing_ids: &[&str]) -> bool {
    if !matches!(ws_recv(ws), Some(RelayFrame::Hello { .. })) {
        return false;
    }
    ws_send(
        ws,
        &RelayFrame::HelloOk {
            protocol_version: 1,
            server_time_ms: 0,
            connection_id: "conn-1".to_string(),
        },
    );
    let nonce_raw = [7u8; 32];
    ws_send(
        ws,
        &RelayFrame::AuthChallenge {
            nonce: STANDARD.encode(nonce_raw),
            server_time_ms: 0,
        },
    );
    let ok = match ws_recv(ws) {
        Some(RelayFrame::AuthResponse { signature, .. }) => {
            let vk = VerifyingKey::from_sec1_bytes(pubkey).unwrap();
            let sig = Signature::from_slice(&STANDARD.decode(&signature).unwrap()).unwrap();
            vk.verify(&nonce_raw, &sig).is_ok()
        }
        _ => false,
    };
    if !ok {
        return false;
    }
    ws_send(
        ws,
        &RelayFrame::AuthOk {
            pairing_ids: pairing_ids.iter().map(|p| PairingId::new(*p)).collect(),
        },
    );
    true
}

// --- Happy path ------------------------------------------------------------

#[test]
fn happy_path_auth_resume_ack_and_echo() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let identity = DeviceIdentity::generate();
    let pubkey = identity.public_key_x963().to_vec();

    let mock = std::thread::spawn(move || {
        let stream = accept_within(&listener).expect("client should connect");
        let mut ws = tungstenite::accept(stream).unwrap();
        assert!(mock_authenticate(&mut ws, &pubkey, &["pair_test"]));

        // The client resumes the known pairing from seq 0.
        match ws_recv(&mut ws) {
            Some(RelayFrame::Resume {
                pairing_id,
                from_seq,
            }) => {
                assert_eq!(pairing_id.as_str(), "pair_test");
                assert_eq!(from_seq, 0);
            }
            other => panic!("expected resume, got {other:?}"),
        }

        // Push a phone→desktop envelope; the client must forward it and auto-ack.
        ws_send(
            &mut ws,
            &RelayFrame::Envelope(EncryptedEnvelope {
                pairing_id: PairingId::new("pair_test"),
                seq: 1,
                sender: Role::Phone,
                sent_at_ms: 0,
                nonce: "bg==".to_string(),
                ciphertext: "aGk=".to_string(),
            }),
        );
        match ws_recv(&mut ws) {
            Some(RelayFrame::Ack { cursor, .. }) => assert_eq!(cursor, 1),
            other => panic!("expected ack, got {other:?}"),
        }

        // The app then sends its own payload; it must arrive as seq 1 (desktop).
        match ws_recv(&mut ws) {
            Some(RelayFrame::Envelope(e)) => {
                assert_eq!(e.seq, 1);
                assert_eq!(e.sender, Role::Desktop);
                assert_eq!(e.ciphertext, "Y2lwaGVy");
            }
            other => panic!("expected desktop envelope, got {other:?}"),
        }
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
        ..RemoteConfig::default()
    };
    let mut seed = RemoteState::default();
    seed.pairings.push(Pairing::new("pair_test"));
    let store = Box::new(MemStore(Mutex::new(seed)));

    let (in_tx, in_rx) = channel();
    let (out_tx, out_rx) = channel();
    let handle = RemoteHandle::start_with_store(cfg, identity, store, in_tx, out_rx);

    let mut got_connected = false;
    let mut got_envelope = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !(got_connected && got_envelope) {
        if let Ok(msg) = in_rx.recv_timeout(Duration::from_millis(500)) {
            match msg {
                RemoteInbound::Link(RemoteLinkState::Connected { .. }) => got_connected = true,
                RemoteInbound::Envelope(e) => {
                    assert_eq!(e.seq, 1);
                    got_envelope = true;
                    // Reply so the mock can observe an outbound envelope.
                    out_tx
                        .send(RemoteOutbound::SendEnvelope {
                            pairing_id: PairingId::new("pair_test"),
                            seq: 1,
                            sent_at_ms: 1_000,
                            nonce: "bg==".to_string(),
                            ciphertext: "Y2lwaGVy".to_string(),
                        })
                        .unwrap();
                }
                _ => {}
            }
        }
    }

    assert!(
        got_connected,
        "client should report Connected after auth_ok"
    );
    assert!(got_envelope, "client should forward the inbound envelope");
    mock.join().unwrap();
    handle.stop();
}

// --- Auth failure ----------------------------------------------------------

#[test]
fn auth_failure_reports_disconnected_and_never_connected() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let identity = DeviceIdentity::generate();

    let mock = std::thread::spawn(move || {
        let stream = match accept_within(&listener) {
            Some(s) => s,
            None => return,
        };
        let mut ws = tungstenite::accept(stream).unwrap();
        if !matches!(ws_recv(&mut ws), Some(RelayFrame::Hello { .. })) {
            return;
        }
        ws_send(
            &mut ws,
            &RelayFrame::HelloOk {
                protocol_version: 1,
                server_time_ms: 0,
                connection_id: "conn".to_string(),
            },
        );
        ws_send(
            &mut ws,
            &RelayFrame::AuthChallenge {
                nonce: STANDARD.encode([1u8; 16]),
                server_time_ms: 0,
            },
        );
        let _ = ws_recv(&mut ws); // consume auth_response
        ws_send(
            &mut ws,
            &RelayFrame::Error {
                code: RelayErrorCode::AuthFailed,
                message: "bad key".to_string(),
                pairing_id: None,
                expected_seq: None,
            },
        );
        // Drop the listener/socket; the client will keep retrying.
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
        ..RemoteConfig::default()
    };
    let store = Box::new(MemStore(Mutex::new(RemoteState::default())));
    let (in_tx, in_rx) = channel();
    let (_out_tx, out_rx) = channel();
    let handle = RemoteHandle::start_with_store(cfg, identity, store, in_tx, out_rx);

    let mut connected = false;
    let mut disconnected = false;
    // The empty store makes this a fresh-desktop connect, which waits
    // PENDING_OFFER_WAIT (~1s) for a pairing request before it sends auth; add
    // headroom for that plus connect/handshake latency on slower (Windows) CI.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(msg) = in_rx.recv_timeout(Duration::from_millis(250)) {
            match msg {
                RemoteInbound::Link(RemoteLinkState::Connected { .. }) => connected = true,
                RemoteInbound::Link(RemoteLinkState::Disconnected) => disconnected = true,
                _ => {}
            }
        }
    }

    assert!(!connected, "auth failure must never report Connected");
    assert!(disconnected, "auth failure must report Disconnected");
    handle.stop();
    let _ = mock.join();
}

// --- Reconnect after drop --------------------------------------------------

#[test]
fn reconnects_after_socket_drop() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let identity = DeviceIdentity::generate();
    let pubkey = identity.public_key_x963().to_vec();

    let mock = std::thread::spawn(move || {
        // Accept twice: authenticate, then drop the socket to force a reconnect.
        for _ in 0..2 {
            let stream = match accept_within(&listener) {
                Some(s) => s,
                None => return,
            };
            let mut ws = tungstenite::accept(stream).unwrap();
            if !mock_authenticate(&mut ws, &pubkey, &[]) {
                return;
            }
            drop(ws);
        }
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
        ..RemoteConfig::default()
    };
    let store = Box::new(MemStore(Mutex::new(RemoteState::default())));
    let (in_tx, in_rx) = channel();
    let (_out_tx, out_rx) = channel();
    let handle = RemoteHandle::start_with_store(cfg, identity, store, in_tx, out_rx);

    let mut connected_count = 0;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && connected_count < 2 {
        if let Ok(RemoteInbound::Link(RemoteLinkState::Connected { .. })) =
            in_rx.recv_timeout(Duration::from_millis(500))
        {
            connected_count += 1;
        }
    }

    assert!(
        connected_count >= 2,
        "should reconnect and reach Connected twice, got {connected_count}"
    );
    handle.stop();
    let _ = mock.join();
}

// --- Self-heal on a pairing the relay no longer recognizes (1jy) ------------

/// A [`RemoteStore`] over a shared `Arc<Mutex<_>>` so the test can inspect what
/// the client persisted after it self-heals.
#[derive(Clone)]
struct SharedStore(std::sync::Arc<Mutex<RemoteState>>);

impl RemoteStore for SharedStore {
    fn load(&self) -> RemoteState {
        self.0.lock().unwrap().clone()
    }
    fn save(&self, state: &RemoteState) {
        *self.0.lock().unwrap() = state.clone();
    }
}

#[test]
fn repeated_auth_rejection_drops_stale_pairing_and_signals_repair() {
    // A returning desktop with a persisted pairing connects to a relay that no
    // longer knows it (e.g. its store was wiped). Each auth-first attempt is
    // rejected with `auth_failed`. After AUTH_REJECT_REOFFER_THRESHOLD
    // consecutive rejections the client must self-heal: drop the stale pairing
    // from persisted state and emit `PairingRejected` — instead of looping
    // forever (remote-control-1jy).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let identity = DeviceIdentity::generate();

    let mock = std::thread::spawn(move || {
        // Reject every connect. More accepts than the threshold so timing/
        // jitter can never starve the test of a rejection; the client self-heals
        // after THRESHOLD rejections and stops, so the surplus accepts simply
        // time out via `accept_within` rather than parking `mock.join()` forever.
        for _ in 0..(AUTH_REJECT_REOFFER_THRESHOLD + 3) {
            let stream = match accept_within(&listener) {
                Some(s) => s,
                None => return,
            };
            let mut ws = match tungstenite::accept(stream) {
                Ok(ws) => ws,
                Err(_) => return,
            };
            if !matches!(ws_recv(&mut ws), Some(RelayFrame::Hello { .. })) {
                return;
            }
            ws_send(
                &mut ws,
                &RelayFrame::HelloOk {
                    protocol_version: 1,
                    server_time_ms: 0,
                    connection_id: "conn".to_string(),
                },
            );
            ws_send(
                &mut ws,
                &RelayFrame::AuthChallenge {
                    nonce: STANDARD.encode([1u8; 16]),
                    server_time_ms: 0,
                },
            );
            let _ = ws_recv(&mut ws); // consume auth_response
            ws_send(
                &mut ws,
                &RelayFrame::Error {
                    code: RelayErrorCode::AuthFailed,
                    message: "unknown device".to_string(),
                    pairing_id: None,
                    expected_seq: None,
                },
            );
        }
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
        ..RemoteConfig::default()
    };
    // Seed a persisted pairing so the client takes the auth-first path.
    let mut seed = RemoteState::default();
    seed.pairings.push(Pairing::new("pair_stale"));
    let shared = std::sync::Arc::new(Mutex::new(seed));
    let store = Box::new(SharedStore(shared.clone()));

    let (in_tx, in_rx) = channel();
    let (_out_tx, out_rx) = channel();
    let handle = RemoteHandle::start_with_store(cfg, identity, store, in_tx, out_rx);

    // Wait for the self-heal signal. Threshold rejections each incur backoff, so
    // allow generous wall-clock time.
    let mut rejected: Option<Vec<PairingId>> = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && rejected.is_none() {
        if let Ok(RemoteInbound::PairingRejected { pairing_ids }) =
            in_rx.recv_timeout(Duration::from_millis(500))
        {
            rejected = Some(pairing_ids);
        }
    }

    let dropped = rejected.expect("client should emit PairingRejected after repeated rejections");
    assert_eq!(
        dropped,
        vec![PairingId::new("pair_stale")],
        "the dropped pairing id should be reported"
    );
    assert!(
        shared.lock().unwrap().pairings.is_empty(),
        "the stale pairing must be cleared from persisted state so the next connect re-offers"
    );

    handle.stop();
    let _ = mock.join();
}

// --- Recover from a relay seq-violation (relay restart) without a fatal loop (bbf) --

/// Read frames until an [`EncryptedEnvelope`] arrives (skipping pings/acks etc.),
/// bounded so a misbehaving client can never hang the mock forever.
fn ws_recv_envelope(ws: &mut Ws) -> EncryptedEnvelope {
    for _ in 0..20 {
        match ws_recv(ws) {
            Some(RelayFrame::Envelope(e)) => return e,
            Some(_) => continue,
            None => break,
        }
    }
    panic!("expected an envelope frame from the client");
}

/// Block until the client reports [`RemoteLinkState::Connected`].
fn wait_for_connected(in_rx: &std::sync::mpsc::Receiver<RemoteInbound>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(RemoteInbound::Link(RemoteLinkState::Connected { .. })) =
            in_rx.recv_timeout(Duration::from_millis(250))
        {
            return;
        }
    }
    panic!("client never reported Connected");
}

/// A `seq_violation` advisory means our INBOUND cursor is stale (the peer
/// restarted its stream, or the relay shed envelopes we needed). The client must
/// NOT tear the connection down and reconnect into the same advisory forever
/// (remote-control-bbf); it must zero `last_received_seq`, re-`resume` from 0,
/// and emit `SeqResync` so the bridge re-snapshots.
///
/// Critically it must NOT renumber the outbound stream. That rewind was the old
/// recovery, and against a relay that persists its watermark it never
/// terminates: restart at seq 1 → rejected → rewind → restart at seq 1
/// (remote-control-arg). The mock only ever accepts ONE connection, so a
/// fatal-reconnect regression could never deliver the follow-up envelope and
/// this test would fail.
#[test]
fn seq_violation_resyncs_inbound_without_renumbering_the_outbound_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let identity = DeviceIdentity::generate();
    let pubkey = identity.public_key_x963().to_vec();

    let mock = std::thread::spawn(move || {
        let stream = accept_within(&listener).expect("client should connect");
        let mut ws = tungstenite::accept(stream).unwrap();
        assert!(mock_authenticate(&mut ws, &pubkey, &["pair_test"]));

        // Returning desktop resumes the known pairing.
        match ws_recv(&mut ws) {
            Some(RelayFrame::Resume { pairing_id, .. }) => {
                assert_eq!(pairing_id.as_str(), "pair_test");
            }
            other => panic!("expected resume, got {other:?}"),
        }

        // The desktop sends its next outbound envelope at seq 6 (it persisted
        // last_sent_seq = 5). The relay answers the resync advisory — about the
        // desktop's INBOUND direction, not this envelope.
        let first = ws_recv_envelope(&mut ws);
        assert_eq!(first.seq, 6, "desktop resumes from its persisted cursor");
        ws_send(
            &mut ws,
            &RelayFrame::Error {
                code: RelayErrorCode::SeqViolation,
                message: "peer restarted its stream; drop your inbound cursor and resync"
                    .to_string(),
                pairing_id: Some(PairingId::new("pair_test")),
                // Receiver-side advisory: no `expected_seq`, so this keeps
                // exercising the inbound-cursor path specifically.
                expected_seq: None,
            },
        );

        // The desktop re-resumes from 0 to pull whatever the relay still holds.
        match ws_recv(&mut ws) {
            Some(RelayFrame::Resume {
                pairing_id,
                from_seq,
            }) => {
                assert_eq!(pairing_id.as_str(), "pair_test");
                assert_eq!(from_seq, 0, "a resync re-resumes from scratch");
            }
            other => panic!("expected a re-resume, got {other:?}"),
        }

        // Delivery continues on the SAME connection, at the NEXT seq — the
        // stream is never renumbered.
        let resynced = ws_recv_envelope(&mut ws);
        assert_eq!(
            resynced.seq, 7,
            "the outbound stream continues gaplessly; it must NOT restart at 1"
        );
        ws_send(
            &mut ws,
            &RelayFrame::Ack {
                pairing_id: PairingId::new("pair_test"),
                cursor: 7,
            },
        );
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
        ..RemoteConfig::default()
    };
    let mut seed = RemoteState::default();
    let mut pairing = Pairing::new("pair_test");
    pairing.last_sent_seq = 5; // desktop already sent 5 envelopes before the restart
    seed.pairings.push(pairing);
    let shared = std::sync::Arc::new(Mutex::new(seed));
    let store = Box::new(SharedStore(shared.clone()));

    let (in_tx, in_rx) = channel();
    let (out_tx, out_rx) = channel();
    // This test inspects the persisted outbound cursor mid-session, so disable the
    // cursor-persist debounce (0ef.11): a zero interval flushes a dirty gate on the
    // next pump tick, restoring the pre-debounce near-immediate persistence.
    let tuning = ClientTuning {
        cursor_flush_interval: Duration::ZERO,
        ..ClientTuning::default()
    };
    let handle = RemoteHandle::start_tuned(cfg, identity, store, in_tx, out_rx, tuning);

    // Once connected, act as the bridge and send the next outbound envelope.
    wait_for_connected(&in_rx);
    out_tx
        .send(RemoteOutbound::SendEnvelope {
            pairing_id: PairingId::new("pair_test"),
            seq: 6,
            sent_at_ms: 1_000,
            nonce: "bg==".to_string(),
            ciphertext: "Y2lwaGVy".to_string(),
        })
        .unwrap();

    // The client must surface a resync (not a disconnect/reconnect loop).
    let mut resynced = false;
    let mut disconnected = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !resynced {
        match in_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(RemoteInbound::SeqResync { pairing_id }) => {
                assert_eq!(pairing_id.as_str(), "pair_test");
                resynced = true;
            }
            Ok(RemoteInbound::Link(RemoteLinkState::Disconnected)) => disconnected = true,
            _ => {}
        }
    }
    assert!(
        resynced,
        "client must emit SeqResync on a relay seq_violation"
    );
    assert!(!disconnected, "recovery must not tear the connection down");

    // Play the bridge's resync response: re-snapshot on the SAME stream, which
    // keeps counting up rather than restarting.
    out_tx
        .send(RemoteOutbound::SendEnvelope {
            pairing_id: PairingId::new("pair_test"),
            seq: 7,
            sent_at_ms: 2_000,
            nonce: "bg==".to_string(),
            ciphertext: "Y2lwaGVy".to_string(),
        })
        .unwrap();

    mock.join().unwrap();

    // The persisted OUTBOUND cursor advanced normally — the resync never touched
    // it. Rewinding it here is exactly what livelocked the phone in
    // remote-control-arg.
    let mut settled = 0;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        settled = shared
            .lock()
            .unwrap()
            .pairing("pair_test")
            .map(|p| p.last_sent_seq)
            .unwrap_or(0);
        if settled == 7 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        settled, 7,
        "a resync must leave the outbound cursor advancing, never rewind it"
    );

    // ...while the INBOUND cursor was dropped, so the peer's restarted stream is
    // not deduped away on the next launch.
    assert_eq!(
        shared
            .lock()
            .unwrap()
            .pairing("pair_test")
            .map(|p| p.last_received_seq),
        Some(0),
        "a resync drops the stale inbound cursor"
    );

    handle.stop();
}

/// The mirror-image fault: the relay rejects our OUTBOUND envelopes because our
/// `seq` ran ahead of its watermark, and names the seq it will accept next by
/// putting `expected_seq` on the advisory.
///
/// The client must surface that as `SeqRealign` — NOT as `SeqResync`. Treating
/// it as a resync is what left the stream wedged: it zeroes a perfectly good
/// inbound cursor and re-resumes, neither of which does anything about the
/// outbound counter that is actually wrong, so the relay keeps rejecting and the
/// desktop keeps counting (remote-control-zv3).
#[test]
fn seq_violation_with_expected_seq_realigns_the_outbound_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let identity = DeviceIdentity::generate();
    let pubkey = identity.public_key_x963().to_vec();

    let mock = std::thread::spawn(move || {
        let stream = accept_within(&listener).expect("client should connect");
        let mut ws = tungstenite::accept(stream).unwrap();
        assert!(mock_authenticate(&mut ws, &pubkey, &["pair_test"]));

        match ws_recv(&mut ws) {
            Some(RelayFrame::Resume { pairing_id, .. }) => {
                assert_eq!(pairing_id.as_str(), "pair_test");
            }
            other => panic!("expected resume, got {other:?}"),
        }

        // We are far ahead of the relay: it holds high_water = 97.
        let first = ws_recv_envelope(&mut ws);
        assert_eq!(first.seq, 25_000);
        ws_send(
            &mut ws,
            &RelayFrame::Error {
                code: RelayErrorCode::SeqViolation,
                message: "envelope seq is not gapless/monotonic; resume at 98".to_string(),
                pairing_id: Some(PairingId::new("pair_test")),
                expected_seq: Some(98),
            },
        );

        // Acting as the bridge, the test re-sends at the named seq. It must
        // arrive on the SAME connection, at exactly 98.
        let realigned = ws_recv_envelope(&mut ws);
        assert_eq!(
            realigned.seq, 98,
            "the realigned stream must resume at the seq the relay named"
        );
        ws_send(
            &mut ws,
            &RelayFrame::Ack {
                pairing_id: PairingId::new("pair_test"),
                cursor: 98,
            },
        );
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
        ..RemoteConfig::default()
    };
    let mut seed = RemoteState::default();
    let mut pairing = Pairing::new("pair_test");
    pairing.last_sent_seq = 24_999;
    pairing.last_received_seq = 42; // healthy inbound cursor: must survive
    seed.pairings.push(pairing);
    let shared = std::sync::Arc::new(Mutex::new(seed));
    let store = Box::new(SharedStore(shared.clone()));

    let (in_tx, in_rx) = channel();
    let (out_tx, out_rx) = channel();
    let tuning = ClientTuning {
        cursor_flush_interval: Duration::ZERO,
        ..ClientTuning::default()
    };
    let handle = RemoteHandle::start_tuned(cfg, identity, store, in_tx, out_rx, tuning);

    wait_for_connected(&in_rx);
    out_tx
        .send(RemoteOutbound::SendEnvelope {
            pairing_id: PairingId::new("pair_test"),
            seq: 25_000,
            sent_at_ms: 1_000,
            nonce: "bg==".to_string(),
            ciphertext: "Y2lwaGVy".to_string(),
        })
        .unwrap();

    let mut realigned_to = None;
    let mut resynced = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && realigned_to.is_none() {
        match in_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(RemoteInbound::SeqRealign {
                pairing_id,
                next_seq,
            }) => {
                assert_eq!(pairing_id.as_str(), "pair_test");
                realigned_to = Some(next_seq);
            }
            Ok(RemoteInbound::SeqResync { .. }) => resynced = true,
            _ => {}
        }
    }
    assert_eq!(
        realigned_to,
        Some(98),
        "an advisory carrying expected_seq must surface as SeqRealign"
    );
    assert!(
        !resynced,
        "a sender-side realign must not be reported as an inbound resync"
    );

    // Play the bridge's response: resume the stream at the named seq.
    out_tx
        .send(RemoteOutbound::SendEnvelope {
            pairing_id: PairingId::new("pair_test"),
            seq: 98,
            sent_at_ms: 2_000,
            nonce: "bg==".to_string(),
            ciphertext: "Y2lwaGVy".to_string(),
        })
        .unwrap();

    mock.join().unwrap();

    // The healthy INBOUND cursor is untouched — this fault was never about it.
    assert_eq!(
        shared
            .lock()
            .unwrap()
            .pairing("pair_test")
            .map(|p| p.last_received_seq),
        Some(42),
        "a realign must not disturb a perfectly good inbound cursor"
    );

    // The realigned OUTBOUND cursor is persisted at the lowered value, then
    // advances normally. If the realign were left to `SendEnvelope`'s monotonic
    // bump this would still read 24_999 — and the next launch would floor
    // `out_seq` straight back into the rejected range.
    let mut settled = 0;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        settled = shared
            .lock()
            .unwrap()
            .pairing("pair_test")
            .map(|p| p.last_sent_seq)
            .unwrap_or(0);
        if settled == 98 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        settled, 98,
        "the realigned cursor must be persisted, not left at the runaway value"
    );

    handle.stop();
}

/// The peer lost its outbound cursor and restarted its stream at seq 1 while we
/// hold a much higher inbound cursor. Plain `seq > last` dedup throws every
/// envelope of the restarted stream away — silently, forever — so the phone's
/// commands would never reach the desktop again (remote-control-arg). A seq of 1
/// under a non-zero cursor can only be a genuine restart (a healthy stream is
/// monotonic and never re-emits 1), so it must be accepted. Mirrors the rule the
/// phone already applies to the desktop's stream.
#[test]
fn a_peers_restarted_stream_is_accepted_not_deduped_away() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let identity = DeviceIdentity::generate();
    let pubkey = identity.public_key_x963().to_vec();

    let mock = std::thread::spawn(move || {
        let stream = accept_within(&listener).expect("client should connect");
        let mut ws = tungstenite::accept(stream).unwrap();
        assert!(mock_authenticate(&mut ws, &pubkey, &["pair_test"]));
        match ws_recv(&mut ws) {
            Some(RelayFrame::Resume { .. }) => {}
            other => panic!("expected resume, got {other:?}"),
        }

        // The peer restarted: its stream now begins again at seq 1, well below
        // our persisted inbound cursor of 40.
        ws_send(
            &mut ws,
            &RelayFrame::Envelope(EncryptedEnvelope {
                pairing_id: PairingId::new("pair_test"),
                seq: 1,
                sender: Role::Phone,
                sent_at_ms: 3_000,
                nonce: "bg==".to_string(),
                ciphertext: "Y2lwaGVy".to_string(),
            }),
        );

        // Accepting it means auto-acking it. A dedup regression sends nothing
        // and this read times out.
        match ws_recv(&mut ws) {
            Some(RelayFrame::Ack { cursor, .. }) => {
                assert_eq!(cursor, 1, "the restarted envelope must be acked");
            }
            other => panic!("expected an ack for the restarted stream, got {other:?}"),
        }
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
        ..RemoteConfig::default()
    };
    let mut seed = RemoteState::default();
    let mut pairing = Pairing::new("pair_test");
    pairing.last_received_seq = 40; // we are far ahead of the peer's restart
    seed.pairings.push(pairing);
    let shared = std::sync::Arc::new(Mutex::new(seed));
    let store = Box::new(SharedStore(shared.clone()));

    let (in_tx, in_rx) = channel();
    let (_out_tx, out_rx) = channel();
    let handle = RemoteHandle::start_with_store(cfg, identity, store, in_tx, out_rx);

    wait_for_connected(&in_rx);

    // The restarted envelope must reach the app, not be swallowed by dedup.
    let mut delivered = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !delivered {
        if let Ok(RemoteInbound::Envelope(env)) = in_rx.recv_timeout(Duration::from_millis(250)) {
            assert_eq!(env.seq, 1);
            delivered = true;
        }
    }
    assert!(
        delivered,
        "a peer's restarted stream must be delivered, not deduped away"
    );

    mock.join().unwrap();
    handle.stop();
}

// --- Machine name (spec §10.1) ---------------------------------------------

/// The desktop announces its machine name on `auth_response`, sent on every
/// connect. The mock captures the field and asserts it is present and bounded.
#[test]
fn desktop_announces_machine_name_on_auth() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let identity = DeviceIdentity::generate();
    let pubkey = identity.public_key_x963().to_vec();

    let mock = std::thread::spawn(move || -> Option<String> {
        let stream = accept_within(&listener)?;
        let mut ws = tungstenite::accept(stream).unwrap();
        if !matches!(ws_recv(&mut ws), Some(RelayFrame::Hello { .. })) {
            return None;
        }
        ws_send(
            &mut ws,
            &RelayFrame::HelloOk {
                protocol_version: 1,
                server_time_ms: 0,
                connection_id: "conn".to_string(),
            },
        );
        let nonce_raw = [7u8; 32];
        ws_send(
            &mut ws,
            &RelayFrame::AuthChallenge {
                nonce: STANDARD.encode(nonce_raw),
                server_time_ms: 0,
            },
        );
        let captured = match ws_recv(&mut ws) {
            Some(RelayFrame::AuthResponse {
                signature,
                machine_name,
                ..
            }) => {
                let vk = VerifyingKey::from_sec1_bytes(&pubkey).unwrap();
                let sig = Signature::from_slice(&STANDARD.decode(&signature).unwrap()).unwrap();
                assert!(vk.verify(&nonce_raw, &sig).is_ok(), "signature must verify");
                machine_name
            }
            other => panic!("expected auth_response, got {other:?}"),
        };
        ws_send(
            &mut ws,
            &RelayFrame::AuthOk {
                pairing_ids: vec![],
            },
        );
        captured
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
        ..RemoteConfig::default()
    };
    let store = Box::new(MemStore(Mutex::new(RemoteState::default())));
    let (in_tx, _in_rx) = channel();
    let (_out_tx, out_rx) = channel();
    let handle = RemoteHandle::start_with_store(cfg, identity, store, in_tx, out_rx);

    let captured = mock.join().unwrap();
    handle.stop();

    let name = captured.expect("desktop must announce a machine name (system hostname)");
    assert!(!name.is_empty(), "machine name must not be empty");
    assert!(
        name.chars().count() <= 64,
        "machine name must be length-bounded to 64 chars, got {}",
        name.chars().count()
    );
}

// --- Phone-initiated revoke (spec §10.2) -----------------------------------

/// On receiving `pairing_revoked`, the desktop drops the pairing from persisted
/// state and surfaces `RemoteInbound::PairingRevoked` so the app can tear down.
#[test]
fn pairing_revoked_drops_pairing_and_notifies_app() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let identity = DeviceIdentity::generate();
    let pubkey = identity.public_key_x963().to_vec();

    let mock = std::thread::spawn(move || {
        let stream = accept_within(&listener).expect("client should connect");
        let mut ws = tungstenite::accept(stream).unwrap();
        assert!(mock_authenticate(&mut ws, &pubkey, &["pair_test"]));
        // The client resumes; then the relay tells it the phone unpaired.
        let _ = ws_recv(&mut ws); // resume
        ws_send(
            &mut ws,
            &RelayFrame::PairingRevoked {
                pairing_id: PairingId::new("pair_test"),
            },
        );
        // Keep the socket open briefly so the client processes the frame.
        std::thread::sleep(Duration::from_millis(300));
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
        ..RemoteConfig::default()
    };
    let mut seed = RemoteState::default();
    seed.pairings.push(Pairing::new("pair_test"));
    let shared = std::sync::Arc::new(Mutex::new(seed));
    let store = Box::new(SharedStore(shared.clone()));

    let (in_tx, in_rx) = channel();
    let (_out_tx, out_rx) = channel();
    let handle = RemoteHandle::start_with_store(cfg, identity, store, in_tx, out_rx);

    let mut revoked = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !revoked {
        if let Ok(RemoteInbound::PairingRevoked { pairing_id }) =
            in_rx.recv_timeout(Duration::from_millis(200))
        {
            assert_eq!(pairing_id.as_str(), "pair_test");
            revoked = true;
        }
    }

    assert!(revoked, "client must surface PairingRevoked to the app");
    // The pairing was dropped from persisted state.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && !shared.lock().unwrap().pairings.is_empty() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        shared.lock().unwrap().pairings.is_empty(),
        "revoked pairing must be removed from persisted state"
    );

    let _ = mock.join();
    handle.stop();
}

// --- Half-open / liveness teardown (0ef.1) ---------------------------------

/// Build a [`ClientTuning`] with a short liveness deadline (and no forced write
/// failures) so a half-open socket is detected in milliseconds, not the real 60s.
fn fast_liveness_tuning(liveness: Duration) -> ClientTuning {
    ClientTuning {
        liveness_timeout: liveness,
        ..ClientTuning::default()
    }
}

/// A socket that dies without a FIN (laptop sleep, NAT reap, relay redeploy) stays
/// "open" to the client: idle reads loop forever and the tiny pings sit in the
/// kernel send buffer so the write timeout never trips. The liveness deadline must
/// notice the absence of ANY inbound frame and tear the session down so the
/// supervisor reconnects (remote-control-0ef.1). The mock authenticates, then holds
/// conn #1 OPEN and SILENT — the reconnect is driven purely by the liveness
/// deadline, never by a socket close.
#[test]
fn half_open_socket_is_torn_down_by_liveness_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let identity = DeviceIdentity::generate();
    let pubkey = identity.public_key_x963().to_vec();

    let mock = std::thread::spawn(move || {
        // conn #1: authenticate, then go silent while holding the socket open.
        let s1 = accept_within(&listener).expect("first connect");
        let mut ws1 = tungstenite::accept(s1).unwrap();
        assert!(mock_authenticate(&mut ws1, &pubkey, &["p"]));
        // Keep conn #1 alive and silent so the client's reconnect is caused by
        // the liveness deadline, not by conn #1 being closed.
        let _held = ws1;

        // conn #2: the client reconnected because conn #1 delivered no inbound
        // frames for the liveness window — the half-open link was detected.
        let s2 = accept_within(&listener).expect("reconnect after liveness teardown");
        let mut ws2 = tungstenite::accept(s2).unwrap();
        assert!(mock_authenticate(&mut ws2, &pubkey, &["p"]));
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
        relay_password: String::new(),
    };
    // A persisted pairing makes conn #1 auth-first (no pre-auth offer wait).
    let mut seed = RemoteState::default();
    seed.pairings.push(Pairing::new("p"));
    let store = Box::new(MemStore(Mutex::new(seed)));

    let (in_tx, in_rx) = channel();
    let (_out_tx, out_rx) = channel();
    let handle = RemoteHandle::start_tuned(
        cfg,
        identity,
        store,
        in_tx,
        out_rx,
        fast_liveness_tuning(Duration::from_millis(300)),
    );

    // Must reach Connected TWICE: once on conn #1, then again after the half-open
    // socket is torn down by the liveness deadline and the client reconnects.
    let mut connected = 0;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && connected < 2 {
        if let Ok(RemoteInbound::Link(RemoteLinkState::Connected { .. })) =
            in_rx.recv_timeout(Duration::from_millis(200))
        {
            connected += 1;
        }
    }
    assert!(
        connected >= 2,
        "liveness deadline must tear down a half-open socket and reconnect (got {connected} Connected)"
    );

    handle.stop();
    let _ = mock.join();
}

// --- No wire seq gap across a failed write + reconnect (0ef.9) --------------

/// When an outbound envelope's write fails mid-session the client must HOLD that
/// exact envelope and re-send it on the next session, so its `seq` is never
/// skipped on the wire — the bridge already advanced its `out_seq` past it, so a
/// dropped envelope would leave a gap the phone's contiguous-seq dedup stalls on
/// (remote-control-0ef.9). The forced-write-failure test seam makes the failure
/// deterministic (no dependence on OS-specific TCP RST timing): the first envelope
/// write fails, and the mock's SECOND connection must receive that same envelope
/// at seq 1 — not seq 2 (which is what a dropped-and-skipped envelope would leave).
#[test]
fn failed_envelope_write_is_held_and_resent_without_a_seq_gap() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let identity = DeviceIdentity::generate();
    let pubkey = identity.public_key_x963().to_vec();

    let mock = std::thread::spawn(move || {
        // conn #1: authenticate, read the resume, then block until the client
        // tears the session down (its envelope write was forced to fail → the
        // session ends and drops the socket, unblocking this read with None).
        let s1 = accept_within(&listener).expect("first connect");
        let mut ws1 = tungstenite::accept(s1).unwrap();
        assert!(mock_authenticate(&mut ws1, &pubkey, &["pair_test"]));
        match ws_recv(&mut ws1) {
            Some(RelayFrame::Resume { pairing_id, .. }) => {
                assert_eq!(pairing_id.as_str(), "pair_test");
            }
            other => panic!("expected resume on conn #1, got {other:?}"),
        }
        // The forced-failed envelope is never written; the session ends. Wait for
        // the client to drop conn #1 (returns None), then move on to conn #2.
        let _ = ws_recv(&mut ws1);

        // conn #2: the client re-sends the HELD envelope first. It must arrive at
        // seq 1 (re-sent), proving the seq was not skipped.
        let s2 = accept_within(&listener).expect("reconnect");
        let mut ws2 = tungstenite::accept(s2).unwrap();
        assert!(mock_authenticate(&mut ws2, &pubkey, &["pair_test"]));
        match ws_recv(&mut ws2) {
            Some(RelayFrame::Resume { .. }) => {}
            other => panic!("expected resume on conn #2, got {other:?}"),
        }
        let resent = ws_recv_envelope(&mut ws2);
        assert_eq!(
            resent.seq, 1,
            "the held envelope must be re-sent at its original seq (no gap)"
        );
        ws_send(
            &mut ws2,
            &RelayFrame::Ack {
                pairing_id: PairingId::new("pair_test"),
                cursor: 1,
            },
        );
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
        relay_password: String::new(),
    };
    let mut seed = RemoteState::default();
    seed.pairings.push(Pairing::new("pair_test"));
    let store = Box::new(MemStore(Mutex::new(seed)));

    let (in_tx, in_rx) = channel();
    let (out_tx, out_rx) = channel();
    // Force the first envelope write to fail so the hold/re-send path is exercised.
    let tuning = ClientTuning {
        fail_next_envelope_writes: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(1)),
        ..ClientTuning::default()
    };
    let handle = RemoteHandle::start_tuned(cfg, identity, store, in_tx, out_rx, tuning);

    // As the bridge: once connected on conn #1, enqueue exactly one envelope (seq
    // 1). Its write is forced to fail; the client holds it and re-sends on conn #2.
    wait_for_connected(&in_rx);
    out_tx
        .send(RemoteOutbound::SendEnvelope {
            pairing_id: PairingId::new("pair_test"),
            seq: 1,
            sent_at_ms: 1_000,
            nonce: "bg==".to_string(),
            ciphertext: "Y2lwaGVy".to_string(),
        })
        .unwrap();

    mock.join().unwrap();
    handle.stop();
}

// --- Version-incompatible is a distinct terminal state (0ef.20) -------------

/// A relay past this build's supported protocol range is permanent until the app
/// updates — retrying can never succeed. The client must surface a DISTINCT
/// terminal [`RemoteLinkState::Incompatible`] and STOP reconnecting, instead of
/// backoff-looping forever in silence (remote-control-0ef.20).
#[test]
fn version_incompatible_is_terminal_and_stops_reconnecting() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let identity = DeviceIdentity::generate();

    let mock = std::thread::spawn(move || {
        // Answer the first hello with version_incompatible, then keep accepting to
        // prove the client does NOT reconnect (surplus accepts simply time out).
        let s1 = accept_within(&listener).expect("first connect");
        let mut ws1 = tungstenite::accept(s1).unwrap();
        if !matches!(ws_recv(&mut ws1), Some(RelayFrame::Hello { .. })) {
            return false;
        }
        ws_send(
            &mut ws1,
            &RelayFrame::VersionIncompatible {
                your_version: 3,
                min_supported: 4,
                max_supported: 4,
            },
        );
        // If the client (wrongly) reconnects, a second connection arrives.
        accept_within(&listener).is_some()
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
        relay_password: String::new(),
    };
    let store = Box::new(MemStore(Mutex::new(RemoteState::default())));
    let (in_tx, in_rx) = channel();
    let (_out_tx, out_rx) = channel();
    let handle = RemoteHandle::start_with_store(cfg, identity, store, in_tx, out_rx);

    // The client must report the distinct terminal Incompatible state.
    let mut incompatible = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && incompatible.is_none() {
        if let Ok(RemoteInbound::Link(RemoteLinkState::Incompatible {
            our_version,
            relay_min,
            relay_max,
        })) = in_rx.recv_timeout(Duration::from_millis(200))
        {
            incompatible = Some((our_version, relay_min, relay_max));
        }
    }
    let versions = incompatible.expect("client must surface a terminal Incompatible link state");
    assert_eq!(
        versions,
        (3, 4, 4),
        "the incompatible-version detail must be surfaced for the UI"
    );

    // It must NOT reconnect: the mock's second accept should time out (false).
    let reconnected = mock.join().unwrap();
    assert!(
        !reconnected,
        "version-incompatible is terminal — the client must stop reconnecting"
    );

    handle.stop();
}

// --- Relay-password precedence (remote-control-uq7) ------------------------

#[test]
fn relay_password_env_overrides_config() {
    // A non-empty env value wins over the config.toml value.
    assert_eq!(
        resolve_relay_password(Some("from-env".to_string()), "from-config"),
        Some("from-env".to_string())
    );
}

#[test]
fn relay_password_falls_back_to_config_when_env_unset_or_empty() {
    // Env unset → config value is used.
    assert_eq!(
        resolve_relay_password(None, "from-config"),
        Some("from-config".to_string())
    );
    // Env present but empty/whitespace → treated as unset, config used.
    assert_eq!(
        resolve_relay_password(Some(String::new()), "from-config"),
        Some("from-config".to_string())
    );
    assert_eq!(
        resolve_relay_password(Some("  ".to_string()), "from-config"),
        Some("from-config".to_string())
    );
}

#[test]
fn relay_password_none_when_neither_configured() {
    // Both layers empty → no password presented (local/dev relay stays open).
    assert_eq!(resolve_relay_password(None, ""), None);
    assert_eq!(resolve_relay_password(Some("   ".to_string()), "   "), None);
}

#[test]
fn relay_password_is_sent_verbatim_not_trimmed() {
    // A real value is preserved byte-for-byte so it matches the relay's
    // constant-time compare exactly.
    assert_eq!(
        resolve_relay_password(Some(" pad ".to_string()), ""),
        Some(" pad ".to_string())
    );
}

// --- retiring superseded pairings (remote-control-4wk) ---------------------

fn pairing_for(id: &str, device: Option<&str>) -> crate::remote::Pairing {
    let mut p = crate::remote::Pairing::new(id);
    p.peer_device_id = device.map(|d| d.to_string());
    p.established = true;
    p
}

#[test]
fn a_new_pairing_retires_the_older_one_to_the_same_phone() {
    // The exact state found on Ruud's Mac: two established pairings, same
    // peer_device_id, the older one served nothing by the single-pairing bridge.
    let mut state = RemoteState::default();
    state
        .pairings
        .push(pairing_for("pair_0004", Some("phone_a")));
    state
        .pairings
        .push(pairing_for("pair_0005", Some("phone_a")));

    let retired = retire_superseded_pairings(&mut state, "pair_0005", Some("phone_a"));

    assert_eq!(retired, vec!["pair_0004".to_string()]);
    assert_eq!(
        state.pairing_ids(),
        vec!["pair_0005".to_string()],
        "only the newly-claimed pairing survives"
    );
}

#[test]
fn a_second_different_phone_is_never_retired() {
    // Multi-phone pairing is a real feature; only DUPLICATES of one phone go.
    let mut state = RemoteState::default();
    state
        .pairings
        .push(pairing_for("pair_phone_a", Some("phone_a")));
    state
        .pairings
        .push(pairing_for("pair_phone_b", Some("phone_b")));

    let retired = retire_superseded_pairings(&mut state, "pair_phone_b", Some("phone_b"));

    assert!(retired.is_empty(), "a different phone is not a duplicate");
    assert_eq!(state.pairings.len(), 2);
}

#[test]
fn an_unknown_peer_device_id_retires_nothing() {
    // Without knowing which phone claimed this pairing we cannot distinguish a
    // duplicate from a legitimate second phone, so we must not guess.
    let mut state = RemoteState::default();
    state
        .pairings
        .push(pairing_for("pair_old", Some("phone_a")));
    state.pairings.push(pairing_for("pair_new", None));

    let retired = retire_superseded_pairings(&mut state, "pair_new", None);

    assert!(retired.is_empty());
    assert_eq!(state.pairings.len(), 2);

    // Nor is a pairing with an unknown device id ever collateral damage.
    let mut state = RemoteState::default();
    state.pairings.push(pairing_for("pair_unknown", None));
    state
        .pairings
        .push(pairing_for("pair_new", Some("phone_a")));
    let retired = retire_superseded_pairings(&mut state, "pair_new", Some("phone_a"));
    assert!(retired.is_empty());
    assert_eq!(state.pairings.len(), 2);
}

#[test]
fn retiring_is_idempotent_and_keeps_the_claimed_pairing() {
    // Re-claiming the SAME pairing (a resume / repeat claim) must not retire it.
    let mut state = RemoteState::default();
    state
        .pairings
        .push(pairing_for("pair_only", Some("phone_a")));

    let retired = retire_superseded_pairings(&mut state, "pair_only", Some("phone_a"));

    assert!(retired.is_empty());
    assert_eq!(state.pairing_ids(), vec!["pair_only".to_string()]);
}
