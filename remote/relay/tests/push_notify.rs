//! Desktop-requested encrypted alert pushes (spec §5.5.1–§5.5.3,
//! remote-control-l9xe), driven end to end through the real relay with the live
//! [`ApnsPushService`](flightdeck_relay::apns::ApnsPushService) over a fake APNs
//! transport:
//!
//! - `push_notify` on the WebSocket → exactly one alert request, collapse id =
//!   event id, and nothing in the body beyond the generic text + `fd_push`;
//! - an envelope queued for an offline phone → no push at all;
//! - refusals: non-member desktop, phone sender, oversized ciphertext, no token;
//! - `POST /v1/push`: signature, timestamp, membership, role, relay password,
//!   size and rate-limit checks with the spec's status codes and JSON bodies;
//! - v4 pairing fields: `direct_endpoint` to the phone, `peer_device_public_key`
//!   to the desktop, and `hello_ok.relay_mode`.

mod support;

use std::time::Duration;

use flightdeck_relay::apns::{ApnsRequest, ApnsSendError};
use flightdeck_remote_protocol::relay::{PUSH_HTTP_PATH, RELAY_PASSWORD_HEADER};
use flightdeck_remote_protocol::{
    ApnsEnvironment, DeviceId, DirectEndpoint, EncryptedEnvelope, EventId, PairingId, PushClass,
    PushErrorBody, PushNotifyBody, PushRequest, RelayErrorCode, RelayFrame, RelayMode, Role,
};
use support::{
    hello_probe, now_ms, spawn_app_with_fake_apns, spawn_app_with_fake_apns_state, TestClient,
};

const EVENT_ID: &str = "6f9a1c2e-3b4d-4e5f-8a7b-1c2d3e4f5a6b";

fn push_body(pairing: &PairingId, class: PushClass) -> PushNotifyBody {
    PushNotifyBody {
        pairing_id: pairing.clone(),
        event_id: EventId::new(EVENT_ID),
        class,
        nonce: "AAECAwQFBgcICQoL".into(),
        ciphertext: "AP5g0H3CJyAB8Vf8b/H9pSudjvO+qoKbU/jVgc5XAl4Sgjg6r3Rhijvj793DPzr4".into(),
    }
}

fn is_error(frame: &RelayFrame) -> bool {
    matches!(frame, RelayFrame::Error { .. })
}

/// A desktop and phone, paired and authenticated, with the phone's APNs token
/// registered (`with_token`). Returns `(desktop, phone, pairing)`.
async fn paired(base: &str, with_token: bool) -> (TestClient, TestClient, PairingId) {
    let mut desktop = TestClient::connect(base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing.clone()]).await;

    let mut phone = TestClient::connect(base, Role::Phone, "dev_phone").await;
    phone.claim_pairing(&token).await;
    phone.authenticate(vec![pairing.clone()]).await;
    if with_token {
        phone
            .register_push_token(&pairing, "apns-device-token", ApnsEnvironment::Sandbox)
            .await;
        phone
            .recv_until(|f| matches!(f, RelayFrame::PushTokenAck { .. }))
            .await;
    }
    (desktop, phone, pairing)
}

fn headers(req: &ApnsRequest) -> std::collections::HashMap<String, String> {
    req.headers.iter().cloned().collect()
}

/// Assert `req` is the spec §5.5.2 encrypted alert for `push` — and that its
/// body is *exactly* the generic alert plus `fd_push`, so no plaintext beyond
/// the class can ride along.
fn assert_encrypted_alert(req: &ApnsRequest, push: &PushNotifyBody, generic_body: &str) {
    assert_eq!(req.authority, "api.sandbox.push.apple.com");
    assert_eq!(req.path, "/3/device/apns-device-token");
    let h = headers(req);
    assert_eq!(h["apns-push-type"], "alert");
    assert_eq!(h["apns-priority"], "10");
    assert_eq!(h["apns-collapse-id"], push.event_id.as_str());
    assert_eq!(h["apns-topic"], "agency.neworange.flightdeck.remote");
    let expiration: i64 = h["apns-expiration"].parse().unwrap();
    let now_secs = now_ms() / 1000;
    assert!(
        (now_secs + 24 * 3600 - 60..=now_secs + 24 * 3600 + 60).contains(&expiration),
        "expiration ≈ now + 24 h, got {expiration}"
    );

    let level = if push.class == PushClass::NeedsInput {
        "time-sensitive"
    } else {
        "active"
    };
    let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({
            "aps": {
                "alert": { "title": "FlightDeck", "body": generic_body },
                "mutable-content": 1,
                "sound": "default",
                "interruption-level": level,
                "thread-id": push.pairing_id.as_str(),
            },
            "fd_push": {
                "v": 1,
                "pairing_id": push.pairing_id.as_str(),
                "event_id": push.event_id.as_str(),
                "class": push.class.as_str(),
                "nonce": push.nonce,
                "ciphertext": push.ciphertext,
            },
        })
    );
}

// ── WebSocket push_notify ────────────────────────────────────────────────

#[tokio::test]
async fn push_notify_sends_exactly_one_encrypted_alert() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (mut desktop, _phone, pairing) = paired(&base, true).await;

    let push = push_body(&pairing, PushClass::NeedsInput);
    desktop.push_notify(push.clone()).await;

    let sent = apns.wait_for(1).await;
    assert_encrypted_alert(&sent[0], &push, "An agent needs your input");
    // Fire-and-forget: no reply, no error, and no second request.
    desktop.expect_no_frame_matching(300, is_error).await;
    apns.expect_no_more(1, 100).await;
}

#[tokio::test]
async fn each_class_gets_its_generic_text() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (mut desktop, _phone, pairing) = paired(&base, true).await;

    let error = push_body(&pairing, PushClass::Error);
    desktop.push_notify(error.clone()).await;
    let sent = apns.wait_for(1).await;
    assert_encrypted_alert(&sent[0], &error, "An agent hit an error");

    let mut done = push_body(&pairing, PushClass::Done);
    done.event_id = EventId::new("0b9c8d7e-1111-4222-8333-944455566677");
    desktop.push_notify(done.clone()).await;
    let sent = apns.wait_for(2).await;
    assert_encrypted_alert(&sent[1], &done, "An agent is done");
}

#[tokio::test]
async fn envelope_to_an_offline_phone_triggers_no_push() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (mut desktop, phone, pairing) = paired(&base, true).await;
    phone.close().await;
    desktop
        .recv_until(|f| matches!(f, RelayFrame::PeerPresence { .. }))
        .await;

    // Several envelopes queue for the offline phone; before v4 each one fired
    // a wake push.
    for seq in 1..=3 {
        desktop
            .send(RelayFrame::Envelope(EncryptedEnvelope {
                pairing_id: pairing.clone(),
                seq,
                sender: Role::Desktop,
                sent_at_ms: 0,
                nonce: "bm9uY2U=".into(),
                ciphertext: "opaque".into(),
            }))
            .await;
    }
    desktop.expect_no_frame_matching(300, is_error).await;
    apns.expect_no_more(0, 100).await;
}

#[tokio::test]
async fn non_member_desktop_gets_unknown_pairing() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (_desktop, _phone, pairing) = paired(&base, true).await;

    // Another desktop with its own pairing tries to push to someone else's.
    let mut intruder = TestClient::connect(&base, Role::Desktop, "dev_intruder").await;
    let (own, _) = intruder.offer_pairing().await;
    intruder.authenticate(vec![own]).await;
    intruder
        .push_notify(push_body(&pairing, PushClass::NeedsInput))
        .await;

    let frame = intruder.recv_until(is_error).await;
    assert!(
        matches!(
            &frame,
            RelayFrame::Error { code: RelayErrorCode::UnknownPairing, pairing_id: Some(p), .. }
                if *p == pairing
        ),
        "got {frame:?}"
    );
    apns.expect_no_more(0, 100).await;

    // An unknown pairing id is refused the same way.
    intruder
        .push_notify(push_body(&PairingId::new("pair_nope"), PushClass::Done))
        .await;
    assert!(matches!(
        intruder.recv_until(is_error).await,
        RelayFrame::Error {
            code: RelayErrorCode::UnknownPairing,
            ..
        }
    ));
}

#[tokio::test]
async fn phone_sending_push_notify_is_bad_frame() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (_desktop, mut phone, pairing) = paired(&base, true).await;

    phone
        .push_notify(push_body(&pairing, PushClass::NeedsInput))
        .await;
    assert!(matches!(
        phone.recv_until(is_error).await,
        RelayFrame::Error {
            code: RelayErrorCode::BadFrame,
            ..
        }
    ));
    apns.expect_no_more(0, 100).await;
}

#[tokio::test]
async fn oversized_ciphertext_is_bad_frame() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (mut desktop, _phone, pairing) = paired(&base, true).await;

    let mut push = push_body(&pairing, PushClass::NeedsInput);
    push.ciphertext = "A".repeat(3001);
    desktop.push_notify(push).await;
    let frame = desktop.recv_until(is_error).await;
    assert!(
        matches!(
            &frame,
            RelayFrame::Error { code: RelayErrorCode::BadFrame, pairing_id: Some(p), .. }
                if *p == pairing
        ),
        "got {frame:?}"
    );
    apns.expect_no_more(0, 100).await;
}

#[tokio::test]
async fn no_registered_token_sends_nothing_and_says_nothing() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (mut desktop, _phone, pairing) = paired(&base, false).await;

    desktop
        .push_notify(push_body(&pairing, PushClass::Done))
        .await;
    desktop.expect_no_frame_matching(300, is_error).await;
    apns.expect_no_more(0, 100).await;
}

#[tokio::test]
async fn a_dead_token_is_purged_after_a_permanent_rejection() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (mut desktop, _phone, pairing) = paired(&base, true).await;
    apns.script(vec![Err(ApnsSendError::Permanent(
        "apns status 410: Unregistered".into(),
    ))]);

    desktop
        .push_notify(push_body(&pairing, PushClass::NeedsInput))
        .await;
    apns.wait_for(1).await;
    // The token is gone, so the next push is a silent no-op.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut next = push_body(&pairing, PushClass::Done);
    next.event_id = EventId::new("evt-after-purge");
    desktop.push_notify(next).await;
    apns.expect_no_more(1, 300).await;
}

// ── HTTPS POST /v1/push ──────────────────────────────────────────────────

/// POST `body` (already JSON) to the push endpoint, with an optional relay
/// password header. Returns `(status, parsed error body if any)`.
async fn post_raw(
    base: &str,
    body: Vec<u8>,
    password: Option<&str>,
) -> (u16, Option<PushErrorBody>) {
    let mut req = reqwest::Client::new()
        .post(format!("{base}{PUSH_HTTP_PATH}"))
        .header("content-type", "application/json")
        .body(body);
    if let Some(pw) = password {
        req = req.header(RELAY_PASSWORD_HEADER, pw);
    }
    let resp = req.send().await.expect("post");
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.unwrap_or_default();
    let error = (status >= 300).then(|| {
        serde_json::from_slice::<PushErrorBody>(&bytes)
            .unwrap_or_else(|e| panic!("non-2xx body must be PushErrorBody ({e}): {bytes:?}"))
    });
    (status, error)
}

async fn post(base: &str, request: &PushRequest) -> (u16, Option<PushErrorBody>) {
    post_raw(base, serde_json::to_vec(request).unwrap(), None).await
}

fn assert_error(result: (u16, Option<PushErrorBody>), status: u16, code: RelayErrorCode) {
    assert_eq!(result.0, status, "status; body {:?}", result.1);
    assert_eq!(result.1.expect("error body").code, code);
}

#[tokio::test]
async fn https_valid_signed_request_is_accepted_and_sends_one_alert() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (desktop, _phone, pairing) = paired(&base, true).await;
    let push = push_body(&pairing, PushClass::NeedsInput);
    let request = desktop.signed_push_request(now_ms(), push.clone());
    // The HTTPS path needs no live desktop WebSocket.
    desktop.close().await;

    let (status, error) = post(&base, &request).await;
    assert_eq!(status, 202, "body {error:?}");
    let sent = apns.wait_for(1).await;
    assert_encrypted_alert(&sent[0], &push, "An agent needs your input");
    apns.expect_no_more(1, 100).await;
}

#[tokio::test]
async fn https_no_token_is_still_202_and_sends_nothing() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (desktop, _phone, pairing) = paired(&base, false).await;
    let request = desktop.signed_push_request(now_ms(), push_body(&pairing, PushClass::Done));
    let (status, _) = post(&base, &request).await;
    assert_eq!(status, 202);
    apns.expect_no_more(0, 200).await;
}

#[tokio::test]
async fn https_bad_signature_is_401() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (desktop, _phone, pairing) = paired(&base, true).await;

    // Signed, then tampered: the class changed after signing.
    let mut request = desktop.signed_push_request(now_ms(), push_body(&pairing, PushClass::Done));
    request.push.class = PushClass::NeedsInput;
    assert_error(post(&base, &request).await, 401, RelayErrorCode::AuthFailed);

    // Signed by a different key entirely.
    let other = p256::ecdsa::SigningKey::random(&mut rand_core::OsRng);
    let forged = support::sign_push_request(
        &other,
        &DeviceId::new("dev_mac"),
        now_ms(),
        push_body(&pairing, PushClass::Done),
    );
    assert_error(post(&base, &forged).await, 401, RelayErrorCode::AuthFailed);
    apns.expect_no_more(0, 100).await;
}

#[tokio::test]
async fn https_stale_or_future_timestamp_is_401() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (desktop, _phone, pairing) = paired(&base, true).await;

    let stale = desktop.signed_push_request(
        now_ms() - 301_000,
        push_body(&pairing, PushClass::NeedsInput),
    );
    assert_error(post(&base, &stale).await, 401, RelayErrorCode::AuthFailed);
    let future = desktop.signed_push_request(
        now_ms() + 301_000,
        push_body(&pairing, PushClass::NeedsInput),
    );
    assert_error(post(&base, &future).await, 401, RelayErrorCode::AuthFailed);
    apns.expect_no_more(0, 100).await;
}

#[tokio::test]
async fn https_unknown_device_is_401() {
    let (base, _apns) = spawn_app_with_fake_apns(|_| {}).await;
    let key = p256::ecdsa::SigningKey::random(&mut rand_core::OsRng);
    let request = support::sign_push_request(
        &key,
        &DeviceId::new("dev_never_registered"),
        now_ms(),
        push_body(&PairingId::new("pair_x"), PushClass::Done),
    );
    assert_error(post(&base, &request).await, 401, RelayErrorCode::AuthFailed);
}

#[tokio::test]
async fn https_non_member_desktop_is_404() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (_desktop, _phone, pairing) = paired(&base, true).await;
    let mut intruder = TestClient::connect(&base, Role::Desktop, "dev_intruder").await;
    intruder.offer_pairing().await; // registers its key
    let request =
        intruder.signed_push_request(now_ms(), push_body(&pairing, PushClass::NeedsInput));
    assert_error(
        post(&base, &request).await,
        404,
        RelayErrorCode::UnknownPairing,
    );
    apns.expect_no_more(0, 100).await;
}

#[tokio::test]
async fn https_phone_device_is_400() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (_desktop, phone, pairing) = paired(&base, true).await;
    let request = phone.signed_push_request(now_ms(), push_body(&pairing, PushClass::NeedsInput));
    assert_error(post(&base, &request).await, 400, RelayErrorCode::BadFrame);
    apns.expect_no_more(0, 100).await;
}

#[tokio::test]
async fn https_malformed_json_is_400() {
    let (base, _apns) = spawn_app_with_fake_apns(|_| {}).await;
    assert_error(
        post_raw(&base, b"{\"device_id\":".to_vec(), None).await,
        400,
        RelayErrorCode::BadFrame,
    );
    assert_error(
        post_raw(&base, b"{\"device_id\":\"d\"}".to_vec(), None).await,
        400,
        RelayErrorCode::BadFrame,
    );
}

#[tokio::test]
async fn https_oversized_is_413() {
    let (base, apns) = spawn_app_with_fake_apns(|_| {}).await;
    let (desktop, _phone, pairing) = paired(&base, true).await;

    // A validly signed request whose ciphertext is over the 3000-char cap.
    let mut push = push_body(&pairing, PushClass::NeedsInput);
    push.ciphertext = "A".repeat(3001);
    let request = desktop.signed_push_request(now_ms(), push);
    assert_error(post(&base, &request).await, 413, RelayErrorCode::BadFrame);

    // A request body far past any valid push is refused before parsing.
    assert_error(
        post_raw(&base, vec![b' '; 64 * 1024], None).await,
        413,
        RelayErrorCode::BadFrame,
    );
    apns.expect_no_more(0, 100).await;
}

#[tokio::test]
async fn https_relay_password_is_enforced_when_configured() {
    let (base, apns, state) = spawn_app_with_fake_apns_state(|c| {
        c.relay_password = Some("correct horse".into());
    })
    .await;
    // The WebSocket test client does not present a password, so seed a paired
    // desktop + phone with a push token straight into the store.
    let key = p256::ecdsa::SigningKey::random(&mut rand_core::OsRng);
    let desktop = DeviceId::new("dev_mac");
    state
        .store
        .register_device(
            desktop.clone(),
            support::b64(key.verifying_key().to_encoded_point(false).as_bytes()),
        )
        .await;
    let pairing = state.store.create_pairing(desktop.clone()).await;
    state
        .store
        .add_phone_to_pairing(&pairing, DeviceId::new("dev_phone"))
        .await
        .unwrap();
    state
        .store
        .register_push_token(
            pairing.clone(),
            "apns-device-token".into(),
            ApnsEnvironment::Sandbox,
        )
        .await;

    let request = support::sign_push_request(
        &key,
        &desktop,
        now_ms(),
        push_body(&pairing, PushClass::Done),
    );
    let body = serde_json::to_vec(&request).unwrap();

    let missing = post_raw(&base, body.clone(), None).await;
    assert_error(missing, 401, RelayErrorCode::AuthFailed);
    let wrong = post_raw(&base, body.clone(), Some("battery staple")).await;
    assert_error(wrong, 401, RelayErrorCode::AuthFailed);
    apns.expect_no_more(0, 100).await;

    let (status, error) = post_raw(&base, body, Some("correct horse")).await;
    assert_eq!(status, 202, "body {error:?}");
    apns.wait_for(1).await;
}

#[tokio::test]
async fn https_is_rate_limited_per_device() {
    let (base, apns) = spawn_app_with_fake_apns(|c| c.push_rate_limit_per_minute = 2).await;
    let (desktop, _phone, pairing) = paired(&base, true).await;
    for i in 0..2 {
        let mut push = push_body(&pairing, PushClass::Done);
        push.event_id = EventId::new(format!("evt-{i}"));
        let (status, _) = post(&base, &desktop.signed_push_request(now_ms(), push)).await;
        assert_eq!(status, 202);
    }
    let request = desktop.signed_push_request(now_ms(), push_body(&pairing, PushClass::Done));
    assert_error(
        post(&base, &request).await,
        429,
        RelayErrorCode::RateLimited,
    );
    apns.wait_for(2).await;
    apns.expect_no_more(2, 100).await;
}

// ── v4 pairing + hello fields ────────────────────────────────────────────

#[tokio::test]
async fn hello_ok_announces_full_relay_mode() {
    let (base, _apns) = spawn_app_with_fake_apns(|_| {}).await;
    match hello_probe(&base, None).await {
        RelayFrame::HelloOk { relay_mode, .. } => assert_eq!(relay_mode, Some(RelayMode::Full)),
        other => panic!("expected hello_ok, got {other:?}"),
    }
}

#[tokio::test]
async fn pairing_claimed_carries_direct_endpoint_to_phone_and_identity_key_to_desktop() {
    let (base, _apns) = spawn_app_with_fake_apns(|_| {}).await;
    let endpoint = DirectEndpoint {
        host: "ruuds-mbp.tail1234.ts.net".into(),
        port: 47800,
    };
    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing_direct(endpoint.clone()).await;
    desktop.authenticate(vec![pairing.clone()]).await;

    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    let phone_identity_key = phone.public_key_b64.clone();
    match phone.claim_pairing_frame(&token).await {
        RelayFrame::PairingClaimed {
            pairing_id,
            direct_endpoint,
            peer_device_public_key,
            ..
        } => {
            assert_eq!(pairing_id, pairing);
            assert_eq!(direct_endpoint, Some(endpoint));
            assert_eq!(peer_device_public_key, None, "phone-bound copy omits it");
        }
        other => panic!("expected pairing_claimed, got {other:?}"),
    }
    match desktop
        .recv_until(|f| matches!(f, RelayFrame::PairingClaimed { .. }))
        .await
    {
        RelayFrame::PairingClaimed {
            direct_endpoint,
            peer_device_public_key,
            ..
        } => {
            assert_eq!(direct_endpoint, None, "desktop-bound copy omits it");
            assert_eq!(peer_device_public_key, Some(phone_identity_key));
        }
        other => panic!("expected pairing_claimed, got {other:?}"),
    }
}

#[tokio::test]
async fn relay_mode_pairing_has_no_direct_endpoint_but_still_the_identity_key() {
    let (base, _apns) = spawn_app_with_fake_apns(|_| {}).await;
    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing().await;
    desktop.authenticate(vec![pairing]).await;

    let mut phone = TestClient::connect(&base, Role::Phone, "dev_phone").await;
    let phone_identity_key = phone.public_key_b64.clone();
    assert!(matches!(
        phone.claim_pairing_frame(&token).await,
        RelayFrame::PairingClaimed {
            direct_endpoint: None,
            ..
        }
    ));
    match desktop
        .recv_until(|f| matches!(f, RelayFrame::PairingClaimed { .. }))
        .await
    {
        RelayFrame::PairingClaimed {
            peer_device_public_key,
            ..
        } => assert_eq!(peer_device_public_key, Some(phone_identity_key)),
        other => panic!("expected pairing_claimed, got {other:?}"),
    }
}

#[tokio::test]
async fn malformed_direct_endpoint_is_refused() {
    let (base, _apns) = spawn_app_with_fake_apns(|_| {}).await;
    let mut desktop = TestClient::connect(&base, Role::Desktop, "dev_mac").await;
    desktop
        .send(RelayFrame::PairingOffer {
            device_id: desktop.device_id.clone(),
            device_public_key: desktop.public_key_b64.clone(),
            key_agreement_public_key: desktop.key_agreement_public_key_b64.clone(),
            role: Role::Desktop,
            claim_token_hint: None,
            direct_endpoint: Some(DirectEndpoint {
                host: String::new(),
                port: 47800,
            }),
        })
        .await;
    assert!(matches!(
        desktop.recv().await,
        RelayFrame::Error {
            code: RelayErrorCode::BadFrame,
            ..
        }
    ));
    desktop.expect_closed(2000).await;
}
