//! Push-only relay mode (spec §5.9, remote-control-ck44), driven end to end
//! through the real relay:
//!
//! - `hello_ok.relay_mode` and `/version` report the configured mode;
//! - the control plane still works: pairing offer/claim (incl. the v4 direct
//!   fields), auth, presence, machine name, push-token register/unregister,
//!   revoke, and `push_notify` over the WebSocket and `POST /v1/push`;
//! - the data plane is refused: `envelope`, `resume` and `ack` get an advisory
//!   `error { code: relay_mode_push_only, pairing_id }`, the socket stays open,
//!   and nothing reaches the peer;
//! - the store's queue methods are never called (a spy store counts them), so
//!   no sender queue / seq high-water mark is ever created;
//! - the same scenario in `full` mode does route and queue (the spy sees it),
//!   proving the gate is the mode and not the test harness;
//! - an invalid `RELAY_MODE` stops the real binary at startup.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use flightdeck_relay::{
    apns::ApnsPushService,
    build_with_state,
    claims::{ClaimError, RedeemedClaim},
    config::{Config, LogFormat},
    queue::{AppendOutcome, QueueError, ResumeOutcome},
    store::{InMemoryStore, PairingMembers, RelayStore, RevokeOutcome, StoreError},
    AppState,
};
use flightdeck_remote_protocol::relay::PUSH_HTTP_PATH;
use flightdeck_remote_protocol::{
    ApnsEnvironment, DeviceId, DirectEndpoint, EncryptedEnvelope, EventId, PairingId,
    PresenceState, PushClass, PushNotifyBody, RelayErrorCode, RelayFrame, RelayMode, Role,
};
use support::{hello_probe, now_ms, test_apns_config, FakeApns, TestClient};

// ── a store that counts queue calls ──────────────────────────────────────

/// Delegates everything to an [`InMemoryStore`] and counts every call to the
/// data-plane queue methods (`enqueue`, `reset_stream`, `resume`, `ack`,
/// `ack_cursor`). A push-only relay must leave the count at zero.
struct SpyStore {
    inner: InMemoryStore,
    queue_calls: AtomicUsize,
}

impl SpyStore {
    fn queue(&self) {
        self.queue_calls.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl RelayStore for SpyStore {
    async fn register_device(&self, device: DeviceId, public_key_b64: String) {
        self.inner.register_device(device, public_key_b64).await
    }
    async fn device_public_key(&self, device: &DeviceId) -> Option<String> {
        self.inner.device_public_key(device).await
    }
    async fn register_key_agreement_key(&self, device: DeviceId, key: String) {
        self.inner.register_key_agreement_key(device, key).await
    }
    async fn device_key_agreement_key(&self, device: &DeviceId) -> Option<String> {
        self.inner.device_key_agreement_key(device).await
    }
    async fn create_pairing(&self, desktop: DeviceId) -> PairingId {
        self.inner.create_pairing(desktop).await
    }
    async fn add_phone_to_pairing(
        &self,
        pairing: &PairingId,
        phone: DeviceId,
    ) -> Result<DeviceId, StoreError> {
        self.inner.add_phone_to_pairing(pairing, phone).await
    }
    async fn pairing_members(&self, pairing: &PairingId) -> Option<PairingMembers> {
        self.inner.pairing_members(pairing).await
    }
    async fn set_machine_name(&self, pairing: &PairingId, machine_name: String) {
        self.inner.set_machine_name(pairing, machine_name).await
    }
    async fn machine_name(&self, pairing: &PairingId) -> Option<String> {
        self.inner.machine_name(pairing).await
    }
    async fn revoke_pairing(&self, pairing: &PairingId, requester: &DeviceId) -> RevokeOutcome {
        self.inner.revoke_pairing(pairing, requester).await
    }
    async fn issue_claim(
        &self,
        token: String,
        pairing: PairingId,
        desktop: DeviceId,
        expires_at_ms: i64,
        direct_endpoint: Option<DirectEndpoint>,
    ) {
        self.inner
            .issue_claim(token, pairing, desktop, expires_at_ms, direct_endpoint)
            .await
    }
    async fn claim_token_is_free(&self, token: &str, now_ms: i64) -> bool {
        self.inner.claim_token_is_free(token, now_ms).await
    }
    async fn redeem_claim(&self, token: &str, now_ms: i64) -> Result<RedeemedClaim, ClaimError> {
        self.inner.redeem_claim(token, now_ms).await
    }
    async fn sweep_expired_claims(&self, now_ms: i64) -> usize {
        self.inner.sweep_expired_claims(now_ms).await
    }
    async fn enqueue(&self, env: EncryptedEnvelope) -> Result<AppendOutcome, QueueError> {
        self.queue();
        self.inner.enqueue(env).await
    }
    async fn reset_stream(&self, pairing: &PairingId, sender: Role, next_seq: u64) {
        self.queue();
        self.inner.reset_stream(pairing, sender, next_seq).await
    }
    async fn resume(&self, pairing: &PairingId, sender: Role, from_seq: u64) -> ResumeOutcome {
        self.queue();
        self.inner.resume(pairing, sender, from_seq).await
    }
    async fn ack(&self, pairing: &PairingId, sender: Role, cursor: u64) {
        self.queue();
        self.inner.ack(pairing, sender, cursor).await
    }
    async fn ack_cursor(&self, pairing: &PairingId, sender: Role) -> u64 {
        self.queue();
        self.inner.ack_cursor(pairing, sender).await
    }
    async fn register_push_token(&self, pairing: PairingId, token: String, env: ApnsEnvironment) {
        self.inner.register_push_token(pairing, token, env).await
    }
    async fn unregister_push_token(&self, pairing: &PairingId) {
        self.inner.unregister_push_token(pairing).await
    }
    async fn push_token(&self, pairing: &PairingId) -> Option<(String, ApnsEnvironment)> {
        self.inner.push_token(pairing).await
    }
}

// ── harness ──────────────────────────────────────────────────────────────

struct Relay {
    base: String,
    apns: Arc<FakeApns>,
    spy: Arc<SpyStore>,
}

impl Relay {
    fn queue_calls(&self) -> usize {
        self.spy.queue_calls.load(Ordering::SeqCst)
    }
}

/// Spawn the real relay in `mode`, with the spy store and the live APNs push
/// service over a fake transport (store wired as the purge hook, as in prod).
async fn spawn(mode: RelayMode) -> Relay {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");

    let mut config = Config::new(addr.port(), LogFormat::Pretty, "test-sha");
    config.relay_mode = mode;
    let mut state = AppState::new(config);
    let spy = Arc::new(SpyStore {
        inner: InMemoryStore::new(1000),
        queue_calls: AtomicUsize::new(0),
    });
    let store: Arc<dyn RelayStore> = spy.clone();
    state.store = store.clone();
    let apns = Arc::new(FakeApns::default());
    state.push = Arc::new(
        ApnsPushService::new(test_apns_config(), apns.clone())
            .with_retry(3, Duration::ZERO)
            .with_purge(store),
    );
    let (router, _shutdown) = build_with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    Relay {
        base: format!("http://{addr}"),
        apns,
        spy,
    }
}

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

fn envelope(pairing: &PairingId, sender: Role, seq: u64) -> EncryptedEnvelope {
    EncryptedEnvelope {
        pairing_id: pairing.clone(),
        seq,
        sender,
        sent_at_ms: 0,
        nonce: "bm9uY2U=".into(),
        ciphertext: "opaque".into(),
    }
}

fn is_envelope(frame: &RelayFrame) -> bool {
    matches!(frame, RelayFrame::Envelope(_))
}

fn is_ack(frame: &RelayFrame) -> bool {
    matches!(frame, RelayFrame::Ack { .. })
}

fn is_data_plane(frame: &RelayFrame) -> bool {
    is_envelope(frame) || is_ack(frame)
}

/// Assert `frame` is the spec §5.9 refusal for `pairing`.
fn assert_push_only_refusal(frame: RelayFrame, pairing: &PairingId) {
    match frame {
        RelayFrame::Error {
            code,
            message,
            pairing_id,
            expected_seq,
        } => {
            assert_eq!(code, RelayErrorCode::RelayModePushOnly);
            assert_eq!(pairing_id.as_ref(), Some(pairing));
            assert_eq!(expected_seq, None);
            assert!(message.contains("push_only"), "message: {message}");
        }
        other => panic!("expected relay_mode_push_only error, got {other:?}"),
    }
}

/// The next `error` frame on `client` (skipping unrelated traffic).
async fn next_error(client: &mut TestClient) -> RelayFrame {
    client
        .recv_until(|f| matches!(f, RelayFrame::Error { .. }))
        .await
}

/// The connection is still open and serving: a ping gets its pong.
async fn assert_still_open(client: &mut TestClient) {
    client.send(RelayFrame::Ping { client_time_ms: 42 }).await;
    match client
        .recv_until(|f| matches!(f, RelayFrame::Pong { .. }))
        .await
    {
        RelayFrame::Pong { client_time_ms, .. } => assert_eq!(client_time_ms, 42),
        other => panic!("expected pong, got {other:?}"),
    }
}

/// A direct-mode pairing made through `relay`, both legs authenticated, the
/// presence exchange drained. Returns `(desktop, phone, pairing)`.
async fn paired(relay: &Relay, with_token: bool) -> (TestClient, TestClient, PairingId) {
    let mut desktop = TestClient::connect(&relay.base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop
        .offer_pairing_direct(DirectEndpoint {
            host: "ruuds-mbp.tail1234.ts.net".into(),
            port: 47800,
        })
        .await;
    desktop
        .authenticate_named(vec![pairing.clone()], Some("Ruud's MacBook Pro"))
        .await;

    let mut phone = TestClient::connect(&relay.base, Role::Phone, "dev_phone").await;
    phone.claim_pairing(&token).await;
    desktop
        .recv_until(|f| matches!(f, RelayFrame::PairingClaimed { .. }))
        .await;
    phone.authenticate(vec![pairing.clone()]).await;
    desktop
        .recv_until(|f| matches!(f, RelayFrame::PeerPresence { .. }))
        .await;
    phone
        .recv_until(|f| matches!(f, RelayFrame::MachineName { .. }))
        .await;
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

// ── mode announcement ────────────────────────────────────────────────────

#[tokio::test]
async fn hello_ok_announces_push_only() {
    let relay = spawn(RelayMode::PushOnly).await;
    match hello_probe(&relay.base, None).await {
        RelayFrame::HelloOk { relay_mode, .. } => {
            assert_eq!(relay_mode, Some(RelayMode::PushOnly))
        }
        other => panic!("expected hello_ok, got {other:?}"),
    }
}

#[tokio::test]
async fn version_endpoint_reports_the_mode() {
    for mode in [RelayMode::Full, RelayMode::PushOnly] {
        let relay = spawn(mode).await;
        let body: serde_json::Value = reqwest::get(format!("{}/version", relay.base))
            .await
            .expect("request")
            .json()
            .await
            .expect("json");
        assert_eq!(body["relay_mode"], mode.as_str());
        assert_eq!(body["git_sha"], "test-sha");
        // The probes stay plain `ok` regardless of mode.
        let health = reqwest::get(format!("{}/healthz", relay.base))
            .await
            .expect("request");
        assert_eq!(health.text().await.unwrap(), "ok");
    }
}

// ── control plane still served ───────────────────────────────────────────

#[tokio::test]
async fn pairing_bootstrap_auth_presence_and_machine_name_work() {
    let relay = spawn(RelayMode::PushOnly).await;
    let endpoint = DirectEndpoint {
        host: "ruuds-mbp.tail1234.ts.net".into(),
        port: 47800,
    };

    let mut desktop = TestClient::connect(&relay.base, Role::Desktop, "dev_mac").await;
    let (pairing, token) = desktop.offer_pairing_direct(endpoint.clone()).await;
    let activated = desktop
        .authenticate_named(vec![pairing.clone()], Some("Ruud's MacBook Pro"))
        .await;
    assert_eq!(activated, vec![pairing.clone()]);

    let mut phone = TestClient::connect(&relay.base, Role::Phone, "dev_phone").await;
    let phone_identity = phone.public_key_b64.clone();
    match phone.claim_pairing_frame(&token).await {
        RelayFrame::PairingClaimed {
            pairing_id,
            peer_device_id,
            peer_key_agreement_public_key,
            direct_endpoint,
            ..
        } => {
            assert_eq!(pairing_id, pairing);
            assert_eq!(peer_device_id, Some(DeviceId::new("dev_mac")));
            assert_eq!(
                peer_key_agreement_public_key,
                Some(desktop.key_agreement_public_key_b64.clone())
            );
            assert_eq!(direct_endpoint, Some(endpoint));
        }
        other => panic!("expected pairing_claimed, got {other:?}"),
    }
    match desktop
        .recv_until(|f| matches!(f, RelayFrame::PairingClaimed { .. }))
        .await
    {
        RelayFrame::PairingClaimed {
            peer_device_public_key,
            ..
        } => assert_eq!(peer_device_public_key, Some(phone_identity)),
        other => panic!("expected pairing_claimed, got {other:?}"),
    }

    assert_eq!(
        phone.authenticate(vec![pairing.clone()]).await,
        vec![pairing.clone()]
    );
    // Presence both ways, and the Mac's name reaches the phone.
    for client in [&mut desktop, &mut phone] {
        match client
            .recv_until(|f| matches!(f, RelayFrame::PeerPresence { .. }))
            .await
        {
            RelayFrame::PeerPresence { state, .. } => {
                assert_eq!(state, PresenceState::Connected)
            }
            other => panic!("expected peer_presence, got {other:?}"),
        }
    }
    match phone
        .recv_until(|f| matches!(f, RelayFrame::MachineName { .. }))
        .await
    {
        RelayFrame::MachineName { machine_name, .. } => {
            assert_eq!(machine_name, "Ruud's MacBook Pro")
        }
        other => panic!("expected machine_name, got {other:?}"),
    }

    // No auth-time ack cursor: a push-only relay forwards no acks, so it must
    // not advertise that it does (spec §5.9).
    desktop.expect_no_frame_matching(200, is_ack).await;
    phone.expect_no_frame_matching(200, is_ack).await;

    // A disconnect is still announced to the peer.
    phone.close().await;
    match desktop
        .recv_until(|f| matches!(f, RelayFrame::PeerPresence { .. }))
        .await
    {
        RelayFrame::PeerPresence { state, .. } => {
            assert_eq!(state, PresenceState::Disconnected)
        }
        other => panic!("expected peer_presence, got {other:?}"),
    }
    assert_eq!(relay.queue_calls(), 0, "no queue method may be called");
}

#[tokio::test]
async fn push_notify_over_websocket_sends_one_alert() {
    let relay = spawn(RelayMode::PushOnly).await;
    let (mut desktop, _phone, pairing) = paired(&relay, true).await;

    desktop
        .push_notify(push_body(&pairing, PushClass::NeedsInput))
        .await;
    let sent = relay.apns.wait_for(1).await;
    assert_eq!(sent[0].path, "/3/device/apns-device-token");
    let headers: std::collections::HashMap<_, _> = sent[0].headers.iter().cloned().collect();
    assert_eq!(headers["apns-push-type"], "alert");
    assert_eq!(headers["apns-collapse-id"], EVENT_ID);
    desktop
        .expect_no_frame_matching(200, |f| matches!(f, RelayFrame::Error { .. }))
        .await;
    relay.apns.expect_no_more(1, 100).await;
    assert_eq!(relay.queue_calls(), 0);
}

#[tokio::test]
async fn push_notify_over_https_is_accepted_without_a_desktop_websocket() {
    let relay = spawn(RelayMode::PushOnly).await;
    let (desktop, _phone, pairing) = paired(&relay, true).await;
    let request = desktop.signed_push_request(now_ms(), push_body(&pairing, PushClass::Done));
    // The push-only deployment's normal shape: no persistent desktop WS.
    desktop.close().await;

    let resp = reqwest::Client::new()
        .post(format!("{}{PUSH_HTTP_PATH}", relay.base))
        .json(&request)
        .send()
        .await
        .expect("post");
    assert_eq!(resp.status().as_u16(), 202);
    let sent = relay.apns.wait_for(1).await;
    let body: serde_json::Value = serde_json::from_slice(&sent[0].body).unwrap();
    assert_eq!(body["fd_push"]["class"], "done");
    assert_eq!(body["fd_push"]["pairing_id"], pairing.as_str());
    relay.apns.expect_no_more(1, 100).await;
    assert_eq!(relay.queue_calls(), 0);
}

#[tokio::test]
async fn push_token_register_and_unregister_are_served() {
    let relay = spawn(RelayMode::PushOnly).await;
    let (mut desktop, mut phone, pairing) = paired(&relay, true).await;

    // Registered (by `paired`): a push goes out.
    desktop
        .push_notify(push_body(&pairing, PushClass::Error))
        .await;
    relay.apns.wait_for(1).await;

    // Unregister: acked, and the next push sends nothing.
    phone.unregister_push_token(&pairing).await;
    match phone
        .recv_until(|f| matches!(f, RelayFrame::PushTokenAck { .. }))
        .await
    {
        RelayFrame::PushTokenAck { pairing_id } => assert_eq!(pairing_id, pairing),
        other => panic!("expected push_token_ack, got {other:?}"),
    }
    desktop
        .push_notify(push_body(&pairing, PushClass::Error))
        .await;
    relay.apns.expect_no_more(1, 300).await;

    // Re-register: pushes resume.
    phone
        .register_push_token(&pairing, "apns-device-token", ApnsEnvironment::Sandbox)
        .await;
    phone
        .recv_until(|f| matches!(f, RelayFrame::PushTokenAck { .. }))
        .await;
    desktop
        .push_notify(push_body(&pairing, PushClass::Done))
        .await;
    relay.apns.wait_for(2).await;
    assert_eq!(relay.queue_calls(), 0);
}

#[tokio::test]
async fn revoke_notifies_both_ends_and_stops_pushes() {
    let relay = spawn(RelayMode::PushOnly).await;
    let (mut desktop, mut phone, pairing) = paired(&relay, true).await;

    phone.revoke(&pairing).await;
    for client in [&mut phone, &mut desktop] {
        match client
            .recv_until(|f| matches!(f, RelayFrame::PairingRevoked { .. }))
            .await
        {
            RelayFrame::PairingRevoked { pairing_id } => assert_eq!(pairing_id, pairing),
            other => panic!("expected pairing_revoked, got {other:?}"),
        }
    }

    // The desktop is no longer a member: its push is refused, nothing sent.
    desktop
        .push_notify(push_body(&pairing, PushClass::Done))
        .await;
    match desktop
        .recv_until(|f| matches!(f, RelayFrame::Error { .. }))
        .await
    {
        RelayFrame::Error { code, .. } => assert_eq!(code, RelayErrorCode::UnknownPairing),
        other => panic!("expected error, got {other:?}"),
    }
    relay.apns.expect_no_more(0, 100).await;
    assert_eq!(relay.queue_calls(), 0);
}

// ── data plane refused ───────────────────────────────────────────────────

#[tokio::test]
async fn envelopes_are_refused_both_ways_and_never_forwarded() {
    let relay = spawn(RelayMode::PushOnly).await;
    let (mut desktop, mut phone, pairing) = paired(&relay, false).await;

    desktop
        .send(RelayFrame::Envelope(envelope(&pairing, Role::Desktop, 1)))
        .await;
    assert_push_only_refusal(next_error(&mut desktop).await, &pairing);
    phone
        .send(RelayFrame::Envelope(envelope(&pairing, Role::Phone, 1)))
        .await;
    assert_push_only_refusal(next_error(&mut phone).await, &pairing);

    // Neither peer received the other's envelope (or any ack).
    phone.expect_no_frame_matching(300, is_data_plane).await;
    desktop.expect_no_frame_matching(300, is_data_plane).await;

    // Advisory, not fatal: both sockets keep serving the control plane.
    assert_still_open(&mut desktop).await;
    assert_still_open(&mut phone).await;
    assert_eq!(relay.queue_calls(), 0, "no envelope was queued");
}

#[tokio::test]
async fn resume_and_ack_are_refused_and_acks_are_not_forwarded() {
    let relay = spawn(RelayMode::PushOnly).await;
    let (mut desktop, mut phone, pairing) = paired(&relay, false).await;

    phone
        .send(RelayFrame::Resume {
            pairing_id: pairing.clone(),
            from_seq: 0,
        })
        .await;
    assert_push_only_refusal(next_error(&mut phone).await, &pairing);

    phone
        .send(RelayFrame::Ack {
            pairing_id: pairing.clone(),
            cursor: 7,
        })
        .await;
    assert_push_only_refusal(next_error(&mut phone).await, &pairing);
    desktop.expect_no_frame_matching(300, is_ack).await;

    desktop
        .send(RelayFrame::Resume {
            pairing_id: pairing.clone(),
            from_seq: 3,
        })
        .await;
    assert_push_only_refusal(next_error(&mut desktop).await, &pairing);

    assert_still_open(&mut phone).await;
    assert_still_open(&mut desktop).await;
    assert_eq!(relay.queue_calls(), 0);
}

#[tokio::test]
async fn refusal_does_not_depend_on_pairing_membership() {
    // The mode is the reason: even an envelope for a pairing this connection
    // never activated gets `relay_mode_push_only`, not `unknown_pairing`.
    let relay = spawn(RelayMode::PushOnly).await;
    let (mut desktop, _phone, _pairing) = paired(&relay, false).await;
    let stranger = PairingId::new("pair_not_mine");
    desktop
        .send(RelayFrame::Envelope(envelope(&stranger, Role::Desktop, 1)))
        .await;
    assert_push_only_refusal(next_error(&mut desktop).await, &stranger);
    assert_eq!(relay.queue_calls(), 0);
}

#[tokio::test]
async fn data_plane_before_auth_is_still_not_authenticated() {
    // The pre-auth rule is unchanged by the mode: wrong-phase frames close.
    let relay = spawn(RelayMode::PushOnly).await;
    let mut desktop = TestClient::connect(&relay.base, Role::Desktop, "dev_mac").await;
    let pairing = PairingId::new("pair_x");
    desktop
        .send(RelayFrame::Envelope(envelope(&pairing, Role::Desktop, 1)))
        .await;
    match desktop.recv().await {
        RelayFrame::Error { code, .. } => assert_eq!(code, RelayErrorCode::NotAuthenticated),
        other => panic!("expected not_authenticated, got {other:?}"),
    }
    desktop.expect_closed(2000).await;
    assert_eq!(relay.queue_calls(), 0);
}

// ── full mode, same scenario ─────────────────────────────────────────────

#[tokio::test]
async fn full_mode_routes_and_queues_the_same_traffic() {
    let relay = spawn(RelayMode::Full).await;
    match hello_probe(&relay.base, None).await {
        RelayFrame::HelloOk { relay_mode, .. } => assert_eq!(relay_mode, Some(RelayMode::Full)),
        other => panic!("expected hello_ok, got {other:?}"),
    }
    let (mut desktop, mut phone, pairing) = paired(&relay, false).await;

    desktop
        .send(RelayFrame::Envelope(envelope(&pairing, Role::Desktop, 1)))
        .await;
    match phone.recv_until(is_envelope).await {
        RelayFrame::Envelope(env) => assert_eq!(env.seq, 1),
        other => panic!("expected envelope, got {other:?}"),
    }
    phone
        .send(RelayFrame::Ack {
            pairing_id: pairing.clone(),
            cursor: 1,
        })
        .await;
    match desktop
        .recv_until(|f| matches!(f, RelayFrame::Ack { cursor: 1, .. }))
        .await
    {
        RelayFrame::Ack { pairing_id, .. } => assert_eq!(pairing_id, pairing),
        other => panic!("expected forwarded ack, got {other:?}"),
    }
    desktop
        .expect_no_frame_matching(200, |f| {
            matches!(
                f,
                RelayFrame::Error {
                    code: RelayErrorCode::RelayModePushOnly,
                    ..
                }
            )
        })
        .await;
    assert!(
        relay.queue_calls() > 0,
        "full mode uses the queue (proves the spy is wired)"
    );
}

// ── startup ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn invalid_relay_mode_stops_the_binary_at_startup() {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_flightdeck-relay"))
        .env("RELAY_MODE", "pushonly")
        .env("PORT", "0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn relay binary");
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("an invalid RELAY_MODE must exit, not start serving")
        .expect("wait");
    assert!(!status.success(), "exit status {status:?}");
    let mut stderr = String::new();
    use tokio::io::AsyncReadExt as _;
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .await
        .expect("read stderr");
    assert!(
        stderr.contains("invalid RELAY_MODE") && stderr.contains("push_only"),
        "stderr names the variable and the accepted values: {stderr}"
    );
}
