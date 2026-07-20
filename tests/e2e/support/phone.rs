//! Rust "phone" driver for the FlightDeck Remote E2E harness (issue c3m.6).
//!
//! This is the relay-plane client **plus** the E2E sealing layer that stands in
//! for the iPhone in Tier A of the harness (see the plan, "Tier A — Protocol
//! E2E"). It lets the Rust capability test (c3m.7) drive a *real* desktop
//! through a *real* relay exactly as the iOS app would: it speaks the full §5
//! handshake as the `phone` role, redeems the desktop's pairing offer, derives
//! the end-to-end [`E2eChannel`], and then seals [`PhoneCommand`]s / opens
//! [`DesktopToPhone`] feed messages.
//!
//! # Why this mirrors, rather than imports, the relay `TestClient`
//!
//! The relay's own `remote/relay/tests/support/mod.rs::TestClient` speaks the
//! identical handshake, but it lives in the separate `remote/` Cargo workspace
//! and is built on **async** `tokio-tungstenite`. The root `flightdeck` crate
//! (which owns this test target) deliberately uses **blocking** `tungstenite`
//! and has no async runtime (see the `Cargo.toml` note on the `tungstenite`
//! dep). So this driver cannot reuse `TestClient` directly — it re-implements
//! the same frame sequence synchronously. The mirrored `TestClient` methods are
//! called out at each step below.
//!
//! # Handshake (phone role), mirroring `TestClient`
//!
//! 1. connect + `hello` + consume `hello_ok`/`auth_challenge`
//!    (`TestClient::connect_with_key`).
//! 2. `pairing_claim { claim_token }` -> `pairing_claimed` carrying the
//!    desktop's key-agreement public key + the `pairing_id`
//!    (`TestClient::claim_pairing_full`). We present our **own** software P-256
//!    key-agreement public key in the claim, exactly as the phone does (the iOS
//!    identity key is Secure-Enclave signing-only, so KA is always a separate
//!    software key).
//! 3. `auth_response` signing the challenge nonce, activating the pairing
//!    (`TestClient::authenticate`).
//!
//! # E2E derivation / seal / open (salt = claim-token bytes)
//!
//! Once paired we derive [`E2eChannel`] from *our* KA private scalar, the
//! desktop's KA public key, the `pairing_id`, and salt = the claim-token bytes
//! (`b"4729"` for the autopair harness — the reconciled §7.1 code-path
//! contract: the desktop derives with the identical salt in
//! `src/remote/pairing.rs::build_channel`). Outgoing commands are sealed under
//! the phone->desktop key with a per-pairing monotonic `seq` (starting at 1);
//! incoming envelopes are opened using the header fields the desktop sealed
//! under (`seq`/`sender`/`sent_at_ms`), which are bound as AEAD AAD.
#![allow(dead_code)]

use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use flightdeck::remote::crypto::E2eChannel;
use flightdeck_remote_protocol::{
    ClientInfo, CommandBody, CommandId, DesktopToPhone, DeviceId, EncryptedEnvelope, PairingId,
    PhoneCommand, RelayFrame, Role, PROTOCOL_VERSION,
};
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use rand_core::OsRng;
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, WebSocket};

/// Generous timeout for the blocking TCP + WebSocket upgrade.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Socket read timeout used *during* the upgrade (before it is tightened).
const UPGRADE_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll granularity for `read` once past the upgrade: a read with no data
/// waiting returns `WouldBlock`/`TimedOut` this often, letting the recv loops
/// re-check their overall deadline instead of blocking forever.
const READ_POLL: Duration = Duration::from_millis(200);
/// Overall budget for the whole handshake (each individual frame is expected
/// to arrive well within this).
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(15);

/// Return the current unix time in milliseconds (the `sent_at_ms` clock).
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The base64 (standard, padded) X9.63 uncompressed SEC1 public point of a
/// P-256 key — the wire encoding every key field on the relay plane uses.
fn public_key_b64(key: &SigningKey) -> String {
    STANDARD.encode(key.verifying_key().to_encoded_point(false).as_bytes())
}

/// A blocking relay-plane WebSocket connection. Both the phone driver and the
/// in-test desktop stand-in are built on one of these; it owns the socket and a
/// cloned control handle used only to retime the socket's read timeout after
/// the upgrade (`SO_RCVTIMEO` is shared across dup'd descriptors — the same
/// trick `src/remote/client.rs` uses).
struct Conn {
    ws: WebSocket<TcpStream>,
    ctl: TcpStream,
}

impl Conn {
    /// Open a plain `ws://` connection and perform the WebSocket upgrade. Only
    /// `ws://` is supported (the harness relay is always local, plaintext);
    /// a `wss://` URL panics, which is a test-setup error, not a runtime path.
    fn connect(ws_url: &str) -> Self {
        let request = ws_url
            .into_client_request()
            .unwrap_or_else(|e| panic!("bad relay url {ws_url:?}: {e}"));
        let uri = request.uri();
        let scheme = uri.scheme_str().unwrap_or("");
        assert!(
            scheme.eq_ignore_ascii_case("ws"),
            "phone driver only supports plaintext ws:// (got {scheme:?})"
        );
        let host = uri
            .host()
            .unwrap_or_else(|| panic!("relay url {ws_url:?} has no host"))
            .to_string();
        let port = uri.port_u16().unwrap_or(80);

        let addr = (host.as_str(), port)
            .to_socket_addrs()
            .unwrap_or_else(|e| panic!("resolve {host}:{port}: {e}"))
            .next()
            .unwrap_or_else(|| panic!("{host}:{port} resolved to no address"));

        let tcp = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
            .unwrap_or_else(|e| panic!("tcp connect to {addr}: {e}"));
        // A generous timeout for the HTTP upgrade read; tightened below.
        tcp.set_read_timeout(Some(UPGRADE_READ_TIMEOUT)).ok();
        tcp.set_write_timeout(Some(CONNECT_TIMEOUT)).ok();
        let ctl = tcp
            .try_clone()
            .unwrap_or_else(|e| panic!("clone relay socket handle: {e}"));

        let (ws, _resp) = tungstenite::client(request, tcp)
            .unwrap_or_else(|e| panic!("ws upgrade to {ws_url}: {e}"));

        // Past the upgrade: switch to a short poll so recv loops can honor a
        // caller-supplied deadline.
        ctl.set_read_timeout(Some(READ_POLL)).ok();
        Conn { ws, ctl }
    }

    /// Serialize and send a relay frame (panics on socket error).
    fn send(&mut self, frame: &RelayFrame) {
        let json = serde_json::to_string(frame).expect("relay frame serializes");
        self.ws
            .send(Message::Text(json))
            .unwrap_or_else(|e| panic!("send {frame:?}: {e}"));
    }

    /// Receive the next *meaningful* relay frame, or `None` if `deadline`
    /// passes first. Interleaved presence and pong frames are transparently
    /// skipped so callers only see the frames they care about (mirrors the
    /// intent of `TestClient::recv_until`, minus the async).
    fn recv(&mut self, deadline: Instant) -> Option<RelayFrame> {
        loop {
            match self.ws.read() {
                Ok(Message::Text(text)) => {
                    match serde_json::from_str::<RelayFrame>(&text) {
                        // Relay-plane background chatter that is never an answer
                        // to a handshake step or an application message.
                        Ok(RelayFrame::PeerPresence { .. }) | Ok(RelayFrame::Pong { .. }) => {}
                        Ok(frame) => return Some(frame),
                        Err(e) => panic!("unparseable relay frame {text:?}: {e}"),
                    }
                }
                Ok(Message::Close(_)) => panic!("relay closed the connection mid-session"),
                // Control / binary / raw frames: ignore (tungstenite auto-pongs
                // pings). Catch-all mirrors `src/remote/client.rs::read_frame`.
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if Instant::now() >= deadline {
                        return None;
                    }
                }
                Err(e) => panic!("relay read error: {e}"),
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }

    /// Receive the next meaningful frame, panicking on timeout/close. Used for
    /// the handshake, where a specific frame is always expected next.
    fn recv_expect(&mut self, deadline: Instant, what: &str) -> RelayFrame {
        self.recv(deadline)
            .unwrap_or_else(|| panic!("timed out waiting for {what}"))
    }
}

/// A paired, ready-to-drive phone endpoint: a live authenticated relay
/// connection plus the derived [`E2eChannel`]. Construct with
/// [`PhoneDriver::pair`]; then [`PhoneDriver::send_command`] /
/// [`PhoneDriver::command`] to issue commands and [`PhoneDriver::recv_desktop`]
/// / [`PhoneDriver::recv_until`] to read the desktop feed.
pub struct PhoneDriver {
    conn: Conn,
    channel: E2eChannel,
    pairing_id: PairingId,
    /// Next phone->desktop envelope `seq` (per-pairing, per-sender, starts 1).
    next_seq: u64,
    /// Monotonic counter behind the auto-generated command ids.
    next_command: u64,
}

impl PhoneDriver {
    /// Perform the full §5 phone-role handshake against the relay at
    /// `relay_ws_url`, redeem `claim_token`, derive the E2E channel, and return
    /// a ready-to-use driver. Panics (with a descriptive message) on any
    /// protocol deviation — this is test infrastructure, so a failed pairing
    /// should fail the test loudly and immediately.
    ///
    /// `claim_token` is both the token redeemed on the relay plane *and* the
    /// E2E HKDF salt (its UTF-8 bytes) — the reconciled §7.1 code-path
    /// contract, matched by the desktop in `build_channel`.
    pub fn pair(relay_ws_url: &str, claim_token: &str) -> Self {
        let deadline = Instant::now() + HANDSHAKE_DEADLINE;

        // A P-256 identity (signing) key for relay auth, and a *separate*
        // software P-256 key-agreement key for the E2E ECDH — the iOS split.
        let identity = SigningKey::random(&mut OsRng);
        let ka = SigningKey::random(&mut OsRng);
        let device_id = DeviceId::new("phone-e2e-driver");

        let mut conn = Conn::connect(relay_ws_url);

        // 1. hello -> hello_ok, auth_challenge (mirror connect_with_key).
        conn.send(&RelayFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            role: Role::Phone,
            device_id: device_id.clone(),
            client: ClientInfo {
                app_version: "e2e-phone".into(),
                platform: "test".into(),
                os_version: None,
            },
        });
        match conn.recv_expect(deadline, "hello_ok") {
            RelayFrame::HelloOk {
                protocol_version, ..
            } => assert_eq!(protocol_version, PROTOCOL_VERSION, "negotiated version"),
            other => panic!("expected hello_ok, got {other:?}"),
        }
        let nonce = match conn.recv_expect(deadline, "auth_challenge") {
            RelayFrame::AuthChallenge { nonce, .. } => STANDARD
                .decode(&nonce)
                .unwrap_or_else(|e| panic!("challenge nonce not base64: {e}")),
            other => panic!("expected auth_challenge, got {other:?}"),
        };

        // 2. pairing_claim -> pairing_claimed (mirror claim_pairing_full). We
        //    present our KA public key; the relay hands back the desktop's.
        conn.send(&RelayFrame::PairingClaim {
            claim_token: claim_token.to_string(),
            device_id: device_id.clone(),
            device_public_key: public_key_b64(&identity),
            key_agreement_public_key: public_key_b64(&ka),
            role: Role::Phone,
        });
        let (pairing_id, desktop_ka_b64) = match conn.recv_expect(deadline, "pairing_claimed") {
            RelayFrame::PairingClaimed {
                pairing_id,
                peer_key_agreement_public_key,
                ..
            } => (
                pairing_id,
                peer_key_agreement_public_key
                    .expect("relay must return the desktop's key-agreement public key"),
            ),
            RelayFrame::Error { code, message, .. } => {
                panic!("pairing claim rejected: {code:?} — {message}")
            }
            other => panic!("expected pairing_claimed, got {other:?}"),
        };

        // 3. auth_response over the challenge -> auth_ok (mirror authenticate).
        let signature: Signature = identity.sign(&nonce);
        conn.send(&RelayFrame::AuthResponse {
            device_id,
            signature: STANDARD.encode(signature.to_bytes()),
            pairing_ids: vec![pairing_id.clone()],
            machine_name: None,
        });
        match conn.recv_expect(deadline, "auth_ok") {
            RelayFrame::AuthOk { pairing_ids } => {
                assert!(
                    pairing_ids.contains(&pairing_id),
                    "auth_ok did not activate our pairing (got {pairing_ids:?})"
                );
            }
            other => panic!("expected auth_ok, got {other:?}"),
        }

        // Derive the E2E channel: our KA scalar + the desktop's KA public key,
        // salt = claim-token bytes, role = phone.
        let desktop_ka = STANDARD
            .decode(&desktop_ka_b64)
            .unwrap_or_else(|e| panic!("desktop KA key not base64: {e}"));
        let channel = E2eChannel::derive(
            &ka.to_bytes(),
            &desktop_ka,
            pairing_id.as_str(),
            claim_token.as_bytes(),
            Role::Phone,
        )
        .expect("derive phone E2E channel");

        PhoneDriver {
            conn,
            channel,
            pairing_id,
            next_seq: 1,
            next_command: 1,
        }
    }

    /// The pairing id shared with the desktop.
    pub fn pairing_id(&self) -> &PairingId {
        &self.pairing_id
    }

    /// Seal and send a fully-formed [`PhoneCommand`]. The command's own
    /// `command_id` / `issued_at_ms` are used verbatim; the envelope's `seq`
    /// and `sent_at_ms` are assigned here (and bound into the AEAD AAD).
    pub fn send_command(&mut self, cmd: PhoneCommand) {
        let plaintext = serde_json::to_vec(&cmd).expect("serialize phone command");
        let seq = self.next_seq;
        self.next_seq += 1;
        let sent_at_ms = now_ms();
        let (nonce, ciphertext) = self
            .channel
            .seal(&plaintext, seq, sent_at_ms)
            .expect("seal phone command");
        self.conn.send(&RelayFrame::Envelope(EncryptedEnvelope {
            pairing_id: self.pairing_id.clone(),
            seq,
            sender: Role::Phone,
            sent_at_ms,
            nonce,
            ciphertext,
        }));
    }

    /// Ergonomic helper: wrap `body` in a [`PhoneCommand`] with a freshly
    /// generated `command_id` + issue timestamp, send it, and return the
    /// `command_id` so the caller can await its [`CommandAck`].
    pub fn command(&mut self, body: CommandBody) -> CommandId {
        let command_id = CommandId::new(format!("cmd_e2e_{:08}", self.next_command));
        self.next_command += 1;
        let cmd = PhoneCommand {
            command_id: command_id.clone(),
            issued_at_ms: now_ms(),
            body,
        };
        self.send_command(cmd);
        command_id
    }

    /// Wait up to `timeout` for the next desktop feed message, open it, and
    /// return the deserialized [`DesktopToPhone`]. Panics on timeout.
    pub fn recv_desktop(&mut self, timeout: Duration) -> DesktopToPhone {
        let deadline = Instant::now() + timeout;
        loop {
            let frame = self
                .conn
                .recv(deadline)
                .expect("timed out waiting for a desktop envelope");
            match frame {
                RelayFrame::Envelope(env) => return self.open(env),
                // Relay-plane acks/errors are not application messages; an
                // error frame is worth surfacing loudly.
                RelayFrame::Ack { .. } => {}
                // The desktop announces its machine name on connect (spec §5.7);
                // it is relay-plane metadata, not an application message — skip it.
                RelayFrame::MachineName { .. } => {}
                RelayFrame::Error { code, message, .. } => {
                    panic!("relay error while awaiting desktop feed: {code:?} — {message}")
                }
                other => panic!("unexpected relay frame while awaiting envelope: {other:?}"),
            }
        }
    }

    /// Wait up to `timeout` for a desktop message matching `pred`, discarding
    /// any that do not match. Panics on timeout. Handy for skipping past the
    /// initial snapshot to a specific `command_ack` / `status_update`.
    pub fn recv_until(
        &mut self,
        timeout: Duration,
        pred: impl Fn(&DesktopToPhone) -> bool,
    ) -> DesktopToPhone {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                remaining > Duration::ZERO,
                "timed out waiting for a matching desktop message"
            );
            let msg = self.recv_desktop(remaining);
            if pred(&msg) {
                return msg;
            }
        }
    }

    /// Open one received envelope with the channel, using the header fields the
    /// desktop sealed under (which are authenticated as AAD).
    fn open(&self, env: EncryptedEnvelope) -> DesktopToPhone {
        let plaintext = self
            .channel
            .open(
                env.seq,
                env.sender,
                env.sent_at_ms,
                &env.nonce,
                &env.ciphertext,
            )
            .expect("open desktop envelope");
        serde_json::from_slice::<DesktopToPhone>(&plaintext).expect("deserialize desktop message")
    }
}

#[cfg(test)]
mod tests {
    use super::super::relay::RelayHandle;
    use super::*;
    use flightdeck_remote_protocol::ProjectId;
    use flightdeck_remote_protocol::{
        CommandOutcome, ProjectState, SessionState, StateSnapshot, StatusRollup, StatusUpdate,
    };

    const CLAIM_TOKEN: &str = "4729";

    // -----------------------------------------------------------------------
    // Local crypto round-trip: no network. Proves key generation + the
    // salt = claim-token-bytes derivation produce a channel that seals a
    // `PhoneCommand` on the phone side and opens it on the desktop side (and
    // that the reverse direction works too). This is the always-on floor of
    // coverage for the sealing path.
    // -----------------------------------------------------------------------
    #[test]
    fn e2e_channel_round_trips_a_phone_command_locally() {
        let pairing_id = "pair_local";
        // Independent phone + desktop key-agreement keypairs.
        let phone_ka = SigningKey::random(&mut OsRng);
        let desktop_ka = SigningKey::random(&mut OsRng);
        let phone_ka_pub = desktop_ka_bytes(&phone_ka);
        let desktop_ka_pub = desktop_ka_bytes(&desktop_ka);

        let phone = E2eChannel::derive(
            &phone_ka.to_bytes(),
            &desktop_ka_pub,
            pairing_id,
            CLAIM_TOKEN.as_bytes(),
            Role::Phone,
        )
        .expect("phone derive");
        let desktop = E2eChannel::derive(
            &desktop_ka.to_bytes(),
            &phone_ka_pub,
            pairing_id,
            CLAIM_TOKEN.as_bytes(),
            Role::Desktop,
        )
        .expect("desktop derive");

        // Phone seals a real PhoneCommand; desktop opens + deserializes it.
        let cmd = PhoneCommand {
            command_id: CommandId::new("cmd_1"),
            issued_at_ms: 1_752_412_800_000,
            body: CommandBody::RequestSnapshot { project_id: None },
        };
        let plaintext = serde_json::to_vec(&cmd).unwrap();
        let (nonce, ct) = phone.seal(&plaintext, 1, 1_752_412_800_000).expect("seal");
        let opened = desktop
            .open(1, Role::Phone, 1_752_412_800_000, &nonce, &ct)
            .expect("desktop opens phone command");
        assert_eq!(
            serde_json::from_slice::<PhoneCommand>(&opened).unwrap(),
            cmd
        );

        // Reverse direction: desktop -> phone snapshot.
        let snapshot = DesktopToPhone::Snapshot(StateSnapshot {
            server_time_ms: 1_752_412_801_000,
            projects: vec![],
        });
        let snap_bytes = serde_json::to_vec(&snapshot).unwrap();
        let (n2, c2) = desktop
            .seal(&snap_bytes, 1, 1_752_412_801_000)
            .expect("seal d2p");
        let opened2 = phone
            .open(1, Role::Desktop, 1_752_412_801_000, &n2, &c2)
            .expect("phone opens snapshot");
        assert_eq!(
            serde_json::from_slice::<DesktopToPhone>(&opened2).unwrap(),
            snapshot
        );
    }

    /// X9.63 public bytes of a key (helper mirrors what the driver sends).
    fn desktop_ka_bytes(key: &SigningKey) -> Vec<u8> {
        key.verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec()
    }

    // -----------------------------------------------------------------------
    // Full relay round-trip: real relay binary + a minimal in-test desktop
    // stand-in that speaks the desktop half of the §5 handshake (offer + auth)
    // and the desktop half of the E2E channel. This proves the *whole*
    // PhoneDriver — connect, handshake, KA-key exchange, salt-`b"4729"`
    // derivation, seal, open — end to end against the actual relay.
    //
    // NOTE for c3m.7 (DesktopHandle wiring): this deliberately uses a
    // relay-plane stand-in rather than `super::desktop::DesktopHandle`. The
    // stand-in makes the driver's coverage self-contained and deterministic
    // (no PTY, no fixture scripts), which is the right shape for a driver unit
    // test. c3m.7 should add the *real*-desktop capability suite on top, using
    // c3m.5's actual API (confirmed in `desktop.rs`):
    //     let relay = RelayHandle::spawn();
    //     let (fixture, _dir) = super::super::desktop::make_fixture(relay.port());
    //     let _desktop = super::super::desktop::DesktopHandle::spawn(&fixture);
    //     let mut phone = PhoneDriver::pair(&relay.ws_url(), "4729");
    //     let snap = phone.recv_until(TIMEOUT, |m| matches!(m, DesktopToPhone::Snapshot(_)));
    // then drive real capabilities and assert real desktop-side side effects.
    // (Note: `DesktopHandle::spawn` takes only the fixture dir — the relay port
    // is baked into the fixture's config by `make_fixture`, not passed here.)
    // -----------------------------------------------------------------------
    #[test]
    fn phone_pairs_and_round_trips_through_the_real_relay() {
        let relay = RelayHandle::spawn();
        let ws_url = relay.ws_url();

        // --- The desktop stand-in: offer pairing (hint 4729) + authenticate,
        //     activating the pairing so the relay will route to it and notify
        //     it when the phone claims. Reuses one signing key as both its
        //     identity and its KA key, exactly like the real desktop does. ---
        let desktop_key = SigningKey::random(&mut OsRng);
        let desktop_device = DeviceId::new("desktop-standin");
        let mut desktop = Conn::connect(&ws_url);
        let d_deadline = Instant::now() + HANDSHAKE_DEADLINE;

        desktop.send(&RelayFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            role: Role::Desktop,
            device_id: desktop_device.clone(),
            client: ClientInfo {
                app_version: "e2e-desktop".into(),
                platform: "test".into(),
                os_version: None,
            },
        });
        assert!(matches!(
            desktop.recv_expect(d_deadline, "hello_ok"),
            RelayFrame::HelloOk { .. }
        ));
        let d_nonce = match desktop.recv_expect(d_deadline, "auth_challenge") {
            RelayFrame::AuthChallenge { nonce, .. } => STANDARD.decode(&nonce).unwrap(),
            other => panic!("desktop expected auth_challenge, got {other:?}"),
        };
        desktop.send(&RelayFrame::PairingOffer {
            device_id: desktop_device.clone(),
            device_public_key: public_key_b64(&desktop_key),
            key_agreement_public_key: public_key_b64(&desktop_key),
            role: Role::Desktop,
            claim_token_hint: Some(CLAIM_TOKEN.to_string()),
        });
        let (pairing_id, issued_token) = match desktop.recv_expect(d_deadline, "pairing_offer_ok") {
            RelayFrame::PairingOfferOk {
                pairing_id,
                claim_token,
                ..
            } => (pairing_id, claim_token),
            other => panic!("desktop expected pairing_offer_ok, got {other:?}"),
        };
        assert_eq!(
            issued_token, CLAIM_TOKEN,
            "relay should honor the free 4-digit hint verbatim"
        );
        let d_sig: Signature = desktop_key.sign(&d_nonce);
        desktop.send(&RelayFrame::AuthResponse {
            device_id: desktop_device.clone(),
            signature: STANDARD.encode(d_sig.to_bytes()),
            pairing_ids: vec![pairing_id.clone()],
            machine_name: None,
        });
        assert!(matches!(
            desktop.recv_expect(d_deadline, "auth_ok"),
            RelayFrame::AuthOk { .. }
        ));

        // --- The phone pairs (full handshake + derivation). ---
        let mut phone = PhoneDriver::pair(&ws_url, CLAIM_TOKEN);
        assert_eq!(phone.pairing_id(), &pairing_id, "same pairing on both ends");

        // The relay now notifies the desktop that the phone joined, carrying the
        // phone's KA key. Read it and derive the desktop channel.
        let phone_ka_b64 = match desktop.recv_expect(d_deadline, "pairing_claimed (desktop side)") {
            RelayFrame::PairingClaimed {
                peer_key_agreement_public_key,
                ..
            } => peer_key_agreement_public_key.expect("relay hands the desktop the phone's KA key"),
            other => panic!("desktop expected pairing_claimed, got {other:?}"),
        };
        let phone_ka = STANDARD.decode(&phone_ka_b64).unwrap();
        let desktop_channel = E2eChannel::derive(
            &desktop_key.to_bytes(),
            &phone_ka,
            pairing_id.as_str(),
            CLAIM_TOKEN.as_bytes(),
            Role::Desktop,
        )
        .expect("desktop derive");

        // --- desktop -> phone: send a sealed snapshot; the phone opens it. ---
        let snapshot = DesktopToPhone::Snapshot(StateSnapshot {
            server_time_ms: now_ms(),
            projects: vec![ProjectState {
                project_id: ProjectId::new("proj_1"),
                name: "fixture".into(),
                rollup: StatusRollup {
                    dot: flightdeck_remote_protocol::RollupDot::Idle,
                    summary: "0 agents".into(),
                    working: 0,
                    idle: 0,
                    needs_input: 0,
                    manual: 0,
                    agent_count: 0,
                },
                sessions: Vec::<SessionState>::new(),
            }],
        });
        let snap_bytes = serde_json::to_vec(&snapshot).unwrap();
        // The envelope's `sent_at_ms` MUST equal the value passed to `seal`
        // (it is bound as AEAD AAD), so seal with an explicit timestamp and
        // echo the same one into the envelope.
        let snap_ts = now_ms();
        let (n, c) = desktop_channel
            .seal(&snap_bytes, 1, snap_ts)
            .expect("seal snapshot");
        desktop.send(&RelayFrame::Envelope(EncryptedEnvelope {
            pairing_id: pairing_id.clone(),
            seq: 1,
            sender: Role::Desktop,
            sent_at_ms: snap_ts,
            nonce: n,
            ciphertext: c,
        }));

        let got = phone.recv_desktop(Duration::from_secs(5));
        match got {
            DesktopToPhone::Snapshot(s) => {
                assert_eq!(s.projects.len(), 1);
                assert_eq!(s.projects[0].name, "fixture");
            }
            other => panic!("expected a snapshot, got {other:?}"),
        }

        // --- phone -> desktop: send a command; desktop opens + acks it;
        //     phone awaits the ack. Full bidirectional round trip. ---
        let command_id = phone.command(CommandBody::RequestSnapshot { project_id: None });
        let env = match desktop.recv_expect(d_deadline, "phone command envelope") {
            RelayFrame::Envelope(env) => env,
            other => panic!("desktop expected an envelope, got {other:?}"),
        };
        let opened = desktop_channel
            .open(
                env.seq,
                env.sender,
                env.sent_at_ms,
                &env.nonce,
                &env.ciphertext,
            )
            .expect("desktop opens phone command");
        let received: PhoneCommand = serde_json::from_slice(&opened).unwrap();
        assert_eq!(received.command_id, command_id);
        assert!(matches!(
            received.body,
            CommandBody::RequestSnapshot { project_id: None }
        ));

        // Desktop acks; phone waits for the matching command_ack.
        let ack = DesktopToPhone::CommandAck(flightdeck_remote_protocol::CommandAck {
            command_id: command_id.clone(),
            outcome: CommandOutcome::Accepted,
            message: None,
        });
        let ack_bytes = serde_json::to_vec(&ack).unwrap();
        let ack_ts = now_ms();
        let (an, ac) = desktop_channel
            .seal(&ack_bytes, 2, ack_ts)
            .expect("seal ack");
        desktop.send(&RelayFrame::Envelope(EncryptedEnvelope {
            pairing_id: pairing_id.clone(),
            seq: 2,
            sender: Role::Desktop,
            sent_at_ms: ack_ts,
            nonce: an,
            ciphertext: ac,
        }));

        let matched = phone.recv_until(
            Duration::from_secs(5),
            |m| matches!(m, DesktopToPhone::CommandAck(a) if a.command_id == command_id),
        );
        match matched {
            DesktopToPhone::CommandAck(a) => assert_eq!(a.outcome, CommandOutcome::Accepted),
            other => panic!("expected a command_ack, got {other:?}"),
        }

        // StatusUpdate is a distinct variant the driver must also pass through
        // unaltered — exercise it to keep the open path honest across variants.
        let status = DesktopToPhone::StatusUpdate(StatusUpdate { updates: vec![] });
        let status_bytes = serde_json::to_vec(&status).unwrap();
        let s_ts = now_ms();
        let (sn, sc) = desktop_channel
            .seal(&status_bytes, 3, s_ts)
            .expect("seal status");
        desktop.send(&RelayFrame::Envelope(EncryptedEnvelope {
            pairing_id: pairing_id.clone(),
            seq: 3,
            sender: Role::Desktop,
            sent_at_ms: s_ts,
            nonce: sn,
            ciphertext: sc,
        }));
        assert!(matches!(
            phone.recv_desktop(Duration::from_secs(5)),
            DesktopToPhone::StatusUpdate(_)
        ));

        drop(phone);
        drop(relay);
    }
}
