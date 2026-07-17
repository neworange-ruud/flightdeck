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
            },
        );
        // Drop the listener/socket; the client will keep retrying.
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
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
                },
            );
        }
    });

    let cfg = RemoteConfig {
        enabled: true,
        relay_url: format!("ws://{addr}/ws"),
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
