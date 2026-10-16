//! Desktop-requested pushes (spec §5.5.1–§5.5.3).
//!
//! A desktop asks for exactly one visible alert per push-worthy event — an
//! agent needs input, hit an error, or is done — either with the `push_notify`
//! frame on its authenticated WebSocket ([`crate::session`]) or with
//! `POST /v1/push` over HTTPS ([`crate::handlers::push_http`]). Both transports
//! authenticate the device their own way (challenge-response vs a signed
//! request) and then hand the push to the one shared function here,
//! [`accept_push`], so the rules cannot drift between them:
//!
//! 1. the device must be the **desktop** member of `pairing_id` (a phone member
//!    is `bad_frame`; a non-member or unknown pairing is `unknown_pairing`);
//! 2. field limits: `event_id` ≤ [`MAX_EVENT_ID_BYTES`] (it becomes the APNs
//!    collapse id), a short `nonce`, a non-empty `ciphertext` of at most
//!    [`MAX_PUSH_CIPHERTEXT_B64_LEN`] characters (too large);
//! 3. a per-device rate limit ([`PushRateLimiter`]);
//! 4. look up the pairing's push token — none is a silent success;
//! 5. send the encrypted alert through [`crate::apns::PushService`] on a
//!    detached task, so neither the WebSocket read loop nor the HTTP response
//!    waits on APNs, and log one line with the outcome.
//!
//! **Zero-knowledge.** Nothing here opens `nonce`/`ciphertext`; logs carry the
//! pairing id, event id, class and outcome only — never content and never the
//! device token.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use flightdeck_remote_protocol::relay::MAX_PUSH_CIPHERTEXT_B64_LEN;
use flightdeck_remote_protocol::{DeviceId, PushNotifyBody, RelayErrorCode};

use crate::apns::PushOutcome;
use crate::AppState;

/// Largest accepted `event_id`, in bytes. It is sent as `apns-collapse-id`,
/// which APNs caps at 64 bytes (spec §5.5.1: a UUID string ≤ 64 bytes); a longer
/// one would be rejected by APNs on every attempt.
pub const MAX_EVENT_ID_BYTES: usize = 64;

/// Largest accepted base64 `nonce`, in characters. A 12-byte AEAD nonce is 16
/// base64 characters; the slack only keeps junk from inflating the APNs
/// payload — the relay never decodes it.
pub const MAX_PUSH_NONCE_B64_LEN: usize = 64;

/// Default per-desktop push budget per minute (both transports together). A
/// desktop pushes only on needs-input / error / settled-done, so a legitimate
/// one stays far below this; the cap only stops a runaway or compromised
/// client from spamming a phone through the relay.
pub const DEFAULT_PUSH_RATE_LIMIT_PER_MINUTE: u32 = 60;

/// Which transport a push arrived on, for the log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The `push_notify` frame on the desktop's relay WebSocket.
    WebSocket,
    /// `POST /v1/push`.
    Https,
}

impl Via {
    fn label(self) -> &'static str {
        match self {
            Via::WebSocket => "ws",
            Via::Https => "https",
        }
    }
}

/// Why a push was refused. Mapped to an `error` frame on the WebSocket
/// ([`Self::code`]) and to a status + [`flightdeck_remote_protocol::PushErrorBody`]
/// over HTTPS ([`Self::http_status`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushRejection {
    /// Malformed field, or the device is the pairing's phone rather than its
    /// desktop. `bad_frame` / HTTP 400.
    BadFrame(&'static str),
    /// The device is not a member of the pairing, or the pairing does not
    /// exist. `unknown_pairing` / HTTP 404.
    UnknownPairing,
    /// The `ciphertext` is over [`MAX_PUSH_CIPHERTEXT_B64_LEN`].
    /// `bad_frame` / HTTP 413.
    TooLarge,
    /// The device's push budget is spent. `rate_limited` / HTTP 429.
    RateLimited,
}

impl PushRejection {
    /// The wire error code (spec §5.5.1, §5.5.3).
    pub fn code(self) -> RelayErrorCode {
        match self {
            PushRejection::BadFrame(_) | PushRejection::TooLarge => RelayErrorCode::BadFrame,
            PushRejection::UnknownPairing => RelayErrorCode::UnknownPairing,
            PushRejection::RateLimited => RelayErrorCode::RateLimited,
        }
    }

    /// The HTTPS status (spec §5.5.3).
    pub fn http_status(self) -> StatusCode {
        match self {
            PushRejection::BadFrame(_) => StatusCode::BAD_REQUEST,
            PushRejection::UnknownPairing => StatusCode::NOT_FOUND,
            PushRejection::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            PushRejection::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        }
    }

    /// Human-readable detail. Never contains push content.
    pub fn message(self) -> &'static str {
        match self {
            PushRejection::BadFrame(detail) => detail,
            PushRejection::UnknownPairing => "not the desktop member of this pairing",
            PushRejection::TooLarge => "push ciphertext exceeds 3000 characters",
            PushRejection::RateLimited => "push rate limit exceeded",
        }
    }
}

/// A fixed-window, per-device push rate limiter: at most `limit` accepted
/// pushes per `window` per desktop. Stale windows are pruned as the map grows,
/// so memory is bounded by the number of desktops that pushed recently.
pub struct PushRateLimiter {
    limit: u32,
    window: Duration,
    windows: Mutex<HashMap<DeviceId, (Instant, u32)>>,
}

/// Prune expired windows once the map holds this many devices.
const LIMITER_PRUNE_AT: usize = 1024;

impl PushRateLimiter {
    /// A limiter admitting `limit` pushes per `window` per device. A `limit` of
    /// 0 is treated as 1 (a relay that can never push is a misconfiguration,
    /// not a feature).
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit: limit.max(1),
            window,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Record a push attempt by `device` at `now`; `true` if it is within the
    /// budget (and was counted), `false` if the device is over it.
    pub fn check(&self, device: &DeviceId, now: Instant) -> bool {
        let mut windows = self.windows.lock().expect("push limiter mutex poisoned");
        if windows.len() >= LIMITER_PRUNE_AT {
            let window = self.window;
            windows.retain(|_, (start, _)| now.duration_since(*start) < window);
        }
        let entry = windows.entry(device.clone()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        if entry.1 >= self.limit {
            return false;
        }
        entry.1 += 1;
        true
    }
}

/// Validate an authenticated device's push and, if it passes, deliver it: the
/// single path shared by the WebSocket `push_notify` frame and `POST /v1/push`
/// (see the module docs for the exact checks and their order).
///
/// `Ok(())` means accepted — including the silent no-op when the pairing has no
/// registered push token (spec §5.5.1). The APNs send itself runs on a detached
/// task; its outcome is logged, never returned (the request is
/// fire-and-forget on both transports).
pub async fn accept_push(
    state: &AppState,
    device: &DeviceId,
    push: PushNotifyBody,
    via: Via,
) -> Result<(), PushRejection> {
    let result = validate(state, device, &push).await;
    if let Err(rejection) = result {
        tracing::info!(
            pairing = %push.pairing_id, event = %push.event_id, class = %push.class,
            via = via.label(), code = ?rejection.code(), reason = rejection.message(),
            "push refused"
        );
        return Err(rejection);
    }

    let Some((token, environment)) = state.store.push_token(&push.pairing_id).await else {
        tracing::debug!(
            pairing = %push.pairing_id, event = %push.event_id, class = %push.class,
            via = via.label(), outcome = "no_token",
            "push: no push token registered for this pairing; nothing sent"
        );
        return Ok(());
    };

    let service = state.push.clone();
    tokio::spawn(async move {
        let outcome = service.push_alert(&token, environment, &push).await;
        match outcome {
            PushOutcome::Sent | PushOutcome::NotConfigured => tracing::info!(
                pairing = %push.pairing_id, event = %push.event_id, class = %push.class,
                via = via.label(), outcome = %outcome, "push"
            ),
            _ => tracing::warn!(
                pairing = %push.pairing_id, event = %push.event_id, class = %push.class,
                via = via.label(), outcome = %outcome, "push"
            ),
        }
    });
    Ok(())
}

/// Steps 1–3 of [`accept_push`]: membership, field limits, rate limit.
async fn validate(
    state: &AppState,
    device: &DeviceId,
    push: &PushNotifyBody,
) -> Result<(), PushRejection> {
    let members = state
        .store
        .pairing_members(&push.pairing_id)
        .await
        .ok_or(PushRejection::UnknownPairing)?;
    if members.desktop != *device {
        return Err(if members.phone.as_ref() == Some(device) {
            PushRejection::BadFrame("push_notify is accepted only from a desktop")
        } else {
            PushRejection::UnknownPairing
        });
    }

    let event_id = push.event_id.as_str();
    if event_id.is_empty() || event_id.len() > MAX_EVENT_ID_BYTES {
        return Err(PushRejection::BadFrame("event_id must be 1-64 bytes"));
    }
    if push.nonce.is_empty() || push.nonce.len() > MAX_PUSH_NONCE_B64_LEN {
        return Err(PushRejection::BadFrame("malformed push nonce"));
    }
    if push.ciphertext.is_empty() {
        return Err(PushRejection::BadFrame("empty push ciphertext"));
    }
    if push.ciphertext.len() > MAX_PUSH_CIPHERTEXT_B64_LEN {
        return Err(PushRejection::TooLarge);
    }

    if !state.push_limiter.check(device, Instant::now()) {
        return Err(PushRejection::RateLimited);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apns::PushService;
    use crate::config::{Config, LogFormat};
    use async_trait::async_trait;
    use flightdeck_remote_protocol::{ApnsEnvironment, EventId, PairingId, PushClass};
    use std::sync::Arc;

    #[derive(Default)]
    struct RecordingPush {
        fired: Mutex<Vec<(String, PushNotifyBody)>>,
    }

    #[async_trait]
    impl PushService for RecordingPush {
        async fn push_alert(
            &self,
            token: &str,
            _environment: ApnsEnvironment,
            push: &PushNotifyBody,
        ) -> PushOutcome {
            self.fired
                .lock()
                .unwrap()
                .push((token.to_string(), push.clone()));
            PushOutcome::Sent
        }
    }

    impl RecordingPush {
        /// The push runs on a detached task; wait (bounded) for it to land.
        async fn wait_for(&self, n: usize) -> Vec<(String, PushNotifyBody)> {
            for _ in 0..200 {
                if self.fired.lock().unwrap().len() >= n {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            self.fired.lock().unwrap().clone()
        }
    }

    /// A store with one pairing (desktop `dev_mac`, phone `dev_phone`), with or
    /// without a push token.
    async fn setup(with_token: bool) -> (AppState, Arc<RecordingPush>, PairingId) {
        let push = Arc::new(RecordingPush::default());
        let state = AppState::with_push(Config::new(0, LogFormat::Pretty, "test"), push.clone());
        let pairing = state.store.create_pairing(DeviceId::new("dev_mac")).await;
        state
            .store
            .add_phone_to_pairing(&pairing, DeviceId::new("dev_phone"))
            .await
            .unwrap();
        if with_token {
            state
                .store
                .register_push_token(pairing.clone(), "tok".into(), ApnsEnvironment::Sandbox)
                .await;
        }
        (state, push, pairing)
    }

    fn body(pairing: &PairingId) -> PushNotifyBody {
        PushNotifyBody {
            pairing_id: pairing.clone(),
            event_id: EventId::new("6f9a1c2e-3b4d-4e5f-8a7b-1c2d3e4f5a6b"),
            class: PushClass::NeedsInput,
            nonce: "AAECAwQFBgcICQoL".into(),
            ciphertext: "AP5g0H3C".into(),
        }
    }

    #[tokio::test]
    async fn desktop_member_push_is_sent_once() {
        let (state, push, pairing) = setup(true).await;
        let device = DeviceId::new("dev_mac");
        accept_push(&state, &device, body(&pairing), Via::WebSocket)
            .await
            .expect("accepted");
        let fired = push.wait_for(1).await;
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].0, "tok");
        assert_eq!(fired[0].1, body(&pairing));
    }

    #[tokio::test]
    async fn no_token_is_silent_success() {
        let (state, push, pairing) = setup(false).await;
        accept_push(
            &state,
            &DeviceId::new("dev_mac"),
            body(&pairing),
            Via::Https,
        )
        .await
        .expect("accepted without a token");
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(push.fired.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn phone_member_is_bad_frame_and_strangers_unknown_pairing() {
        let (state, push, pairing) = setup(true).await;
        let phone = accept_push(
            &state,
            &DeviceId::new("dev_phone"),
            body(&pairing),
            Via::Https,
        )
        .await
        .unwrap_err();
        assert!(matches!(phone, PushRejection::BadFrame(_)));
        assert_eq!(phone.http_status(), StatusCode::BAD_REQUEST);

        let stranger = accept_push(
            &state,
            &DeviceId::new("dev_other"),
            body(&pairing),
            Via::Https,
        )
        .await
        .unwrap_err();
        assert_eq!(stranger, PushRejection::UnknownPairing);
        assert_eq!(stranger.http_status(), StatusCode::NOT_FOUND);

        let unknown = accept_push(
            &state,
            &DeviceId::new("dev_mac"),
            body(&PairingId::new("pair_gone")),
            Via::Https,
        )
        .await
        .unwrap_err();
        assert_eq!(unknown, PushRejection::UnknownPairing);
        assert!(push.fired.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn field_limits() {
        let (state, push, pairing) = setup(true).await;
        let device = DeviceId::new("dev_mac");

        let mut big = body(&pairing);
        big.ciphertext = "A".repeat(MAX_PUSH_CIPHERTEXT_B64_LEN + 1);
        let err = accept_push(&state, &device, big, Via::Https)
            .await
            .unwrap_err();
        assert_eq!(err, PushRejection::TooLarge);
        assert_eq!(err.code(), RelayErrorCode::BadFrame);
        assert_eq!(err.http_status(), StatusCode::PAYLOAD_TOO_LARGE);

        // Exactly at the cap is fine.
        let mut at_cap = body(&pairing);
        at_cap.ciphertext = "A".repeat(MAX_PUSH_CIPHERTEXT_B64_LEN);
        accept_push(&state, &device, at_cap, Via::Https)
            .await
            .expect("at the cap");

        let mut long_event = body(&pairing);
        long_event.event_id = EventId::new("e".repeat(MAX_EVENT_ID_BYTES + 1));
        assert!(matches!(
            accept_push(&state, &device, long_event, Via::Https).await,
            Err(PushRejection::BadFrame(_))
        ));

        let mut no_cipher = body(&pairing);
        no_cipher.ciphertext.clear();
        assert!(matches!(
            accept_push(&state, &device, no_cipher, Via::Https).await,
            Err(PushRejection::BadFrame(_))
        ));

        let mut long_nonce = body(&pairing);
        long_nonce.nonce = "A".repeat(MAX_PUSH_NONCE_B64_LEN + 1);
        assert!(matches!(
            accept_push(&state, &device, long_nonce, Via::Https).await,
            Err(PushRejection::BadFrame(_))
        ));

        assert_eq!(push.wait_for(1).await.len(), 1, "only the at-cap push");
    }

    #[test]
    fn rate_limiter_windows_per_device() {
        let limiter = PushRateLimiter::new(2, Duration::from_secs(60));
        let a = DeviceId::new("a");
        let b = DeviceId::new("b");
        let t0 = Instant::now();
        assert!(limiter.check(&a, t0));
        assert!(limiter.check(&a, t0));
        assert!(!limiter.check(&a, t0), "third in the window is refused");
        assert!(limiter.check(&b, t0), "budgets are per device");
        // A new window resets the budget.
        assert!(limiter.check(&a, t0 + Duration::from_secs(60)));
    }

    #[tokio::test]
    async fn rate_limit_applies_after_validation() {
        let (mut state, push, pairing) = setup(true).await;
        state.push_limiter = Arc::new(PushRateLimiter::new(1, Duration::from_secs(60)));
        let device = DeviceId::new("dev_mac");
        accept_push(&state, &device, body(&pairing), Via::WebSocket)
            .await
            .expect("first is within budget");
        let err = accept_push(&state, &device, body(&pairing), Via::WebSocket)
            .await
            .unwrap_err();
        assert_eq!(err, PushRejection::RateLimited);
        assert_eq!(err.code(), RelayErrorCode::RateLimited);
        assert_eq!(push.wait_for(1).await.len(), 1);
    }
}
