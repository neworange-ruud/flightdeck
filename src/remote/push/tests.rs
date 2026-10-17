use super::*;

use std::sync::Mutex;

use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};

use flightdeck_remote_protocol::{DeepLink, EventId, ProjectId, PushClass, SessionId};

use crate::remote::notifier::{build_push_intent, EventContext};
use flightdeck_remote_protocol::Role;

// --- fixtures ---------------------------------------------------------------

/// A desktop/phone channel pair for `pairing`, as both ends derive it.
fn channel_pair(pairing: &str) -> (E2eChannel, E2eChannel) {
    let desktop = DeviceIdentity::generate();
    let phone = DeviceIdentity::generate();
    let d = E2eChannel::derive(
        &desktop.private_key_bytes(),
        phone.public_key_x963(),
        pairing,
        b"4729",
        Role::Desktop,
    )
    .unwrap();
    let p = E2eChannel::derive(
        &phone.private_key_bytes(),
        desktop.public_key_x963(),
        pairing,
        b"4729",
        Role::Phone,
    )
    .unwrap();
    (d, p)
}

fn intent(class: PushClass, preview: Option<&str>) -> PushIntent {
    build_push_intent(
        class,
        EventContext {
            event_id: EventId::new("6f9a1c2e-3b4d-4e5f-8a7b-1c2d3e4f5a6b"),
            deep_link: DeepLink {
                project_id: ProjectId::new("proj"),
                session_id: SessionId::new("t1"),
                item_id: None,
            },
            occurred_at_ms: 1_752_412_800_000,
            session_name: "fd-api".to_string(),
            preview: preview.map(str::to_string),
            files_changed: 0,
            ready_to_push: false,
            error_message: None,
        },
        Some("FlightDeck".to_string()),
    )
}

fn body(pairing: &str) -> PushNotifyBody {
    let (desktop, _) = channel_pair(pairing);
    seal_push(&intent(PushClass::NeedsInput, Some("Proceed?")), &desktop).unwrap()
}

/// Records every HTTPS push instead of sending it.
#[derive(Clone, Default)]
struct RecordingPoster(Arc<Mutex<Vec<HttpsPush>>>);

impl PushPoster for RecordingPoster {
    fn post(&self, push: HttpsPush) {
        self.0.lock().unwrap().push(push);
    }
}

fn verify(identity_pub: &[u8], request: &PushRequest) -> bool {
    let vk = VerifyingKey::from_sec1_bytes(identity_pub).unwrap();
    let raw = STANDARD.decode(&request.signature).unwrap();
    let sig = Signature::from_slice(&raw).unwrap();
    vk.verify(request.signing_message().as_bytes(), &sig)
        .is_ok()
}

// --- the transport rule -----------------------------------------------------

#[test]
fn the_socket_carries_pushes_only_when_live_at_v4_or_later() {
    assert_eq!(choose_route(None), PushRoute::Https, "not connected");
    assert_eq!(choose_route(Some(1)), PushRoute::Https, "an old relay");
    assert_eq!(choose_route(Some(3)), PushRoute::Https, "an old relay");
    assert_eq!(
        choose_route(Some(PUSH_NOTIFY_MIN_VERSION)),
        PushRoute::RelaySocket
    );
    assert_eq!(choose_route(Some(9)), PushRoute::RelaySocket);
}

#[test]
fn the_push_link_tracks_the_session() {
    let link = PushLink::new();
    assert_eq!(link.live_version(), None, "starts down");
    let reader = link.clone();
    link.set_live(4);
    assert_eq!(reader.live_version(), Some(4), "clones share the state");
    link.set_down();
    assert_eq!(reader.live_version(), None);
}

#[test]
fn the_push_link_is_fresh_only_while_relay_frames_keep_arriving() {
    let link = PushLink::new();
    assert_eq!(link.fresh_version(Duration::from_secs(60)), None, "down");
    link.set_live(4);
    assert_eq!(
        link.fresh_version(Duration::from_secs(60)),
        Some(4),
        "auth_ok is itself an inbound frame"
    );
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(
        link.fresh_version(Duration::from_millis(10)),
        None,
        "silent past the window: maybe half-open"
    );
    assert_eq!(link.live_version(), Some(4), "still live, just not trusted");
    link.mark_inbound();
    assert_eq!(link.fresh_version(Duration::from_millis(10)), Some(4));
}

/// A live v4 socket that has gone quiet is not handed the push: it goes over
/// HTTPS instead of into a possibly half-open socket.
#[test]
fn a_live_but_silent_socket_sends_the_push_over_https() {
    let link = PushLink::new();
    link.set_live(4);
    let poster = RecordingPoster::default();
    let (sender, _) = sender(&link, &poster, "wss://relay.example/ws");
    let sender = sender.with_max_silence(Duration::from_millis(10));
    std::thread::sleep(Duration::from_millis(30));
    let mut sent = Vec::new();
    let route = sender.dispatch(body("pair_1"), 1_000, &mut |o| sent.push(o));
    assert_eq!(route, PushRoute::Https);
    assert!(sent.is_empty());
    assert_eq!(poster.0.lock().unwrap().len(), 1);
}

/// The silence bound sits just above the client's ping interval, so a
/// healthy link (a pong per ping) is never mistaken for a silent one.
#[test]
fn the_silence_bound_clears_the_ping_interval() {
    use crate::remote::client::{LIVENESS_TIMEOUT, PING_INTERVAL};
    assert!(PUSH_WS_MAX_SILENCE > PING_INTERVAL);
    assert!(
        PUSH_WS_MAX_SILENCE < LIVENESS_TIMEOUT,
        "under the liveness teardown"
    );
}

// --- sealing ------------------------------------------------------------------

#[test]
fn a_sealed_push_opens_on_the_phone_to_the_intended_payload() {
    let (desktop, phone) = channel_pair("pair_ruud_mbp");
    let intent = intent(PushClass::NeedsInput, Some("Allow `rm -rf build`?"));
    let body = seal_push(&intent, &desktop).unwrap();
    assert_eq!(body.pairing_id.as_str(), "pair_ruud_mbp");
    assert_eq!(body.event_id, intent.event_id);
    assert_eq!(body.class, PushClass::NeedsInput);
    assert!(
        body.ciphertext.len() <= flightdeck_remote_protocol::relay::MAX_PUSH_CIPHERTEXT_B64_LEN
    );
    let plain = phone
        .open_push(&body.event_id, &body.nonce, &body.ciphertext)
        .unwrap();
    let payload: PushPayload = serde_json::from_slice(&plain).unwrap();
    assert_eq!(payload, intent.to_payload());
    assert_eq!(payload.title, "fd-api needs input");
    assert_eq!(payload.body, "Allow `rm -rf build`?");
    // Bound to its event: the same ciphertext under another id does not open.
    assert!(phone
        .open_push(&EventId::new("other"), &body.nonce, &body.ciphertext)
        .is_err());
}

#[test]
fn a_body_that_escapes_past_the_plaintext_cap_is_cut_until_it_fits() {
    // 1000 bytes of control characters serialize as `\u0001` each: ~6000
    // bytes of JSON, far over the 2048-byte cap.
    let (desktop, phone) = channel_pair("pair_1");
    let preview = "\u{1}".repeat(1000);
    let intent = intent(PushClass::NeedsInput, Some(&preview));
    assert_eq!(intent.body.len(), 1000);
    let body = seal_push(&intent, &desktop).expect("shrunk, not lost");
    let plain = phone
        .open_push(&body.event_id, &body.nonce, &body.ciphertext)
        .unwrap();
    assert!(plain.len() <= MAX_PUSH_PLAINTEXT_BYTES, "{}", plain.len());
    let payload: PushPayload = serde_json::from_slice(&plain).unwrap();
    assert!(payload.body.ends_with('…'));
    assert!(preview.starts_with(payload.body.trim_end_matches('…')));
    assert_eq!(payload.title, intent.title, "the title is untouched");
}

// --- signing ------------------------------------------------------------------

#[test]
fn an_https_push_request_is_signed_by_the_device_identity() {
    let identity = DeviceIdentity::generate();
    let push = body("pair_1");
    let request = sign_push_request(&identity, 1_752_412_800_500, push.clone());
    assert_eq!(request.device_id.as_str(), identity.device_id());
    assert_eq!(request.timestamp_ms, 1_752_412_800_500);
    assert_eq!(request.push, push);
    assert_eq!(STANDARD.decode(&request.signature).unwrap().len(), 64);
    assert!(verify(identity.public_key_x963(), &request));
    // The signature covers every field: change one and it no longer verifies.
    let mut tampered = request.clone();
    tampered.timestamp_ms += 1;
    assert!(!verify(identity.public_key_x963(), &tampered));
    let mut tampered = request.clone();
    tampered.push.class = PushClass::Done;
    assert!(!verify(identity.public_key_x963(), &tampered));
    // And only this device's key verifies it.
    let other = DeviceIdentity::generate();
    assert!(!verify(other.public_key_x963(), &request));
}

// --- dispatch -----------------------------------------------------------------

fn sender(link: &PushLink, poster: &RecordingPoster, relay_url: &str) -> (PushSender, Vec<u8>) {
    let identity = DeviceIdentity::generate();
    let public = identity.public_key_x963().to_vec();
    let sender = PushSender::with_poster(
        link.clone(),
        identity,
        relay_url,
        Some("s3cret".to_string()),
        Box::new(poster.clone()),
    );
    (sender, public)
}

#[test]
fn a_live_v4_socket_carries_the_push_as_a_frame() {
    let link = PushLink::new();
    link.set_live(4);
    let poster = RecordingPoster::default();
    let (sender, _) = sender(&link, &poster, "wss://relay.example/ws");
    let push = body("pair_1");
    let mut sent = Vec::new();
    let route = sender.dispatch(push.clone(), 1_000, &mut |o| sent.push(o));
    assert_eq!(route, PushRoute::RelaySocket);
    assert!(
        matches!(sent.as_slice(), [RemoteOutbound::PushNotify { push: b, .. }] if *b == push),
        "{sent:?}"
    );
    assert!(poster.0.lock().unwrap().is_empty(), "no HTTPS");
}

#[test]
fn without_a_live_socket_the_push_goes_over_signed_https() {
    for live in [None, Some(1), Some(3)] {
        let link = PushLink::new();
        if let Some(v) = live {
            link.set_live(v);
        }
        let poster = RecordingPoster::default();
        let (sender, public) = sender(&link, &poster, "wss://relay.example:8443/ws?x=1");
        let push = body("pair_1");
        let mut sent = Vec::new();
        let route = sender.dispatch(push.clone(), 1_752_412_800_500, &mut |o| sent.push(o));
        assert_eq!(route, PushRoute::Https, "live={live:?}");
        assert!(sent.is_empty(), "nothing on the socket (live={live:?})");
        let posted = poster.0.lock().unwrap();
        assert_eq!(posted.len(), 1);
        let p = &posted[0];
        assert_eq!(p.url, "https://relay.example:8443/v1/push");
        assert_eq!(p.relay_password.as_deref(), Some("s3cret"));
        assert_eq!(p.request.push, push);
        assert_eq!(p.request.timestamp_ms, 1_752_412_800_500);
        assert!(verify(&public, &p.request), "signed by the desktop");
    }
}

#[test]
fn a_relay_url_with_no_https_endpoint_drops_the_push() {
    let link = PushLink::new();
    let poster = RecordingPoster::default();
    let (sender, _) = sender(&link, &poster, "not-a-url");
    let mut sent = Vec::new();
    sender.dispatch(body("pair_1"), 0, &mut |o| sent.push(o));
    assert!(sent.is_empty());
    assert!(poster.0.lock().unwrap().is_empty());
}

// --- HTTP delivery against a local server -----------------------------------

/// One request the mock server saw: its head (request line + headers) and body.
#[derive(Debug, Clone)]
struct Seen {
    head: String,
    body: String,
}

/// A plain-HTTP server on loopback that answers successive requests with
/// `statuses` (one connection each) and records what it saw.
async fn mock_server(statuses: Vec<u16>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        for status in statuses {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head, body) = loop {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let head = text[..end].to_string();
                    let len = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len {
                        let body = String::from_utf8_lossy(&buf[end + 4..end + 4 + len]);
                        break (head, body.to_string());
                    }
                }
            };
            log.lock().unwrap().push(Seen { head, body });
            let reply =
                format!("HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
            let _ = sock.write_all(reply.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}/v1/push"), seen)
}

fn https_push(url: &str) -> HttpsPush {
    let identity = DeviceIdentity::generate();
    HttpsPush {
        url: url.to_string(),
        relay_password: Some("s3cret".to_string()),
        request: sign_push_request(&identity, 1_752_412_800_500, body("pair_1")),
    }
}

#[tokio::test]
async fn an_accepted_push_posts_the_signed_json_with_the_password_header() {
    let (url, seen) = mock_server(vec![202]).await;
    let push = https_push(&url);
    let outcome = deliver_with(&push, Duration::ZERO).await;
    assert_eq!(outcome, PushHttpOutcome::Accepted(202));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let head = seen[0].head.to_ascii_lowercase();
    assert!(head.starts_with("post /v1/push http/1.1"), "{head}");
    assert!(head.contains("content-type: application/json"), "{head}");
    assert!(
        head.contains("x-flightdeck-relay-password: s3cret"),
        "{head}"
    );
    let sent: PushRequest = serde_json::from_str(&seen[0].body).unwrap();
    assert_eq!(sent, push.request);
}

#[tokio::test]
async fn a_transient_failure_is_retried_exactly_once() {
    let (url, seen) = mock_server(vec![503, 202]).await;
    let outcome = deliver_with(&https_push(&url), Duration::ZERO).await;
    assert_eq!(outcome, PushHttpOutcome::Accepted(202));
    assert_eq!(seen.lock().unwrap().len(), 2);

    let (url, seen) = mock_server(vec![500, 502, 202]).await;
    let outcome = deliver_with(&https_push(&url), Duration::ZERO).await;
    assert!(
        matches!(outcome, PushHttpOutcome::Transient(_)),
        "{outcome:?}"
    );
    assert_eq!(seen.lock().unwrap().len(), 2, "one retry, not more");
}

#[tokio::test]
async fn a_refusal_is_not_retried() {
    // 404 is also what a pre-v4 relay (no push endpoint) answers.
    for status in [400, 401, 404, 413, 429] {
        let (url, seen) = mock_server(vec![status, 202]).await;
        let outcome = deliver_with(&https_push(&url), Duration::ZERO).await;
        assert_eq!(outcome, PushHttpOutcome::Rejected(status));
        assert_eq!(seen.lock().unwrap().len(), 1, "status {status}");
    }
}

#[tokio::test]
async fn an_unreachable_relay_is_transient_and_gives_up_after_the_retry() {
    // Bind then drop a listener so the port is closed.
    let addr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let outcome = deliver_with(
        &https_push(&format!("http://{addr}/v1/push")),
        Duration::ZERO,
    )
    .await;
    assert!(
        matches!(outcome, PushHttpOutcome::Transient(_)),
        "{outcome:?}"
    );
}
