//! FlightDeck Remote — the direct link end to end (REMOTE_PROTOCOL §5.10,
//! remote-control-inlb).
//!
//! A real [`DirectLink`] listens on loopback (the test stand-in for a Tailscale
//! address) with its outboxes in a temp directory on the real filesystem. A
//! phone-protocol client — `RelayFrame` JSON over a real WebSocket, a P-256
//! identity key for the direct auth signature, and the real E2E channel for the
//! payloads — connects to it exactly as the iPhone does over Tailscale. The
//! desktop side is driven through the same router the relay client's outbound
//! thread uses, so what is under test is the production path from "the bridge
//! sealed an envelope" to "the phone opened it", and back.
//!
//! No PTY and no relay are needed, so this runs on every platform.

use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use flightdeck::contracts::RealFs;
use flightdeck::remote::client::FileRemoteStore;
use flightdeck::remote::crypto::E2eChannel;
use flightdeck::remote::direct::outbox::OutboxLimits;
use flightdeck::remote::direct::{BindPlan, DirectLink, DirectOptions, OutboundRouter};
use flightdeck::remote::identity::DeviceIdentity;
use flightdeck::remote::state::{Pairing, PairingTransport, RemoteState};
use flightdeck::remote::{RemoteInbound, RemoteOutbound};
use flightdeck_remote_protocol::relay::direct_auth_message;
use flightdeck_remote_protocol::{
    AgentEvent, ClientInfo, CommandBody, CommandId, DeepLink, DesktopToPhone, DeviceId,
    EncryptedEnvelope, EventId, EventKind, PairingId, PhoneCommand, PresenceState, ProjectId,
    RelayErrorCode, RelayFrame, Role, SessionId, StateSnapshot, PROTOCOL_VERSION,
};
use tempfile::TempDir;

const PAIRING: &str = "pair_direct";
const SALT: &[u8] = b"4729";
const MACHINE: &str = "Ruud's Test Mac";
const WAIT: Duration = Duration::from_secs(5);
/// How long "nothing more arrives" is watched for.
const QUIET: Duration = Duration::from_millis(400);

// ---------------------------------------------------------------------------
// Fixture: a desktop and a phone that paired in direct mode.
// ---------------------------------------------------------------------------

struct Fixture {
    dir: TempDir,
    phone: DeviceIdentity,
    desktop_chan: E2eChannel,
    phone_chan: E2eChannel,
    state: RemoteState,
}

impl Fixture {
    fn new() -> Self {
        let desktop = DeviceIdentity::generate();
        let phone = DeviceIdentity::generate();
        let desktop_chan = E2eChannel::derive(
            &desktop.private_key_bytes(),
            phone.public_key_x963(),
            PAIRING,
            SALT,
            Role::Desktop,
        )
        .unwrap();
        let phone_chan = E2eChannel::derive(
            &phone.private_key_bytes(),
            desktop.public_key_x963(),
            PAIRING,
            SALT,
            Role::Phone,
        )
        .unwrap();
        // What the relay client persists for a direct-mode claim (§5.2).
        let mut p = Pairing::new(PAIRING);
        p.peer_device_id = Some(phone.device_id().to_string());
        p.peer_key_agreement_public_key = Some(phone.public_key_base64());
        p.peer_device_public_key = Some(phone.public_key_base64());
        p.claim_token = Some("4729".to_string());
        p.established = true;
        p.transport = PairingTransport::Direct;
        let mut state = RemoteState::default();
        state.pairings.push(p);
        Fixture {
            dir: tempfile::tempdir().unwrap(),
            phone,
            desktop_chan,
            phone_chan,
            state,
        }
    }

    fn start(&self, limits: OutboxLimits) -> (DirectLink, Receiver<RemoteInbound>) {
        let (tx, rx) = channel();
        let link = DirectLink::start(
            DirectOptions {
                state_dir: self.dir.path().join("remote-direct"),
                fs: Arc::new(RealFs),
                bind: BindPlan::Fixed(vec!["127.0.0.1:0".parse().unwrap()]),
                limits,
                machine_name: Some(MACHINE.to_string()),
            },
            &self.state,
            tx,
        )
        .expect("runtime");
        (link, rx)
    }

    /// The desktop "emits": seal like the bridge and hand the envelope to the
    /// router exactly as the relay client's outbound thread does.
    fn emit(&self, router: &Arc<dyn OutboundRouter>, seq: u64, msg: &DesktopToPhone) {
        let sent_at_ms = 1_752_000_000_000 + seq as i64;
        let (nonce, ciphertext) = self
            .desktop_chan
            .seal(&serde_json::to_vec(msg).unwrap(), seq, sent_at_ms)
            .unwrap();
        let leftover = router.route(RemoteOutbound::SendEnvelope {
            pairing_id: PairingId::new(PAIRING),
            seq,
            sent_at_ms,
            nonce,
            ciphertext,
        });
        assert!(
            leftover.is_none(),
            "a direct pairing's envelope must never reach the relay"
        );
    }

    fn open(&self, env: &EncryptedEnvelope) -> DesktopToPhone {
        assert_eq!(env.sender, Role::Desktop);
        let plain = self
            .phone_chan
            .open(
                env.seq,
                env.sender,
                env.sent_at_ms,
                &env.nonce,
                &env.ciphertext,
            )
            .expect("the phone opens what the desktop sealed");
        serde_json::from_slice(&plain).unwrap()
    }

    /// A phone command sealed as phone→desktop envelope `seq`.
    fn command_envelope(&self, seq: u64, text: &str) -> EncryptedEnvelope {
        let cmd = PhoneCommand {
            command_id: CommandId::new(format!("cmd-{seq}")),
            issued_at_ms: 1_752_000_000_000,
            body: CommandBody::Reply {
                session_id: SessionId::new("s1"),
                text: text.to_string(),
                image_ids: Vec::new(),
            },
        };
        let sent_at_ms = 1_752_000_100_000 + seq as i64;
        let (nonce, ciphertext) = self
            .phone_chan
            .seal(&serde_json::to_vec(&cmd).unwrap(), seq, sent_at_ms)
            .unwrap();
        EncryptedEnvelope {
            pairing_id: PairingId::new(PAIRING),
            seq,
            sender: Role::Phone,
            sent_at_ms,
            nonce,
            ciphertext,
        }
    }

    fn open_command(&self, env: &EncryptedEnvelope) -> PhoneCommand {
        let plain = self
            .desktop_chan
            .open(
                env.seq,
                env.sender,
                env.sent_at_ms,
                &env.nonce,
                &env.ciphertext,
            )
            .expect("the desktop opens what the phone sealed");
        serde_json::from_slice(&plain).unwrap()
    }
}

fn snapshot() -> DesktopToPhone {
    DesktopToPhone::Snapshot(StateSnapshot {
        server_time_ms: 1_752_000_000_000,
        projects: Vec::new(),
        features: Vec::new(),
    })
}

fn event(n: u32) -> DesktopToPhone {
    DesktopToPhone::Event(AgentEvent {
        event_id: EventId::new(format!("ev-{n}")),
        kind: EventKind::NeedsInput {
            preview: format!("question {n}"),
        },
        deep_link: DeepLink {
            project_id: ProjectId::new("proj"),
            session_id: SessionId::new("s1"),
            item_id: None,
        },
        occurred_at_ms: 1_752_000_000_000 + i64::from(n),
        title: format!("fd-api needs input ({n})"),
    })
}

fn event_number(msg: &DesktopToPhone) -> u32 {
    match msg {
        DesktopToPhone::Event(e) => e.event_id.as_str()["ev-".len()..].parse().unwrap(),
        other => panic!("expected an event, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The phone side of the wire.
// ---------------------------------------------------------------------------

struct Phone {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl Phone {
    async fn connect(link: &DirectLink) -> Phone {
        let addr = link.local_addrs()[0];
        let endpoint = link.endpoint().expect("a bound link advertises itself");
        assert_eq!(endpoint.port, addr.port());
        let (ws, _) = connect_async(endpoint.ws_url()).await.expect("connect");
        Phone { ws }
    }

    async fn send(&mut self, frame: &RelayFrame) {
        self.ws
            .send(Message::Text(serde_json::to_string(frame).unwrap().into()))
            .await
            .expect("send");
    }

    /// The next relay frame, or `None` on close / timeout.
    async fn recv_within(&mut self, limit: Duration) -> Option<RelayFrame> {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            match tokio::time::timeout_at(deadline, self.ws.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    return Some(serde_json::from_str(t.as_str()).expect("valid frame"))
                }
                Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => {
                    return None
                }
                Ok(Some(Ok(_))) => continue,
            }
        }
    }

    async fn recv(&mut self) -> RelayFrame {
        self.recv_within(WAIT)
            .await
            .expect("a frame within the budget")
    }

    /// `hello` → `hello_ok` → `auth_challenge` → `auth_response` signed by
    /// `sign`, returning whatever answers the response.
    async fn authenticate_with(
        &mut self,
        device_id: &str,
        pairing_ids: &[&str],
        sign: impl FnOnce(&[u8]) -> String,
    ) -> RelayFrame {
        self.send(&RelayFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            role: Role::Phone,
            device_id: DeviceId::new(device_id),
            client: ClientInfo {
                app_version: "test".into(),
                platform: "ios".into(),
                os_version: None,
            },
            relay_password: None,
        })
        .await;
        match self.recv().await {
            RelayFrame::HelloOk {
                protocol_version,
                relay_mode,
                ..
            } => {
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert_eq!(relay_mode, None, "the Mac is not a relay (§5.10)");
            }
            other => panic!("expected hello_ok, got {other:?}"),
        }
        let nonce = match self.recv().await {
            RelayFrame::AuthChallenge { nonce, .. } => STANDARD.decode(nonce).unwrap(),
            other => panic!("expected auth_challenge, got {other:?}"),
        };
        self.send(&RelayFrame::AuthResponse {
            device_id: DeviceId::new(device_id),
            signature: sign(&nonce),
            pairing_ids: pairing_ids.iter().map(|p| PairingId::new(*p)).collect(),
            machine_name: None,
        })
        .await;
        self.recv().await
    }

    /// The happy handshake for `fx.phone`, through `peer_presence`.
    async fn authenticate(&mut self, fx: &Fixture) {
        let phone = &fx.phone;
        let reply = self
            .authenticate_with(phone.device_id(), &[PAIRING], |nonce| {
                STANDARD.encode(phone.sign(&direct_auth_message(nonce)))
            })
            .await;
        assert!(
            matches!(&reply, RelayFrame::AuthOk { pairing_ids } if pairing_ids == &[PairingId::new(PAIRING)]),
            "expected auth_ok, got {reply:?}"
        );
        match self.recv().await {
            RelayFrame::MachineName {
                pairing_id,
                machine_name,
            } => {
                assert_eq!(pairing_id.as_str(), PAIRING);
                assert_eq!(machine_name, MACHINE);
            }
            other => panic!("expected machine_name, got {other:?}"),
        }
        match self.recv().await {
            RelayFrame::PeerPresence {
                pairing_id,
                peer,
                state,
                ..
            } => {
                assert_eq!(pairing_id.as_str(), PAIRING);
                assert_eq!(peer, Role::Desktop);
                assert_eq!(state, PresenceState::Connected);
            }
            other => panic!("expected peer_presence, got {other:?}"),
        }
    }

    async fn resume(&mut self, from_seq: u64) {
        self.send(&RelayFrame::Resume {
            pairing_id: PairingId::new(PAIRING),
            from_seq,
        })
        .await;
    }

    async fn ack(&mut self, cursor: u64) {
        self.send(&RelayFrame::Ack {
            pairing_id: PairingId::new(PAIRING),
            cursor,
        })
        .await;
    }

    /// Exactly `n` envelopes, then nothing else for [`QUIET`] (pongs and acks
    /// in between are skipped).
    async fn envelopes(&mut self, n: usize) -> Vec<EncryptedEnvelope> {
        let mut out = Vec::new();
        while out.len() < n {
            match self.recv().await {
                RelayFrame::Envelope(env) => out.push(env),
                RelayFrame::Pong { .. } | RelayFrame::Ack { .. } => {}
                other => panic!("expected an envelope, got {other:?}"),
            }
        }
        self.assert_no_more_envelopes().await;
        out
    }

    async fn assert_no_more_envelopes(&mut self) {
        while let Some(frame) = self.recv_within(QUIET).await {
            if let RelayFrame::Envelope(env) = frame {
                panic!("unexpected extra envelope seq={}", env.seq);
            }
        }
    }

    /// Expect an `error` frame with `code`, skipping nothing.
    async fn expect_error(&mut self, code: RelayErrorCode) {
        match self.recv().await {
            RelayFrame::Error { code: got, .. } => assert_eq!(got, code),
            other => panic!("expected error {code:?}, got {other:?}"),
        }
    }

    async fn expect_closed(&mut self) {
        assert!(
            self.recv_within(WAIT).await.is_none(),
            "the Mac must close the socket"
        );
    }
}

fn seqs(envs: &[EncryptedEnvelope]) -> Vec<u64> {
    envs.iter().map(|e| e.seq).collect()
}

/// Wait (bounded) for an inbound message matching `pred`, returning it.
fn wait_inbound(
    rx: &Receiver<RemoteInbound>,
    mut pred: impl FnMut(&RemoteInbound) -> bool,
) -> RemoteInbound {
    let deadline = std::time::Instant::now() + WAIT;
    while std::time::Instant::now() < deadline {
        if let Ok(msg) = rx.recv_timeout(Duration::from_millis(50)) {
            if pred(&msg) {
                return msg;
            }
        }
    }
    panic!("the app never heard the expected message");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The acceptance criterion: the phone connects directly, receives the
/// snapshot and events, disconnects, misses events, reconnects and resumes with
/// no loss and no duplicates — and a Mac restart in between keeps the stream
/// continuous.
#[tokio::test]
async fn snapshot_and_events_survive_a_disconnect_and_a_mac_restart() {
    let fx = Fixture::new();
    let (link, rx) = fx.start(OutboxLimits::default());
    let router = link.router();

    // Desktop state before the phone ever connects (queued in the outbox).
    fx.emit(&router, 1, &snapshot());
    fx.emit(&router, 2, &event(1));

    let mut phone = Phone::connect(&link).await;
    phone.authenticate(&fx).await;
    wait_inbound(&rx, |m| {
        matches!(
            m,
            RemoteInbound::Presence {
                peer: Role::Phone,
                state: PresenceState::Connected,
                ..
            }
        )
    });
    phone.resume(0).await;
    let first = phone.envelopes(2).await;
    assert_eq!(seqs(&first), vec![1, 2]);
    assert!(matches!(fx.open(&first[0]), DesktopToPhone::Snapshot(_)));
    assert_eq!(event_number(&fx.open(&first[1])), 1);

    // Live delivery while connected.
    fx.emit(&router, 3, &event(2));
    let live = phone.envelopes(1).await;
    assert_eq!(seqs(&live), vec![3]);
    assert_eq!(event_number(&fx.open(&live[0])), 2);
    phone.ack(1).await; // the phone acks only part of what it holds
    wait_inbound(&rx, |m| {
        matches!(m, RemoteInbound::PeerAck { cursor: 1, .. })
    });

    // The phone goes away (cellular drop, app suspended).
    drop(phone);
    wait_inbound(&rx, |m| {
        matches!(
            m,
            RemoteInbound::Presence {
                peer: Role::Phone,
                state: PresenceState::Disconnected,
                ..
            }
        )
    });

    // The desktop keeps emitting into the outbox.
    fx.emit(&router, 4, &event(3));
    fx.emit(&router, 5, &event(4));

    // Reconnect and resume from the highest seq the phone holds (3).
    let mut phone = Phone::connect(&link).await;
    phone.authenticate(&fx).await;
    phone.resume(3).await;
    let missed = phone.envelopes(2).await;
    assert_eq!(
        seqs(&missed),
        vec![4, 5],
        "exactly the missed ones, in order"
    );
    assert_eq!(
        missed
            .iter()
            .map(|e| event_number(&fx.open(e)))
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    phone.ack(5).await;
    wait_inbound(&rx, |m| {
        matches!(m, RemoteInbound::PeerAck { cursor: 5, .. })
    });
    drop(phone);

    // Mac restart: the outbox and its seq continuity are on disk.
    fx.emit(&router, 6, &event(5));
    drop(router);
    drop(link);
    let (link, _rx) = fx.start(OutboxLimits::default());
    assert_eq!(
        link.outbound_high_water(PAIRING),
        Some(6),
        "an orderly restart continues the stream where it stopped"
    );
    let router = link.router();
    fx.emit(&router, 7, &event(6));
    let mut phone = Phone::connect(&link).await;
    phone.authenticate(&fx).await;
    phone.resume(5).await;
    let after_restart = phone.envelopes(2).await;
    assert_eq!(seqs(&after_restart), vec![6, 7]);
    assert_eq!(
        after_restart
            .iter()
            .map(|e| event_number(&fx.open(e)))
            .collect::<Vec<_>>(),
        vec![5, 6]
    );
}

/// Phone→desktop envelopes are deduped by seq exactly as relay-delivered ones:
/// a re-sent envelope is applied once and re-acked.
#[tokio::test]
async fn a_duplicate_phone_envelope_is_applied_once() {
    let fx = Fixture::new();
    let (link, rx) = fx.start(OutboxLimits::default());
    let mut phone = Phone::connect(&link).await;
    phone.authenticate(&fx).await;
    phone.resume(0).await;

    for (seq, text) in [
        (1, "one"),
        (2, "two"),
        (2, "two"),
        (3, "three"),
        (3, "three"),
    ] {
        phone
            .send(&RelayFrame::Envelope(fx.command_envelope(seq, text)))
            .await;
        match phone.recv().await {
            RelayFrame::Ack { pairing_id, cursor } => {
                assert_eq!(pairing_id.as_str(), PAIRING);
                assert_eq!(cursor, seq, "every envelope, duplicate or not, is acked");
            }
            other => panic!("expected an ack, got {other:?}"),
        }
    }

    let mut delivered = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while std::time::Instant::now() < deadline {
        if let Ok(RemoteInbound::Envelope(env)) = rx.recv_timeout(Duration::from_millis(50)) {
            delivered.push(env);
        }
    }
    assert_eq!(
        seqs(&delivered),
        vec![1, 2, 3],
        "each seq reaches the app once"
    );
    let texts: Vec<String> = delivered
        .iter()
        .map(|e| match fx.open_command(e).body {
            CommandBody::Reply { text, .. } => text,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(texts, vec!["one", "two", "three"]);
}

/// Unknown pairing, a signature from another key, the relay's (domain-less)
/// signature, a different device and a non-phone role are all refused with
/// `auth_failed` and a close.
#[tokio::test]
async fn unauthenticated_phones_are_refused() {
    let fx = Fixture::new();
    let (link, _rx) = fx.start(OutboxLimits::default());
    let phone = &fx.phone;
    let stranger = DeviceIdentity::generate();

    // Unknown pairing.
    let mut c = Phone::connect(&link).await;
    let reply = c
        .authenticate_with(phone.device_id(), &["pair_nope"], |n| {
            STANDARD.encode(phone.sign(&direct_auth_message(n)))
        })
        .await;
    assert!(
        matches!(
            reply,
            RelayFrame::Error {
                code: RelayErrorCode::AuthFailed,
                ..
            }
        ),
        "{reply:?}"
    );
    c.expect_closed().await;

    // Right device id, wrong key.
    let mut c = Phone::connect(&link).await;
    let reply = c
        .authenticate_with(phone.device_id(), &[PAIRING], |n| {
            STANDARD.encode(stranger.sign(&direct_auth_message(n)))
        })
        .await;
    assert!(
        matches!(
            reply,
            RelayFrame::Error {
                code: RelayErrorCode::AuthFailed,
                ..
            }
        ),
        "{reply:?}"
    );
    c.expect_closed().await;

    // The relay-style signature over the bare nonce (no domain prefix).
    let mut c = Phone::connect(&link).await;
    let reply = c
        .authenticate_with(phone.device_id(), &[PAIRING], |n| {
            STANDARD.encode(phone.sign(n))
        })
        .await;
    assert!(
        matches!(
            reply,
            RelayFrame::Error {
                code: RelayErrorCode::AuthFailed,
                ..
            }
        ),
        "{reply:?}"
    );
    c.expect_closed().await;

    // Another device signing for itself.
    let mut c = Phone::connect(&link).await;
    let reply = c
        .authenticate_with(stranger.device_id(), &[PAIRING], |n| {
            STANDARD.encode(stranger.sign(&direct_auth_message(n)))
        })
        .await;
    assert!(
        matches!(
            reply,
            RelayFrame::Error {
                code: RelayErrorCode::AuthFailed,
                ..
            }
        ),
        "{reply:?}"
    );
    c.expect_closed().await;

    // A desktop role is not served at all.
    let mut c = Phone::connect(&link).await;
    c.send(&RelayFrame::Hello {
        protocol_version: PROTOCOL_VERSION,
        role: Role::Desktop,
        device_id: DeviceId::new("dev_desktop"),
        client: ClientInfo {
            app_version: "test".into(),
            platform: "macos".into(),
            os_version: None,
        },
        relay_password: None,
    })
    .await;
    c.expect_error(RelayErrorCode::AuthFailed).await;
    c.expect_closed().await;

    // And the real phone still gets in afterwards.
    let mut c = Phone::connect(&link).await;
    c.authenticate(&fx).await;
}

/// The phone unpairs over the direct link: the Mac drops the pairing exactly as
/// for a relay-delivered revoke — gone from `remote.json`, outbox deleted, the
/// app told — and the phone is refused from then on.
#[tokio::test]
async fn a_revoked_phone_is_refused() {
    let fx = Fixture::new();
    let (link, rx) = fx.start(OutboxLimits::default());
    // Wire the relay client's store the way `start_remote_at` does.
    let remote_json = fx.dir.path().join("remote.json");
    let store = link.wrap_store(Box::new(FileRemoteStore::at(&remote_json)));
    store.save(&fx.state);
    let router = link.router();
    fx.emit(&router, 1, &snapshot());
    let outbox_dir = fx.dir.path().join("remote-direct");
    assert!(std::fs::read_dir(&outbox_dir).unwrap().next().is_some());

    let mut phone = Phone::connect(&link).await;
    phone.authenticate(&fx).await;
    phone
        .send(&RelayFrame::Revoke {
            pairing_id: PairingId::new(PAIRING),
        })
        .await;
    match phone.recv().await {
        RelayFrame::PairingRevoked { pairing_id } => assert_eq!(pairing_id.as_str(), PAIRING),
        other => panic!("expected pairing_revoked, got {other:?}"),
    }
    phone.expect_closed().await;
    wait_inbound(
        &rx,
        |m| matches!(m, RemoteInbound::PairingRevoked { pairing_id } if pairing_id.as_str() == PAIRING),
    );
    let on_disk = std::fs::read_to_string(&remote_json).unwrap();
    assert!(
        !on_disk.contains(PAIRING),
        "the pairing record is gone: {on_disk}"
    );
    assert!(
        std::fs::read_dir(&outbox_dir).unwrap().next().is_none(),
        "the outbox is deleted"
    );
    // The client's in-memory copy saving again must not resurrect it.
    store.save(&fx.state);
    assert!(!std::fs::read_to_string(&remote_json)
        .unwrap()
        .contains(PAIRING));
    // Its envelopes still never reach the relay.
    assert!(router
        .route(RemoteOutbound::SendEnvelope {
            pairing_id: PairingId::new(PAIRING),
            seq: 2,
            sent_at_ms: 0,
            nonce: String::new(),
            ciphertext: String::new(),
        })
        .is_none());

    let phone_id = &fx.phone;
    let mut again = Phone::connect(&link).await;
    let reply = again
        .authenticate_with(phone_id.device_id(), &[PAIRING], |n| {
            STANDARD.encode(phone_id.sign(&direct_auth_message(n)))
        })
        .await;
    assert!(
        matches!(
            reply,
            RelayFrame::Error {
                code: RelayErrorCode::AuthFailed,
                ..
            }
        ),
        "{reply:?}"
    );
    again.expect_closed().await;
}

/// Unpairing on the Mac (the relay client saving without the pairing) tells a
/// phone that is connected right now, and closes its link.
#[tokio::test]
async fn unpairing_on_the_mac_tells_the_connected_phone() {
    let fx = Fixture::new();
    let (link, _rx) = fx.start(OutboxLimits::default());
    let store = link.wrap_store(Box::new(FileRemoteStore::at(
        fx.dir.path().join("remote.json"),
    )));
    store.save(&fx.state);
    let mut phone = Phone::connect(&link).await;
    phone.authenticate(&fx).await;
    store.save(&RemoteState::default());
    match phone.recv().await {
        RelayFrame::PairingRevoked { pairing_id } => assert_eq!(pairing_id.as_str(), PAIRING),
        other => panic!("expected pairing_revoked, got {other:?}"),
    }
}

/// An unpair landing while the link is still announcing itself (after
/// `auth_ok`, before the session starts watching the registry) still reaches
/// the phone, rather than slipping past a session that subscribed too late.
#[tokio::test]
async fn unpairing_during_the_announcement_still_tells_the_phone() {
    let fx = Fixture::new();
    let (link, _rx) = fx.start(OutboxLimits::default());
    let store = link.wrap_store(Box::new(FileRemoteStore::at(
        fx.dir.path().join("remote.json"),
    )));
    store.save(&fx.state);
    let mut phone = Phone::connect(&link).await;
    let phone_key = &fx.phone;
    let reply = phone
        .authenticate_with(phone_key.device_id(), &[PAIRING], |nonce| {
            STANDARD.encode(phone_key.sign(&direct_auth_message(nonce)))
        })
        .await;
    assert!(matches!(reply, RelayFrame::AuthOk { .. }), "{reply:?}");
    store.save(&RemoteState::default());
    loop {
        match phone.recv().await {
            RelayFrame::MachineName { .. } | RelayFrame::PeerPresence { .. } => continue,
            RelayFrame::PairingRevoked { pairing_id } => {
                assert_eq!(pairing_id.as_str(), PAIRING);
                break;
            }
            other => panic!("expected pairing_revoked, got {other:?}"),
        }
    }
}

/// Bootstrap and push frames are not served on the direct link (`bad_frame`),
/// data-plane frames need auth first, other paths do not upgrade, and the link
/// answers pings.
#[tokio::test]
async fn the_direct_link_serves_only_its_own_frames() {
    let fx = Fixture::new();
    let (link, _rx) = fx.start(OutboxLimits::default());

    // Wrong path: no upgrade.
    let addr = link.local_addrs()[0];
    assert!(connect_async(format!("ws://{addr}/ws")).await.is_err());

    let mut phone = Phone::connect(&link).await;
    phone.authenticate(&fx).await;
    phone
        .send(&RelayFrame::RegisterPushToken {
            pairing_id: PairingId::new(PAIRING),
            token: "abc".into(),
            environment: flightdeck_remote_protocol::ApnsEnvironment::Sandbox,
        })
        .await;
    phone.expect_error(RelayErrorCode::BadFrame).await;
    phone
        .send(&RelayFrame::PairingClaim {
            claim_token: "4729".into(),
            device_id: DeviceId::new(fx.phone.device_id()),
            device_public_key: fx.phone.public_key_base64(),
            key_agreement_public_key: fx.phone.public_key_base64(),
            role: Role::Phone,
        })
        .await;
    phone.expect_error(RelayErrorCode::BadFrame).await;
    phone
        .send(&RelayFrame::Resume {
            pairing_id: PairingId::new("pair_other"),
            from_seq: 0,
        })
        .await;
    phone.expect_error(RelayErrorCode::UnknownPairing).await;
    phone.send(&RelayFrame::Ping { client_time_ms: 42 }).await;
    match phone.recv().await {
        RelayFrame::Pong { client_time_ms, .. } => assert_eq!(client_time_ms, 42),
        other => panic!("expected pong, got {other:?}"),
    }

    // Before auth: data-plane frames are refused.
    let mut early = Phone::connect(&link).await;
    early
        .send(&RelayFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            role: Role::Phone,
            device_id: DeviceId::new(fx.phone.device_id()),
            client: ClientInfo {
                app_version: "test".into(),
                platform: "ios".into(),
                os_version: None,
            },
            relay_password: None,
        })
        .await;
    assert!(matches!(early.recv().await, RelayFrame::HelloOk { .. }));
    assert!(matches!(
        early.recv().await,
        RelayFrame::AuthChallenge { .. }
    ));
    early.resume(0).await;
    early.expect_error(RelayErrorCode::NotAuthenticated).await;
}

/// A resume from before what an overflowing outbox still holds is a
/// `seq_violation`; the phone's recovery (`resume` from 0) then gets the
/// retained tail rather than another `seq_violation`.
#[tokio::test]
async fn an_overflowed_outbox_answers_seq_violation_then_recovers() {
    let fx = Fixture::new();
    let (link, _rx) = fx.start(OutboxLimits {
        max_envelopes: 3,
        max_bytes: usize::MAX,
    });
    let router = link.router();
    for seq in 1..=5 {
        fx.emit(&router, seq, &event(seq as u32));
    }
    let mut phone = Phone::connect(&link).await;
    phone.authenticate(&fx).await;
    phone.resume(1).await;
    phone.expect_error(RelayErrorCode::SeqViolation).await;
    phone.resume(0).await;
    let tail = phone.envelopes(3).await;
    assert_eq!(seqs(&tail), vec![3, 4, 5]);
}
