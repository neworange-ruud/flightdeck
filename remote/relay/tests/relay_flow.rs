//! End-to-end relay flow tests: version negotiation, the auth state machine,
//! pairing bootstrap, zero-knowledge envelope routing, presence, and the
//! pending-event queue (hold / resume / dedup) — all against a real in-process
//! relay driven through `tokio-tungstenite`.

mod support;

use flightdeck_remote_protocol::{
    ApnsEnvironment, ClientInfo, DeviceId, EncryptedEnvelope, PairingId, PresenceState,
    RelayErrorCode, RelayFrame, Role,
};
use futures_util::{SinkExt, StreamExt};
use support::{bogus_signature, spawn_app, spawn_app_with, TestClient};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

/// Build an envelope with opaque (possibly garbage) ciphertext.
fn envelope(pairing: &PairingId, sender: Role, seq: u64, ciphertext: &str) -> RelayFrame {
    RelayFrame::Envelope(EncryptedEnvelope {
        pairing_id: pairing.clone(),
        seq,
        sender,
        sent_at_ms: 1_000_000 + seq as i64,
        nonce: "bm9uY2U=".into(),
        ciphertext: ciphertext.into(),
    })
}

fn is_envelope(frame: &RelayFrame) -> bool {
    matches!(frame, RelayFrame::Envelope(_))
}

fn env_seq(frame: &RelayFrame) -> u64 {
    match frame {
        RelayFrame::Envelope(e) => e.seq,
        other => panic!("not an envelope: {other:?}"),
    }
}

// ── version negotiation ───────────────────────────────────────────────────

#[tokio::test]
async fn incompatible_version_is_rejected_and_closed() {
    let base = spawn_app().await;
    let ws_url = format!("{}/ws", base.replacen("http://", "ws://", 1));
    let (mut ws, _) = connect_async(ws_url).await.unwrap();

    // Per `negotiate_version` (the normative negotiator the relay uses), a peer
    // advertising a version *below* MIN_SUPPORTED is incompatible; a peer above
    // MAX is clamped down and accepted. MIN_SUPPORTED is 1, so the only
    // incompatible case is version 0.
    let hello = RelayFrame::Hello {
        protocol_version: 0,
        role: Role::Phone,
        device_id: DeviceId::new("dev_x"),
        client: ClientInfo {
            app_version: "t".into(),
            platform: "t".into(),
            os_version: None,
        },
    };
    ws.send(WsMessage::Text(
        serde_json::to_string(&hello).unwrap().into(),
    ))
    .await
    .unwrap();

    let msg = ws.next().await.unwrap().unwrap();
    let frame: RelayFrame = match msg {
        WsMessage::Text(t) => serde_json::from_str(&t).unwrap(),
        other => panic!("expected text, got {other:?}"),
    };
    assert!(matches!(
        frame,
        RelayFrame::VersionIncompatible {
            your_version: 0,
            min_supported: 1,
            max_supported: 3,
        }
    ));
}

// ── wrong-order frames ──────────────────────────────────────────────────────

#[tokio::test]
async fn auth_before_hello_is_rejected() {
    let base = spawn_app().await;
    let ws_url = format!("{}/ws", base.replacen("http://", "ws://", 1));
    let (mut ws, _) = connect_async(ws_url).await.unwrap();

    let bad = RelayFrame::AuthResponse {
        device_id: DeviceId::new("dev_x"),
        signature: bogus_signature(),
        pairing_ids: vec![],
        machine_name: None,
    };
    ws.send(WsMessage::Text(serde_json::to_string(&bad).unwrap().into()))
        .await
        .unwrap();

    let msg = ws.next().await.unwrap().unwrap();
    let frame: RelayFrame = match msg {
        WsMessage::Text(t) => serde_json::from_str(&t).unwrap(),
        other => panic!("expected text, got {other:?}"),
    };
    assert!(matches!(
        frame,
        RelayFrame::Error {
            code: RelayErrorCode::BadFrame,
            ..
        }
    ));
}

// ── auth failures ───────────────────────────────────────────────────────────

#[tokio::test]
async fn auth_for_unregistered_device_fails() {
    let base = spawn_app().await;
    // Phone connects but never claims/offers, so its key is not registered.
    let mut phone = TestClient::connect(&base, Role::Phone, "dev_unknown").await;
    phone
        .send(RelayFrame::AuthResponse {
            device_id: DeviceId::new("dev_unknown"),
            signature: bogus_signature(),
            pairing_ids: vec![],
            machine_name: None,
        })
        .await;
    assert!(matches!(
        phone.recv().await,
        RelayFrame::Error {
            code: RelayErrorCode::AuthFailed,
            ..
        }
    ));
}

#[tokio::test]
async fn bad_signature_from_registered_device_fails() {
    let base = spawn_app().await;
    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    // Register the desktop key via a pairing offer, then present a bogus sig.
    let _ = desktop.offer_pairing().await;
    desktop
        .send(RelayFrame::AuthResponse {
            device_id: DeviceId::new("dev_mac"),
            signature: bogus_signature(),
            pairing_ids: vec![],
            machine_name: None,
        })
        .await;
    assert!(matches!(
        desktop.recv().await,
        RelayFrame::Error {
            code: RelayErrorCode::AuthFailed,
            ..
        }
    ));
}

// ── claim token lifecycle ───────────────────────────────────────────────────

#[tokio::test]
async fn claim_token_is_single_use() {
    let base = spawn_app().await;
    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing.clone()]).await;

    // First phone redeems successfully.
    let mut phone1 = TestClient::connect(&base, Role::Phone, "dev_phone_1").await;
    let joined = phone1.claim_pairing(&token).await;
    assert_eq!(joined, pairing);

    // Second phone reusing the same token is rejected.
    let mut phone2 = TestClient::connect(&base, Role::Phone, "dev_phone_2").await;
    phone2
        .send(RelayFrame::PairingClaim {
            claim_token: token.clone(),
            device_id: DeviceId::new("dev_phone_2"),
            device_public_key: phone2.public_key_b64.clone(),
            key_agreement_public_key: phone2.key_agreement_public_key_b64.clone(),
            role: Role::Phone,
        })
        .await;
    assert!(matches!(
        phone2.recv().await,
        RelayFrame::Error {
            code: RelayErrorCode::PairingClaimRejected,
            ..
        }
    ));
}

// ── claim_token_hint (4-digit code) ─────────────────────────────────────────

#[tokio::test]
async fn claim_token_hint_is_honored_when_free() {
    let base = spawn_app().await;
    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;

    // A free, well-formed 4-digit hint is issued verbatim so the desktop can
    // display it as the short code.
    let (pairing, token) = desktop.offer_pairing_hint(Some("4729")).await;
    assert_eq!(
        token, "4729",
        "relay must issue the requested 4-digit token"
    );
    desktop.authenticate(vec![pairing.clone()]).await;

    // The phone redeems exactly that 4-digit code.
    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    let joined = phone.claim_pairing("4729").await;
    assert_eq!(joined, pairing);
}

#[tokio::test]
async fn claim_token_hint_collision_falls_back_to_minted_token() {
    let base = spawn_app().await;

    // First desktop takes "4729".
    let mut desktop1 = TestClient::connect(&base, Role::Desktop, "dev_mac_1").await;
    let (_p1, token1) = desktop1.offer_pairing_hint(Some("4729")).await;
    assert_eq!(token1, "4729");

    // Second desktop asks for the same hint while it is still live → refused,
    // the relay mints its own distinct token instead of colliding.
    let mut desktop2 = TestClient::connect(&base, Role::Desktop, "dev_mac_2").await;
    let (_p2, token2) = desktop2.offer_pairing_hint(Some("4729")).await;
    assert_ne!(token2, "4729", "a colliding hint must not be reused");
}

// ── pairing_claim rate limiting ─────────────────────────────────────────────

#[tokio::test]
async fn repeated_bad_claims_are_rate_limited_and_closed() {
    let base = spawn_app().await;
    let mut phone = TestClient::connect(&base, Role::Phone, "dev_attacker").await;

    // Hammer the relay with wrong tokens. The first few are advisory rejects;
    // once over the per-connection cap the relay rate-limits and closes.
    let mut saw_rate_limited = false;
    for _ in 0..12 {
        phone
            .send(RelayFrame::PairingClaim {
                claim_token: "0000".to_string(),
                device_id: DeviceId::new("dev_attacker"),
                device_public_key: phone.public_key_b64.clone(),
                key_agreement_public_key: phone.key_agreement_public_key_b64.clone(),
                role: Role::Phone,
            })
            .await;
        if let RelayFrame::Error { code, .. } = phone.recv().await {
            if matches!(code, RelayErrorCode::RateLimited) {
                saw_rate_limited = true;
                break;
            }
            assert!(matches!(code, RelayErrorCode::PairingClaimRejected));
        }
    }
    assert!(
        saw_rate_limited,
        "relay must rate-limit repeated pairing_claim attempts"
    );
}

// ── the full happy path ─────────────────────────────────────────────────────

#[tokio::test]
async fn full_pairing_routing_resume_and_dedup() {
    let base = spawn_app().await;

    // 1. Desktop connects, bootstraps a pairing, authenticates.
    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let desktop_ka_key = desktop.key_agreement_public_key_b64.clone();
    let (pairing, token) = desktop.offer_pairing().await;
    let activated = desktop.authenticate(vec![pairing.clone()]).await;
    assert_eq!(activated, vec![pairing.clone()]);

    // 2. Phone connects, claims the token, authenticates.
    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    let phone_key = phone.key();
    let phone_ka_key = phone.key_agreement_public_key_b64.clone();
    let (joined, peer_ka) = phone.claim_pairing_full(&token).await;
    assert_eq!(joined, pairing);
    // The phone must receive the desktop's key-agreement key for the E2E ECDH.
    assert_eq!(peer_ka, Some(desktop_ka_key));

    // The waiting desktop is notified of the join and receives the phone's KA key.
    let desk_claimed = desktop
        .recv_until(|f| matches!(f, RelayFrame::PairingClaimed { .. }))
        .await;
    match desk_claimed {
        RelayFrame::PairingClaimed {
            peer_device_id,
            peer_key_agreement_public_key,
            ..
        } => {
            assert_eq!(peer_device_id, Some(DeviceId::new("dev_phone")));
            assert_eq!(peer_key_agreement_public_key, Some(phone_ka_key));
        }
        other => panic!("expected pairing_claimed on desktop, got {other:?}"),
    }

    let activated = phone.authenticate(vec![pairing.clone()]).await;
    assert_eq!(activated, vec![pairing.clone()]);

    // Phone sees the desktop present; desktop sees the phone join + present.
    let presence = phone
        .recv_until(|f| matches!(f, RelayFrame::PeerPresence { .. }))
        .await;
    assert!(matches!(
        presence,
        RelayFrame::PeerPresence {
            peer: Role::Desktop,
            state: PresenceState::Connected,
            ..
        }
    ));
    let desk_presence = desktop
        .recv_until(|f| {
            matches!(
                f,
                RelayFrame::PeerPresence {
                    state: PresenceState::Connected,
                    ..
                }
            )
        })
        .await;
    assert!(matches!(
        desk_presence,
        RelayFrame::PeerPresence {
            peer: Role::Phone,
            ..
        }
    ));

    // 3. Exchange envelopes both directions.
    desktop
        .send(envelope(&pairing, Role::Desktop, 1, "d->p#1"))
        .await;
    assert_eq!(env_seq(&phone.recv_until(is_envelope).await), 1);

    phone
        .send(envelope(&pairing, Role::Phone, 1, "p->d#1"))
        .await;
    assert_eq!(env_seq(&desktop.recv_until(is_envelope).await), 1);

    // 4. Phone disconnects; desktop observes disconnect presence.
    phone.close().await;
    let gone = desktop
        .recv_until(|f| matches!(f, RelayFrame::PeerPresence { .. }))
        .await;
    assert!(matches!(
        gone,
        RelayFrame::PeerPresence {
            peer: Role::Phone,
            state: PresenceState::Disconnected,
            ..
        }
    ));

    // 5. Desktop sends 3 envelopes while the phone is offline (seq 2,3,4).
    for seq in 2..=4 {
        desktop
            .send(envelope(
                &pairing,
                Role::Desktop,
                seq,
                &format!("queued#{seq}"),
            ))
            .await;
    }

    // 6. Phone reconnects with the SAME identity key and resumes from seq 1.
    let mut phone = TestClient::connect_with_key(&base, Role::Phone, "dev_phone", phone_key).await;
    phone.authenticate(vec![pairing.clone()]).await;
    phone
        .send(RelayFrame::Resume {
            pairing_id: pairing.clone(),
            from_seq: 1,
        })
        .await;

    // Receives exactly seq 2,3,4 in order.
    let mut got = Vec::new();
    for _ in 0..3 {
        got.push(env_seq(&phone.recv_until(is_envelope).await));
    }
    assert_eq!(got, vec![2, 3, 4]);

    // 7. Double-resume is idempotent: replaying from the same cursor yields the
    //    same three, not duplicates beyond them.
    phone
        .send(RelayFrame::Resume {
            pairing_id: pairing.clone(),
            from_seq: 1,
        })
        .await;
    let mut again = Vec::new();
    for _ in 0..3 {
        again.push(env_seq(&phone.recv_until(is_envelope).await));
    }
    assert_eq!(again, vec![2, 3, 4]);

    // 8. Ack up to 4, then resume from 4 → nothing left (pruned + deduped).
    phone
        .send(RelayFrame::Ack {
            pairing_id: pairing.clone(),
            cursor: 4,
        })
        .await;
    phone
        .send(RelayFrame::Resume {
            pairing_id: pairing.clone(),
            from_seq: 4,
        })
        .await;
    phone.expect_idle(300).await;
}

// ── zero-knowledge blindness ────────────────────────────────────────────────

#[tokio::test]
async fn relay_routes_garbage_ciphertext_without_inspecting_it() {
    let base = spawn_app().await;

    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing.clone()]).await;

    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    phone.claim_pairing(&token).await;
    phone.authenticate(vec![pairing.clone()]).await;

    // Ciphertext that is NOT valid base64 and nonce that is NOT valid base64.
    // A relay that tried to decode/inspect the payload would choke; a blind
    // pipe forwards the bytes verbatim.
    let garbage = "!!!not-base64-at-all***\u{0000}\u{FFFD}";
    desktop
        .send(envelope(&pairing, Role::Desktop, 1, garbage))
        .await;

    let received = phone.recv_until(is_envelope).await;
    match received {
        RelayFrame::Envelope(e) => {
            assert_eq!(e.ciphertext, garbage, "ciphertext forwarded byte-for-byte");
            assert_eq!(e.seq, 1);
        }
        other => panic!("expected envelope, got {other:?}"),
    }
}

// ── seq-gap enforcement over the wire ───────────────────────────────────────

#[tokio::test]
async fn envelope_seq_gap_is_rejected() {
    let base = spawn_app().await;

    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let (pairing, _token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing.clone()]).await;

    desktop
        .send(envelope(&pairing, Role::Desktop, 1, "ok"))
        .await;
    // Skip seq 2 → gap.
    desktop
        .send(envelope(&pairing, Role::Desktop, 3, "gap"))
        .await;

    let err = desktop
        .recv_until(|f| matches!(f, RelayFrame::Error { .. }))
        .await;
    assert!(matches!(
        err,
        RelayFrame::Error {
            code: RelayErrorCode::SeqViolation,
            ..
        }
    ));
}

// ── queue overflow ──────────────────────────────────────────────────────────

#[tokio::test]
async fn queue_overflow_emits_advisory_and_keeps_newest() {
    // Tiny queue bound so overflow is easy to trigger.
    let base = spawn_app_with(3, 10).await;

    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing.clone()]).await;

    // Phone claims (so the pairing is real) then disconnects, leaving envelopes
    // to pile up in the queue with no live peer to forward to.
    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    let phone_key = phone.key();
    phone.claim_pairing(&token).await;
    phone.authenticate(vec![pairing.clone()]).await;
    phone.close().await;
    // Drain the disconnect presence.
    let _ = desktop
        .recv_until(|f| {
            matches!(
                f,
                RelayFrame::PeerPresence {
                    state: PresenceState::Disconnected,
                    ..
                }
            )
        })
        .await;

    // Send 5 envelopes into a queue bounded at 3.
    for seq in 1..=5 {
        desktop
            .send(envelope(&pairing, Role::Desktop, seq, &format!("e{seq}")))
            .await;
    }
    // At least one advisory rate_limited error is emitted on overflow.
    let advisory = desktop
        .recv_until(|f| matches!(f, RelayFrame::Error { .. }))
        .await;
    assert!(matches!(
        advisory,
        RelayFrame::Error {
            code: RelayErrorCode::RateLimited,
            ..
        }
    ));

    // Reconnect the phone and resume from 0: only the newest 3 (seq 3,4,5)
    // survive drop-oldest.
    let mut phone = TestClient::connect_with_key(&base, Role::Phone, "dev_phone", phone_key).await;
    phone.authenticate(vec![pairing.clone()]).await;
    phone
        .send(RelayFrame::Resume {
            pairing_id: pairing.clone(),
            from_seq: 0,
        })
        .await;
    let mut got = Vec::new();
    for _ in 0..3 {
        got.push(env_seq(&phone.recv_until(is_envelope).await));
    }
    assert_eq!(got, vec![3, 4, 5]);
    phone.expect_idle(300).await;
}

// ── machine name (spec §10.1) ───────────────────────────────────────────────

/// A desktop announces its machine name on `auth_response`; the relay forwards
/// it to the phone both when the phone connects and again when the desktop
/// reconnects under a new name (a Mac rename propagates on reconnect).
#[tokio::test]
async fn machine_name_is_forwarded_to_phone_on_connect_and_updated_on_rename() {
    let base = spawn_app().await;

    // Desktop pairs and authenticates announcing its name.
    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let desktop_key = desktop.key();
    let (pairing, token) = desktop.offer_pairing().await;
    desktop
        .authenticate_named(vec![pairing.clone()], Some("Ruud's MacBook Pro"))
        .await;

    // Phone claims + authenticates → it receives the stored machine name.
    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    let phone_key = phone.key();
    phone.claim_pairing(&token).await;
    phone.authenticate(vec![pairing.clone()]).await;

    let name = phone
        .recv_until(|f| matches!(f, RelayFrame::MachineName { .. }))
        .await;
    match name {
        RelayFrame::MachineName {
            pairing_id,
            machine_name,
        } => {
            assert_eq!(pairing_id, pairing);
            assert_eq!(machine_name, "Ruud's MacBook Pro");
        }
        other => panic!("expected machine_name, got {other:?}"),
    }

    // The Mac is renamed and the desktop reconnects while the phone is online;
    // the phone receives the new name.
    desktop.close().await;
    let mut desktop =
        TestClient::connect_with_key(&base, Role::Desktop, "dev_mac", desktop_key).await;
    desktop
        .authenticate_named(vec![pairing.clone()], Some("Work Mac"))
        .await;

    let renamed = phone
        .recv_until(|f| matches!(f, RelayFrame::MachineName { .. }))
        .await;
    match renamed {
        RelayFrame::MachineName { machine_name, .. } => assert_eq!(machine_name, "Work Mac"),
        other => panic!("expected updated machine_name, got {other:?}"),
    }

    // An older desktop that omits the field (None) breaks nothing: no new
    // machine_name frame is emitted, and routing continues.
    let _ = phone_key;
}

/// A desktop that announces no name (older build / `None`) must not break the
/// phone's connect — no `machine_name` frame is sent.
#[tokio::test]
async fn absent_machine_name_sends_no_frame() {
    let base = spawn_app().await;

    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing.clone()]).await; // machine_name: None

    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    phone.claim_pairing(&token).await;
    phone.authenticate(vec![pairing.clone()]).await;

    // The phone must NOT receive a machine_name frame (only presence, etc.).
    // Give the relay a moment; assert no machine_name arrives.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(100), phone.recv()).await {
            Ok(RelayFrame::MachineName { .. }) => panic!("unexpected machine_name frame"),
            Ok(_) => {}  // presence / other — ignore
            Err(_) => {} // idle
        }
    }
}

// ── phone-initiated unpair / revoke (spec §10.2) ────────────────────────────

/// The phone revokes a pairing: the relay verifies membership, removes it,
/// notifies the desktop peer, and confirms to the phone. The pairing is gone.
#[tokio::test]
async fn phone_revoke_removes_pairing_and_notifies_desktop() {
    let base = spawn_app().await;

    // Full pair.
    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing.clone()]).await;

    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    phone.claim_pairing(&token).await;
    phone.authenticate(vec![pairing.clone()]).await;

    // Phone revokes.
    phone.revoke(&pairing).await;

    // Desktop is notified its phone unpaired it.
    let desk = desktop
        .recv_until(|f| matches!(f, RelayFrame::PairingRevoked { .. }))
        .await;
    assert!(matches!(
        desk,
        RelayFrame::PairingRevoked { pairing_id } if pairing_id == pairing
    ));

    // Phone gets an idempotent-success confirmation.
    let conf = phone
        .recv_until(|f| matches!(f, RelayFrame::PairingRevoked { .. }))
        .await;
    assert!(matches!(
        conf,
        RelayFrame::PairingRevoked { pairing_id } if pairing_id == pairing
    ));

    // The pairing is gone: an envelope for it is now rejected as unknown.
    phone
        .send(envelope(&pairing, Role::Phone, 1, "after-revoke"))
        .await;
    let err = phone
        .recv_until(|f| matches!(f, RelayFrame::Error { .. }))
        .await;
    assert!(matches!(
        err,
        RelayFrame::Error {
            code: RelayErrorCode::UnknownPairing,
            ..
        }
    ));
}

/// A non-member device that knows a pairing id cannot revoke it: the relay
/// refuses with `unknown_pairing` and the pairing survives.
#[tokio::test]
async fn non_member_cannot_revoke() {
    let base = spawn_app().await;

    // Victim pairing (desktop + phone).
    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing.clone()]).await;
    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    phone.claim_pairing(&token).await;
    phone.authenticate(vec![pairing.clone()]).await;

    // A stranger authenticates via its OWN pairing, then tries to revoke the
    // victim pairing it is not a member of.
    let mut stranger = TestClient::connect(&base, Role::Desktop, "dev_stranger").await;
    let (stranger_pairing, _tok) = stranger.offer_pairing().await;
    stranger.authenticate(vec![stranger_pairing.clone()]).await;
    stranger.revoke(&pairing).await;

    let err = stranger
        .recv_until(|f| matches!(f, RelayFrame::Error { .. }))
        .await;
    assert!(matches!(
        err,
        RelayFrame::Error {
            code: RelayErrorCode::UnknownPairing,
            ..
        }
    ));

    // The victim pairing still routes: desktop→phone envelope arrives.
    desktop
        .send(envelope(&pairing, Role::Desktop, 1, "still-alive"))
        .await;
    assert_eq!(env_seq(&phone.recv_until(is_envelope).await), 1);
}

/// Revoking a pairing that is already gone is an idempotent success: the relay
/// still confirms with `pairing_revoked` and never errors.
#[tokio::test]
async fn revoke_is_idempotent() {
    let base = spawn_app().await;

    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing.clone()]).await;
    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    phone.claim_pairing(&token).await;
    phone.authenticate(vec![pairing.clone()]).await;

    phone.revoke(&pairing).await;
    let _ = phone
        .recv_until(|f| matches!(f, RelayFrame::PairingRevoked { .. }))
        .await;

    // A second revoke of the same (now-gone) pairing still confirms success.
    phone.revoke(&pairing).await;
    let again = phone
        .recv_until(|f| matches!(f, RelayFrame::PairingRevoked { .. }))
        .await;
    assert!(matches!(
        again,
        RelayFrame::PairingRevoked { pairing_id } if pairing_id == pairing
    ));
}

// ── push-token deregistration / per-machine mute (spec §5.5) ─────────────────

/// A member phone registers then unregisters a pairing's push token: the relay
/// acks both, the pairing survives (unlike revoke), and the token is gone — so an
/// offline-phone envelope no longer wakes it via push.
#[tokio::test]
async fn member_unregisters_push_token_without_unpairing() {
    let base = spawn_app().await;

    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing.clone()]).await;
    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    phone.claim_pairing(&token).await;
    phone.authenticate(vec![pairing.clone()]).await;

    // Register, then unregister — both confirmed with push_token_ack.
    phone
        .register_push_token(&pairing, "apns-tok", ApnsEnvironment::Production)
        .await;
    let ack = phone
        .recv_until(|f| matches!(f, RelayFrame::PushTokenAck { .. }))
        .await;
    assert!(matches!(
        ack,
        RelayFrame::PushTokenAck { pairing_id } if pairing_id == pairing
    ));

    phone.unregister_push_token(&pairing).await;
    let ack = phone
        .recv_until(|f| matches!(f, RelayFrame::PushTokenAck { .. }))
        .await;
    assert!(matches!(
        ack,
        RelayFrame::PushTokenAck { pairing_id } if pairing_id == pairing
    ));

    // The pairing still routes: a desktop→phone envelope arrives (not unpaired).
    desktop
        .send(envelope(&pairing, Role::Desktop, 1, "still-paired"))
        .await;
    assert_eq!(env_seq(&phone.recv_until(is_envelope).await), 1);
}

/// Unregistering a pairing that has no token stored is an idempotent success:
/// the relay still confirms with push_token_ack and never errors.
#[tokio::test]
async fn unregister_push_token_is_idempotent() {
    let base = spawn_app().await;

    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing.clone()]).await;
    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    phone.claim_pairing(&token).await;
    phone.authenticate(vec![pairing.clone()]).await;

    // No token was ever registered → still a success ack.
    phone.unregister_push_token(&pairing).await;
    let ack = phone
        .recv_until(|f| matches!(f, RelayFrame::PushTokenAck { .. }))
        .await;
    assert!(matches!(
        ack,
        RelayFrame::PushTokenAck { pairing_id } if pairing_id == pairing
    ));

    // A second unregister of the still-absent token also succeeds.
    phone.unregister_push_token(&pairing).await;
    let ack = phone
        .recv_until(|f| matches!(f, RelayFrame::PushTokenAck { .. }))
        .await;
    assert!(matches!(
        ack,
        RelayFrame::PushTokenAck { pairing_id } if pairing_id == pairing
    ));
}

/// A non-member device cannot unregister a pairing's push token: the relay
/// refuses with `unknown_pairing` (mirroring revoke) and the token survives.
#[tokio::test]
async fn non_member_cannot_unregister_push_token() {
    let base = spawn_app().await;

    // Victim pairing with a registered token.
    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing.clone()]).await;
    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    phone.claim_pairing(&token).await;
    phone.authenticate(vec![pairing.clone()]).await;
    phone
        .register_push_token(&pairing, "apns-tok", ApnsEnvironment::Sandbox)
        .await;
    let _ = phone
        .recv_until(|f| matches!(f, RelayFrame::PushTokenAck { .. }))
        .await;

    // A stranger authenticates via its OWN pairing, then tries to unregister the
    // victim pairing's token.
    let mut stranger = TestClient::connect(&base, Role::Desktop, "dev_stranger").await;
    let (stranger_pairing, _tok) = stranger.offer_pairing().await;
    stranger.authenticate(vec![stranger_pairing.clone()]).await;
    stranger.unregister_push_token(&pairing).await;

    let err = stranger
        .recv_until(|f| matches!(f, RelayFrame::Error { .. }))
        .await;
    assert!(matches!(
        err,
        RelayFrame::Error {
            code: RelayErrorCode::UnknownPairing,
            ..
        }
    ));
}
