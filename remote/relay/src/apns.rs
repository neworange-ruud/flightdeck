//! APNs push sender (PRD §5.2/§9.1, spec §5.5/§11).
//!
//! When the desktop seals a `DesktopToPhone::Event` (needs-input / finished /
//! error) and the phone is **offline**, the relay queues the envelope *and*
//! fires an APNs notification against the pairing's registered token so the
//! phone wakes, reconnects, `resume`s the queued envelope, and lands the user
//! on the agent (spec §11 step 1).
//!
//! **Zero-knowledge is preserved.** The relay is a blind pipe — it never
//! decrypts an `EncryptedEnvelope`, so it *cannot* read an event's typed
//! payload or deep link. The live offline path therefore fires a
//! **content-available background wake** push ([`ApnsRequest::background_wake`]):
//! no user content crosses the relay; the phone rebuilds the rich, typed,
//! deep-linked notification locally from the decrypted `AgentEvent` (that
//! mapping lives on the iOS side, `Features/Push`).
//!
//! The **typed** `AgentEvent → alert payload` mapping
//! ([`notification_content`] / [`ApnsRequest::alert_for_event`]) is nonetheless
//! implemented and tested here: it is the canonical, cross-checked contract for
//! the APNs alert shape (aps alert + `deep_link` + `event_id`, deduped via
//! `apns-collapse-id = event_id`, spec §6.4), usable directly by any
//! plaintext-aware pusher (e.g. a future desktop-driven push once notification
//! content is explicitly carried outside E2E, spec §5.5).
//!
//! **Credentials are injected, never hardcoded.** [`ApnsConfig`] carries the
//! `.p8` ES256 auth key (PEM), key id, team id, and topic; a real network
//! transport ([`ApnsTransport`]) is only compiled under the `apns-live` feature
//! (it needs Apple credentials to exercise and is the deployment's manual
//! step). Everything else — JWT construction, payload/request construction, and
//! the offline-delivery decision — is unit-tested without any Apple secret,
//! using a generated test key and a recording transport.

use crate::store::RelayStore;
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use flightdeck_remote_protocol::{AgentEvent, ApnsEnvironment, EventKind, PairingId};
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The APNs credentials + topic needed to authenticate and address pushes.
/// Every field is injected (from the environment, see [`ApnsConfig::from_env`])
/// so no secret is ever compiled in.
#[derive(Clone)]
pub struct ApnsConfig {
    /// Apple Developer team id (the JWT `iss`).
    pub team_id: String,
    /// The `.p8` auth key's key id (the JWT header `kid`).
    pub key_id: String,
    /// The app's bundle id (the `apns-topic` header).
    pub topic: String,
    /// The ES256 (`.p8`) auth key, PEM (PKCS#8) encoded — as downloaded from
    /// the Apple Developer portal.
    pub auth_key_pem: String,
    /// Which APNs host to target when a token doesn't carry its own
    /// environment (tokens registered by the phone do — see
    /// [`ApnsPushService::notify_offline`]).
    pub default_environment: ApnsEnvironment,
}

impl std::fmt::Debug for ApnsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the auth key.
        f.debug_struct("ApnsConfig")
            .field("team_id", &self.team_id)
            .field("key_id", &self.key_id)
            .field("topic", &self.topic)
            .field("auth_key_pem", &"<redacted>")
            .field("default_environment", &self.default_environment)
            .finish()
    }
}

impl ApnsConfig {
    /// Read the APNs config from the environment, returning `None` (push
    /// disabled) unless *all* required pieces are present. The auth key is read
    /// from the file named by `APNS_AUTH_KEY_PATH` (the `.p8` file). Missing or
    /// unreadable pieces disable push rather than failing relay startup — the
    /// relay still routes and queues; it just cannot wake an offline phone.
    ///
    /// Required: `APNS_TEAM_ID`, `APNS_KEY_ID`, `APNS_TOPIC`,
    /// `APNS_AUTH_KEY_PATH`. Optional: `APNS_ENVIRONMENT`
    /// (`sandbox` | `production`, default `production`).
    pub fn from_env() -> Option<Self> {
        let team_id = std::env::var("APNS_TEAM_ID")
            .ok()
            .filter(|s| !s.is_empty())?;
        let key_id = std::env::var("APNS_KEY_ID")
            .ok()
            .filter(|s| !s.is_empty())?;
        let topic = std::env::var("APNS_TOPIC").ok().filter(|s| !s.is_empty())?;
        let path = std::env::var("APNS_AUTH_KEY_PATH")
            .ok()
            .filter(|s| !s.is_empty())?;
        let auth_key_pem = std::fs::read_to_string(&path).ok()?;
        let default_environment = match std::env::var("APNS_ENVIRONMENT").ok().as_deref() {
            Some(v) if v.eq_ignore_ascii_case("sandbox") => ApnsEnvironment::Sandbox,
            _ => ApnsEnvironment::Production,
        };
        Some(Self {
            team_id,
            key_id,
            topic,
            auth_key_pem,
            default_environment,
        })
    }
}

/// Why building an APNs JWT failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JwtError {
    /// The `.p8` PEM could not be parsed as a PKCS#8 ECDSA P-256 key.
    BadKey,
}

/// How long a minted provider JWT is reused before it is re-minted. Apple
/// accepts a provider token for ~1h and rejects re-minting it more than once
/// per ~20 min (`TooManyProviderTokenUpdates`, HTTP 429); refreshing at 20 min
/// stays safely inside both bounds while avoiding a fresh mint per push
/// (remote-control-0ef.15).
pub const JWT_REFRESH_SECS: i64 = 20 * 60;

/// Store-and-forward window for the background wake push, in seconds. The
/// `apns-expiration` header is an *absolute* unix time; a non-zero value tells
/// APNs to hold and retry delivery to a briefly-unreachable phone rather than
/// deliver-once-or-discard (`apns-expiration: 0`). Five minutes survives a
/// short offline/low-power blip while bounding how stale a wake can be
/// (remote-control-0ef.5).
pub const WAKE_EXPIRATION_WINDOW_SECS: i64 = 5 * 60;

/// Default number of send attempts (1 initial + retries) for a transient push
/// failure before giving up (remote-control-0ef.14).
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
/// Default backoff between transient retries. Injectable so tests run with
/// `Duration::ZERO` (no real sleep).
const DEFAULT_BACKOFF: Duration = Duration::from_millis(200);

/// Mints an APNs provider-authentication token (a short-lived ES256 JWT) from a
/// `.p8` key (spec §5.5 "JWT ES256 auth"). Apple accepts a token for ~1 hour
/// and requires it be re-minted no more than once every ~20 minutes; callers
/// should cache and refresh accordingly.
///
/// `now_secs` is unix time in **seconds** (the JWT `iat`), taken as an argument
/// so the construction is deterministic under test.
pub fn build_jwt(config: &ApnsConfig, now_secs: i64) -> Result<String, JwtError> {
    let signing_key =
        SigningKey::from_pkcs8_pem(&config.auth_key_pem).map_err(|_| JwtError::BadKey)?;

    // ES256 JWT: header + claims are compact-JSON, base64url-no-pad, joined by
    // '.'; the signature is over that joined "signing input".
    let header = format!(r#"{{"alg":"ES256","kid":"{}"}}"#, config.key_id);
    let claims = format!(r#"{{"iss":"{}","iat":{}}}"#, config.team_id, now_secs);
    let signing_input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.as_bytes()),
        URL_SAFE_NO_PAD.encode(claims.as_bytes())
    );

    // p256's ECDSA signer hashes with SHA-256 internally (matching APNs' ES256);
    // `to_bytes()` is the fixed 64-byte r‖s form JWT requires (not DER).
    let signature: Signature = signing_key.sign(signing_input.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());
    Ok(format!("{signing_input}.{sig_b64}"))
}

/// How urgently iOS should present a notification. Mirrors
/// `UNNotificationInterruptionLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptionLevel {
    /// Presented immediately, may break through Focus (needs-input, PRD §5.2).
    TimeSensitive,
    /// Standard delivery (finished / error).
    Active,
}

impl InterruptionLevel {
    /// The `aps."interruption-level"` wire string.
    pub fn wire(self) -> &'static str {
        match self {
            InterruptionLevel::TimeSensitive => "time-sensitive",
            InterruptionLevel::Active => "active",
        }
    }
}

/// The display content of a typed notification. The deep link and event id are
/// carried separately on [`ApnsRequest`] (they are routing/dedup metadata, not
/// shown text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationContent {
    /// Notification title, e.g. `fix-login needs your input`.
    pub title: String,
    /// Notification body, e.g. the pending question or the finish summary.
    pub body: String,
    /// The sound file to play (`needs_input.caf` for the urgent case, else the
    /// system default) — the "distinct sound" of PRD §5.2.
    pub sound: String,
    /// How urgently to present it.
    pub interruption_level: InterruptionLevel,
}

/// Sound played for a *needs input* notification (PRD §5.2 "distinct sound").
/// Bundled with the app; falls back to the default if absent.
pub const NEEDS_INPUT_SOUND: &str = "needs_input.caf";
/// The system default notification sound.
pub const DEFAULT_SOUND: &str = "default";

/// Map a typed [`AgentEvent`] to its notification display content (PRD §5.2
/// copy). This is the canonical mapping shared, in spirit, with the iOS
/// `NotificationContentMapper` (mirrored, then cross-checked in tests).
pub fn notification_content(event: &AgentEvent) -> NotificationContent {
    match &event.kind {
        EventKind::NeedsInput { preview } => NotificationContent {
            title: event.title.clone(),
            body: preview.clone(),
            sound: NEEDS_INPUT_SOUND.to_string(),
            interruption_level: InterruptionLevel::TimeSensitive,
        },
        EventKind::Finished {
            summary,
            files_changed,
            ready_to_push,
        } => {
            // "18 files changed · ready to push" (PRD §5.2). The summary, when
            // present, follows as extra context.
            let files = format!(
                "{files_changed} file{} changed",
                if *files_changed == 1 { "" } else { "s" }
            );
            let ready = if *ready_to_push {
                " · ready to push"
            } else {
                ""
            };
            let body = if summary.is_empty() {
                format!("{files}{ready}")
            } else {
                format!("{files}{ready} · {summary}")
            };
            NotificationContent {
                title: event.title.clone(),
                body,
                sound: DEFAULT_SOUND.to_string(),
                interruption_level: InterruptionLevel::Active,
            }
        }
        EventKind::Error { message } => NotificationContent {
            title: event.title.clone(),
            body: message.clone(),
            sound: DEFAULT_SOUND.to_string(),
            interruption_level: InterruptionLevel::Active,
        },
    }
}

/// The APNs host authority for an environment.
fn host(environment: ApnsEnvironment) -> &'static str {
    match environment {
        ApnsEnvironment::Production => "api.push.apple.com",
        ApnsEnvironment::Sandbox => "api.sandbox.push.apple.com",
    }
}

/// A fully-constructed APNs HTTP/2 request, independent of any HTTP client so
/// it can be asserted on directly in tests. A transport ([`ApnsTransport`])
/// turns this into an actual `POST https://{authority}{path}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApnsRequest {
    /// The `:authority` (host) — production or sandbox APNs.
    pub authority: String,
    /// The `:path`, `/3/device/<device-token>`.
    pub path: String,
    /// HTTP/2 request headers (lowercased names), in a stable order.
    pub headers: Vec<(String, String)>,
    /// The JSON request body.
    pub body: Vec<u8>,
}

impl ApnsRequest {
    /// Build the **typed alert** request for an event: the aps alert (title /
    /// body / sound / interruption level) plus `event_id` and the `deep_link`
    /// so a tap lands on the agent. `apns-collapse-id` is set to the event id so
    /// a queued-then-replayed event coalesces into a single notification rather
    /// than double-firing (spec §6.4 dedup).
    pub fn alert_for_event(
        config: &ApnsConfig,
        jwt: &str,
        device_token: &str,
        environment: ApnsEnvironment,
        event: &AgentEvent,
    ) -> Self {
        let content = notification_content(event);
        let dl = &event.deep_link;
        let body = serde_json::json!({
            "aps": {
                "alert": { "title": content.title, "body": content.body },
                "sound": content.sound,
                "interruption-level": content.interruption_level.wire(),
                "mutable-content": 1,
            },
            "event_id": event.event_id,
            "deep_link": {
                "project_id": dl.project_id,
                "session_id": dl.session_id,
                "item_id": dl.item_id,
            },
        });
        // A user-facing alert is immediate (priority 10); it keeps the
        // deliver-once expiration ("0") — store-and-forward is the wake push's
        // concern (remote-control-0ef.5), and this typed-alert path is not the
        // live offline path.
        let mut headers = base_headers(config, jwt, "alert", "10", "0");
        headers.push(("apns-collapse-id".to_string(), event.event_id.to_string()));
        Self {
            authority: host(environment).to_string(),
            path: format!("/3/device/{device_token}"),
            headers,
            body: serde_json::to_vec(&body).expect("json serialization is infallible here"),
        }
    }

    /// Build a **content-available background wake** request: the zero-knowledge
    /// live path. No user content — it only nudges the phone to reconnect and
    /// `resume` the queued envelope, after which the phone builds the real
    /// notification locally from the decrypted event.
    ///
    /// `now_secs` (unix seconds) sets a **non-zero** `apns-expiration`
    /// (`now + `[`WAKE_EXPIRATION_WINDOW_SECS`]) so APNs stores and retries the
    /// wake to a phone that is momentarily unreachable, instead of discarding it
    /// on the first failed attempt (remote-control-0ef.5).
    pub fn background_wake(
        config: &ApnsConfig,
        jwt: &str,
        device_token: &str,
        environment: ApnsEnvironment,
        now_secs: i64,
    ) -> Self {
        let body = serde_json::json!({ "aps": { "content-available": 1 } });
        let expiration = (now_secs + WAKE_EXPIRATION_WINDOW_SECS).to_string();
        Self {
            authority: host(environment).to_string(),
            path: format!("/3/device/{device_token}"),
            headers: base_headers(config, jwt, "background", "5", &expiration),
            body: serde_json::to_vec(&body).expect("json serialization is infallible here"),
        }
    }
}

/// The headers common to every APNs request: bearer auth, topic, push type,
/// priority, and the `apns-expiration` (as a decimal string — `"0"` for
/// deliver-once-or-discard, or an absolute unix time for store-and-forward).
fn base_headers(
    config: &ApnsConfig,
    jwt: &str,
    push_type: &str,
    priority: &str,
    expiration: &str,
) -> Vec<(String, String)> {
    vec![
        ("authorization".to_string(), format!("bearer {jwt}")),
        ("apns-topic".to_string(), config.topic.clone()),
        ("apns-push-type".to_string(), push_type.to_string()),
        ("apns-priority".to_string(), priority.to_string()),
        ("apns-expiration".to_string(), expiration.to_string()),
    ]
}

/// A failed APNs delivery, classified so the caller can decide whether to retry
/// or purge the token (remote-control-0ef.14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApnsSendError {
    /// A retryable failure: a transport/connection error, or an APNs status that
    /// may succeed later (429 `TooManyProviderTokenUpdates`, 5xx). The caller
    /// retries a bounded number of times.
    Transient(String),
    /// A permanent rejection of *this device token* (410 `Unregistered`, or
    /// 400 `BadDeviceToken`/`DeviceTokenNotForTopic`). Retrying is pointless —
    /// the caller purges the token so the phone re-registers on next connect.
    Permanent(String),
}

/// Classify an APNs failure response by HTTP status + APNs `reason` string
/// (spec §5.5). Permanent when the device token itself is invalid (status 410,
/// or reason `BadDeviceToken`/`Unregistered`/`DeviceTokenNotForTopic`);
/// everything else (429, 5xx, network) is transient and worth a retry.
pub fn classify_apns_failure(status: u16, reason: Option<&str>) -> ApnsSendError {
    let permanent = status == 410
        || matches!(
            reason,
            Some("BadDeviceToken") | Some("Unregistered") | Some("DeviceTokenNotForTopic")
        );
    let detail = match reason {
        Some(r) => format!("apns status {status}: {r}"),
        None => format!("apns status {status}"),
    };
    if permanent {
        ApnsSendError::Permanent(detail)
    } else {
        ApnsSendError::Transient(detail)
    }
}

/// The seam that actually puts an [`ApnsRequest`] on the wire. The real
/// HTTP/2-over-TLS implementation is compiled only under the `apns-live`
/// feature (it needs Apple credentials to be useful); tests inject a recording
/// double.
#[async_trait]
pub trait ApnsTransport: Send + Sync {
    /// Deliver one request to APNs. On failure the error is classified
    /// (transient vs permanent) so the caller can retry or purge; it is never
    /// propagated into the connection state machine (a missed wake push is
    /// recovered by the phone's own reconnect).
    async fn send(&self, request: ApnsRequest) -> Result<(), ApnsSendError>;
}

/// Seam for removing a dead push token from the store when APNs reports it
/// permanently invalid. Kept as a trait so the live push service can purge
/// through the store without `apns.rs` depending on a concrete store type, and
/// so tests assert purges against a recording double (remote-control-0ef.14).
#[async_trait]
pub trait TokenPurge: Send + Sync {
    /// Purge `pairing`'s registered push token so the relay stops firing at a
    /// dead token; the phone re-registers on its next connect.
    async fn purge(&self, pairing: &PairingId);
}

/// Adapts the persistence seam so a live [`ApnsPushService`] can purge a dead
/// token via [`RelayStore::unregister_push_token`].
#[async_trait]
impl TokenPurge for Arc<dyn RelayStore> {
    async fn purge(&self, pairing: &PairingId) {
        self.unregister_push_token(pairing).await;
    }
}

/// The relay's offline-delivery hook. Fired from the envelope path when a
/// desktop→phone envelope is queued for a phone that is **not** currently
/// connected (see `crate::session`). Kept as a trait so the default build wires
/// a no-op and tests wire a recording double.
#[async_trait]
pub trait PushService: Send + Sync {
    /// The phone for `pairing` is offline and an envelope was just queued for
    /// it; wake it via APNs using its registered `token`/`environment`.
    async fn notify_offline(&self, pairing: &PairingId, token: &str, environment: ApnsEnvironment);
}

/// The default [`PushService`]: does nothing. Used when APNs is not configured
/// (no `.p8` credentials) — the relay still queues events for `resume`, it just
/// can't wake a backgrounded phone.
pub struct NoopPushService;

#[async_trait]
impl PushService for NoopPushService {
    async fn notify_offline(
        &self,
        _pairing: &PairingId,
        _token: &str,
        _environment: ApnsEnvironment,
    ) {
    }
}

/// The live [`PushService`]: mints (and caches) a provider JWT and sends a
/// background-wake push through the injected [`ApnsTransport`], retrying
/// transient failures and purging a permanently-dead token.
pub struct ApnsPushService<T: ApnsTransport> {
    config: ApnsConfig,
    transport: T,
    now_secs: Box<dyn Fn() -> i64 + Send + Sync>,
    /// Cached provider JWT + the unix-seconds time it was minted. Shared behind
    /// `Arc` (the service is cloned across connections), so a `Mutex` keeps the
    /// mint/reuse decision race-free (remote-control-0ef.15).
    cached_jwt: Mutex<Option<(String, i64)>>,
    /// Send attempts (1 initial + retries) before giving up on a transient
    /// failure (remote-control-0ef.14).
    max_attempts: u32,
    /// Backoff between transient retries. `Duration::ZERO` in tests (no sleep).
    backoff: Duration,
    /// Where to purge a dead token on a permanent APNs rejection. `None` leaves
    /// the token in place (still stops retrying) (remote-control-0ef.14).
    purge: Option<Arc<dyn TokenPurge>>,
}

impl<T: ApnsTransport> ApnsPushService<T> {
    /// Build a push service over `transport` with the default wall-clock,
    /// default bounded retry, and no token-purge hook (wire one with
    /// [`ApnsPushService::with_purge`]).
    pub fn new(config: ApnsConfig, transport: T) -> Self {
        Self::with_clock(config, transport, || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        })
    }

    /// Build with an injected clock (tests). Deterministic: the same `now_secs`
    /// drives both JWT caching and the wake expiration.
    pub fn with_clock(
        config: ApnsConfig,
        transport: T,
        now_secs: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            config,
            transport,
            now_secs: Box::new(now_secs),
            cached_jwt: Mutex::new(None),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            backoff: DEFAULT_BACKOFF,
            purge: None,
        }
    }

    /// Attach the store-backed token-purge hook (used by the live wiring).
    pub fn with_purge(mut self, purge: impl TokenPurge + 'static) -> Self {
        self.purge = Some(Arc::new(purge));
        self
    }

    /// Override the bounded-retry policy (tests use `Duration::ZERO` for the
    /// backoff so no real time passes).
    pub fn with_retry(mut self, max_attempts: u32, backoff: Duration) -> Self {
        self.max_attempts = max_attempts.max(1);
        self.backoff = backoff;
        self
    }

    /// Return a valid provider JWT, minting one on first use and re-minting only
    /// once [`JWT_REFRESH_SECS`] have elapsed since the last mint; otherwise the
    /// cached token is reused (remote-control-0ef.15). `now` is unix seconds.
    fn provider_jwt(&self, now: i64) -> Result<String, JwtError> {
        let mut guard = self.cached_jwt.lock().expect("jwt cache mutex poisoned");
        if let Some((token, minted_at)) = guard.as_ref() {
            if now.saturating_sub(*minted_at) < JWT_REFRESH_SECS {
                return Ok(token.clone());
            }
        }
        let token = build_jwt(&self.config, now)?;
        *guard = Some((token.clone(), now));
        Ok(token)
    }
}

#[async_trait]
impl<T: ApnsTransport> PushService for ApnsPushService<T> {
    async fn notify_offline(&self, pairing: &PairingId, token: &str, environment: ApnsEnvironment) {
        let now = (self.now_secs)();
        let jwt = match self.provider_jwt(now) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(?e, %pairing, "apns: could not mint JWT; skipping wake push");
                return;
            }
        };
        let request = ApnsRequest::background_wake(&self.config, &jwt, token, environment, now);

        for attempt in 1..=self.max_attempts {
            match self.transport.send(request.clone()).await {
                Ok(()) => return,
                Err(ApnsSendError::Permanent(reason)) => {
                    // The token is dead: stop firing at it and purge so the
                    // phone is prompted to re-register (remote-control-0ef.14).
                    tracing::warn!(
                        %reason, %pairing,
                        "apns: token permanently rejected; purging (no retry)"
                    );
                    if let Some(purge) = &self.purge {
                        purge.purge(pairing).await;
                    }
                    return;
                }
                Err(ApnsSendError::Transient(reason)) => {
                    if attempt >= self.max_attempts {
                        // Best-effort: the phone's own reconnect recovers a
                        // missed wake.
                        tracing::warn!(
                            %reason, %pairing, attempts = self.max_attempts,
                            "apns: wake push failed after retries; giving up"
                        );
                        return;
                    }
                    tracing::debug!(
                        %reason, %pairing, attempt,
                        "apns: transient wake push failure; retrying"
                    );
                    if !self.backoff.is_zero() {
                        tokio::time::sleep(self.backoff).await;
                    }
                }
            }
        }
    }
}

/// The real HTTP/2-over-TLS APNs transport. Compiled only with `apns-live`
/// because exercising it requires Apple credentials + network egress to
/// `api.push.apple.com` — the deployment's manual step. It is a thin adapter:
/// all request *shape* (JWT, headers, body, path) is built and tested above.
#[cfg(feature = "apns-live")]
pub mod live {
    use super::{classify_apns_failure, ApnsRequest, ApnsSendError, ApnsTransport};
    use async_trait::async_trait;

    /// APNs over HTTP/2 using `reqwest` (ALPN negotiates h2 with Apple).
    pub struct HttpApnsTransport {
        client: reqwest::Client,
    }

    impl HttpApnsTransport {
        /// Build the client, forcing HTTP/2 (APNs requires it).
        pub fn new() -> Result<Self, String> {
            let client = reqwest::Client::builder()
                .http2_prior_knowledge()
                .build()
                .map_err(|e| e.to_string())?;
            Ok(Self { client })
        }
    }

    #[async_trait]
    impl ApnsTransport for HttpApnsTransport {
        async fn send(&self, request: ApnsRequest) -> Result<(), ApnsSendError> {
            let url = format!("https://{}{}", request.authority, request.path);
            let mut req = self.client.post(url).body(request.body);
            for (name, value) in request.headers {
                req = req.header(name, value);
            }
            // A network/connection failure is transient — the phone may just be
            // a blip away; let the caller retry.
            let resp = req
                .send()
                .await
                .map_err(|e| ApnsSendError::Transient(e.to_string()))?;
            if resp.status().is_success() {
                return Ok(());
            }
            // On failure APNs returns a JSON body `{"reason":"..."}`; parse it so
            // a dead token (410/BadDeviceToken) is classified permanent and the
            // caller purges it rather than retrying forever.
            let status = resp.status().as_u16();
            let body = resp.bytes().await.unwrap_or_default();
            let reason = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from));
            Err(classify_apns_failure(status, reason.as_deref()))
        }
    }
}

// `ApnsTransport` is object-safe and used behind `Arc<dyn ...>`; provide the
// impl so an `Arc<T: ApnsTransport>` satisfies the trait in wiring/tests.
#[async_trait]
impl<T: ApnsTransport + ?Sized> ApnsTransport for std::sync::Arc<T> {
    async fn send(&self, request: ApnsRequest) -> Result<(), ApnsSendError> {
        (**self).send(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flightdeck_remote_protocol::{DeepLink, EventId, ItemId, ProjectId, SessionId};
    use p256::ecdsa::{signature::Verifier, VerifyingKey};
    use p256::pkcs8::EncodePrivateKey;
    use p256::SecretKey;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Generate a throwaway P-256 `.p8`-shaped PEM + its verifying key, so JWT
    /// construction is tested without any real Apple secret.
    fn test_config() -> (ApnsConfig, VerifyingKey) {
        let secret = SecretKey::random(&mut rand_core::OsRng);
        let pem = secret
            .to_pkcs8_pem(p256::pkcs8::LineEnding::LF)
            .unwrap()
            .to_string();
        let verifying = *SigningKey::from(&secret).verifying_key();
        let config = ApnsConfig {
            team_id: "TEAM123456".into(),
            key_id: "KEY7890AB".into(),
            topic: "agency.neworange.flightdeck.remote".into(),
            auth_key_pem: pem,
            default_environment: ApnsEnvironment::Production,
        };
        (config, verifying)
    }

    fn needs_input_event() -> AgentEvent {
        AgentEvent {
            event_id: EventId::new("evt_1"),
            kind: EventKind::NeedsInput {
                preview: "Allow `rm -rf dist/`?".into(),
            },
            deep_link: DeepLink {
                project_id: ProjectId::new("proj_1"),
                session_id: SessionId::new("sess_1"),
                item_id: Some(ItemId::new("item_9")),
            },
            occurred_at_ms: 1_752_412_802_000,
            title: "fix-login needs your input".into(),
        }
    }

    #[test]
    fn jwt_has_three_parts_and_verifies() {
        let (config, verifying) = test_config();
        let token = build_jwt(&config, 1_752_412_802).unwrap();

        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT is header.claims.signature");

        // Header + claims decode to the expected JSON.
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], "KEY7890AB");
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "TEAM123456");
        assert_eq!(claims["iat"], 1_752_412_802);

        // Signature verifies against the key that "issued" it.
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig_bytes = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        let signature = Signature::from_slice(&sig_bytes).unwrap();
        assert!(verifying
            .verify(signing_input.as_bytes(), &signature)
            .is_ok());
    }

    #[test]
    fn bad_key_is_reported() {
        let mut config = test_config().0;
        config.auth_key_pem = "not a pem".into();
        assert_eq!(build_jwt(&config, 0), Err(JwtError::BadKey));
    }

    #[test]
    fn needs_input_maps_to_urgent_distinct_sound() {
        let content = notification_content(&needs_input_event());
        assert_eq!(content.title, "fix-login needs your input");
        assert_eq!(content.body, "Allow `rm -rf dist/`?");
        assert_eq!(content.sound, NEEDS_INPUT_SOUND);
        assert_eq!(content.interruption_level, InterruptionLevel::TimeSensitive);
    }

    #[test]
    fn finished_maps_to_files_and_ready_copy() {
        let event = AgentEvent {
            event_id: EventId::new("evt_2"),
            kind: EventKind::Finished {
                summary: "SpecAssistant".into(),
                files_changed: 18,
                ready_to_push: true,
            },
            deep_link: DeepLink {
                project_id: ProjectId::new("p"),
                session_id: SessionId::new("s"),
                item_id: None,
            },
            occurred_at_ms: 0,
            title: "add-tests finished its turn".into(),
        };
        let content = notification_content(&event);
        assert_eq!(
            content.body,
            "18 files changed · ready to push · SpecAssistant"
        );
        assert_eq!(content.sound, DEFAULT_SOUND);
        assert_eq!(content.interruption_level, InterruptionLevel::Active);
    }

    #[test]
    fn single_file_change_is_not_pluralized() {
        let event = AgentEvent {
            event_id: EventId::new("evt_3"),
            kind: EventKind::Finished {
                summary: String::new(),
                files_changed: 1,
                ready_to_push: false,
            },
            deep_link: DeepLink {
                project_id: ProjectId::new("p"),
                session_id: SessionId::new("s"),
                item_id: None,
            },
            occurred_at_ms: 0,
            title: "t".into(),
        };
        assert_eq!(notification_content(&event).body, "1 file changed");
    }

    #[test]
    fn alert_request_carries_deeplink_and_dedup_collapse_id() {
        let (config, _) = test_config();
        let event = needs_input_event();
        let req = ApnsRequest::alert_for_event(
            &config,
            "jwt-abc",
            "devicetoken123",
            ApnsEnvironment::Sandbox,
            &event,
        );

        // Sandbox environment picks the sandbox host; path targets the token.
        assert_eq!(req.authority, "api.sandbox.push.apple.com");
        assert_eq!(req.path, "/3/device/devicetoken123");

        let headers: std::collections::HashMap<_, _> = req.headers.iter().cloned().collect();
        assert_eq!(headers["authorization"], "bearer jwt-abc");
        assert_eq!(headers["apns-topic"], config.topic);
        assert_eq!(headers["apns-push-type"], "alert");
        assert_eq!(headers["apns-priority"], "10");
        // event_id doubles as the APNs collapse id so a replayed event dedups.
        assert_eq!(headers["apns-collapse-id"], "evt_1");

        // Body: typed alert + deep link parseable straight back out (the shape
        // the iOS `PushPayload` parser consumes) — the "both directions" check.
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["aps"]["alert"]["title"], "fix-login needs your input");
        assert_eq!(body["aps"]["alert"]["body"], "Allow `rm -rf dist/`?");
        assert_eq!(body["aps"]["interruption-level"], "time-sensitive");
        assert_eq!(body["aps"]["sound"], NEEDS_INPUT_SOUND);
        assert_eq!(body["event_id"], "evt_1");
        assert_eq!(body["deep_link"]["project_id"], "proj_1");
        assert_eq!(body["deep_link"]["session_id"], "sess_1");
        assert_eq!(body["deep_link"]["item_id"], "item_9");
    }

    #[test]
    fn background_wake_carries_no_content() {
        let (config, _) = test_config();
        let req = ApnsRequest::background_wake(
            &config,
            "jwt-xyz",
            "tok",
            ApnsEnvironment::Production,
            1_752_412_802,
        );
        assert_eq!(req.authority, "api.push.apple.com");
        let headers: std::collections::HashMap<_, _> = req.headers.iter().cloned().collect();
        assert_eq!(headers["apns-push-type"], "background");
        assert_eq!(headers["apns-priority"], "5");
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["aps"]["content-available"], 1);
        assert!(
            body.get("deep_link").is_none(),
            "zero-knowledge: no content"
        );
    }

    /// remote-control-0ef.5: the wake push must use a non-zero, future
    /// `apns-expiration` so APNs stores and retries to a briefly-offline phone.
    #[test]
    fn background_wake_has_nonzero_future_expiration() {
        let (config, _) = test_config();
        let now = 1_752_412_802;
        let req = ApnsRequest::background_wake(
            &config,
            "jwt-xyz",
            "tok",
            ApnsEnvironment::Production,
            now,
        );
        let headers: std::collections::HashMap<_, _> = req.headers.iter().cloned().collect();
        let expiration: i64 = headers["apns-expiration"].parse().unwrap();
        assert_ne!(expiration, 0, "must not be deliver-once-or-discard");
        assert!(expiration > now, "expiration is a future unix time");
        assert_eq!(expiration, now + WAKE_EXPIRATION_WINDOW_SECS);
    }

    #[test]
    fn classify_permanent_and_transient_failures() {
        // 410 is always permanent regardless of reason.
        assert!(matches!(
            classify_apns_failure(410, Some("Unregistered")),
            ApnsSendError::Permanent(_)
        ));
        assert!(matches!(
            classify_apns_failure(410, None),
            ApnsSendError::Permanent(_)
        ));
        // Permanent-by-reason on a 400.
        assert!(matches!(
            classify_apns_failure(400, Some("BadDeviceToken")),
            ApnsSendError::Permanent(_)
        ));
        assert!(matches!(
            classify_apns_failure(400, Some("DeviceTokenNotForTopic")),
            ApnsSendError::Permanent(_)
        ));
        // 429 (TooManyProviderTokenUpdates) and 5xx are transient.
        assert!(matches!(
            classify_apns_failure(429, Some("TooManyProviderTokenUpdates")),
            ApnsSendError::Transient(_)
        ));
        assert!(matches!(
            classify_apns_failure(503, None),
            ApnsSendError::Transient(_)
        ));
    }

    /// Recording transport: captures every request (and always succeeds) so
    /// tests can assert on the live push service's behavior without any network
    /// or Apple secret.
    #[derive(Default)]
    struct RecordingTransport {
        sent: Mutex<Vec<ApnsRequest>>,
    }

    #[async_trait]
    impl ApnsTransport for RecordingTransport {
        async fn send(&self, request: ApnsRequest) -> Result<(), ApnsSendError> {
            self.sent.lock().unwrap().push(request);
            Ok(())
        }
    }

    /// Scripted transport: returns a queued outcome per call (front to back),
    /// then `Ok` once the script is exhausted. Counts calls so a test can prove
    /// exactly how many send attempts happened.
    struct ScriptedTransport {
        outcomes: Mutex<std::collections::VecDeque<Result<(), ApnsSendError>>>,
        calls: AtomicUsize,
    }

    impl ScriptedTransport {
        fn new(outcomes: Vec<Result<(), ApnsSendError>>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into()),
                calls: AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ApnsTransport for ScriptedTransport {
        async fn send(&self, _request: ApnsRequest) -> Result<(), ApnsSendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.outcomes.lock().unwrap().pop_front().unwrap_or(Ok(()))
        }
    }

    /// Recording purge hook: remembers which pairings were purged.
    #[derive(Default)]
    struct RecordingPurge {
        purged: Mutex<Vec<PairingId>>,
    }

    #[async_trait]
    impl TokenPurge for Arc<RecordingPurge> {
        async fn purge(&self, pairing: &PairingId) {
            self.purged.lock().unwrap().push(pairing.clone());
        }
    }

    #[tokio::test]
    async fn push_service_sends_a_background_wake() {
        let (config, _) = test_config();
        let transport = std::sync::Arc::new(RecordingTransport::default());
        let service = ApnsPushService::with_clock(config, transport.clone(), || 1_752_412_802);
        service
            .notify_offline(&PairingId::new("pair_1"), "tok_1", ApnsEnvironment::Sandbox)
            .await;

        let sent = transport.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].authority, "api.sandbox.push.apple.com");
        assert_eq!(sent[0].path, "/3/device/tok_1");
        let headers: std::collections::HashMap<_, _> = sent[0].headers.iter().cloned().collect();
        assert_eq!(headers["apns-push-type"], "background");
        assert!(headers["authorization"].starts_with("bearer "));
    }

    /// remote-control-0ef.15: the provider JWT is minted once, reused within the
    /// refresh window, and re-minted only after it elapses. The clock is driven
    /// explicitly — no real time passes.
    #[tokio::test]
    async fn jwt_is_cached_and_refreshed_on_cadence() {
        let (config, _) = test_config();
        let transport = std::sync::Arc::new(RecordingTransport::default());
        // Advance the injected clock only when the test says so.
        let now = std::sync::Arc::new(AtomicUsize::new(1_000_000));
        let now_reader = now.clone();
        let service = ApnsPushService::with_clock(config, transport.clone(), move || {
            now_reader.load(Ordering::SeqCst) as i64
        });

        let pairing = PairingId::new("pair_1");
        // First push mints.
        service
            .notify_offline(&pairing, "tok", ApnsEnvironment::Sandbox)
            .await;
        // A second push a minute later reuses (inside the 20-min window).
        now.store(1_000_000 + 60, Ordering::SeqCst);
        service
            .notify_offline(&pairing, "tok", ApnsEnvironment::Sandbox)
            .await;
        // A push past the window re-mints.
        now.store(1_000_000 + JWT_REFRESH_SECS as usize + 1, Ordering::SeqCst);
        service
            .notify_offline(&pairing, "tok", ApnsEnvironment::Sandbox)
            .await;

        let sent = transport.sent.lock().unwrap();
        assert_eq!(sent.len(), 3);
        let jwt = |req: &ApnsRequest| {
            req.headers
                .iter()
                .find(|(k, _)| k == "authorization")
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(jwt(&sent[0]), jwt(&sent[1]), "reused within the window");
        assert_ne!(jwt(&sent[1]), jwt(&sent[2]), "re-minted past the window");
    }

    /// remote-control-0ef.14: a transient failure is retried (with zero backoff)
    /// until it succeeds; the token is not purged.
    #[tokio::test]
    async fn transient_failure_is_retried_then_succeeds() {
        let (config, _) = test_config();
        let transport = std::sync::Arc::new(ScriptedTransport::new(vec![
            Err(ApnsSendError::Transient("blip".into())),
            Err(ApnsSendError::Transient("blip".into())),
            Ok(()),
        ]));
        let purge = std::sync::Arc::new(RecordingPurge::default());
        let service = ApnsPushService::with_clock(config, transport.clone(), || 1_752_412_802)
            .with_retry(3, Duration::ZERO)
            .with_purge(purge.clone());

        service
            .notify_offline(&PairingId::new("pair_1"), "tok", ApnsEnvironment::Sandbox)
            .await;

        assert_eq!(transport.calls(), 3, "two retries then success");
        assert!(
            purge.purged.lock().unwrap().is_empty(),
            "transient failure never purges"
        );
    }

    /// remote-control-0ef.14: exhausting the retry budget on persistent
    /// transient failures gives up without purging.
    #[tokio::test]
    async fn transient_failure_gives_up_after_max_attempts() {
        let (config, _) = test_config();
        let transport = std::sync::Arc::new(ScriptedTransport::new(vec![
            Err(ApnsSendError::Transient("blip".into())),
            Err(ApnsSendError::Transient("blip".into())),
            Err(ApnsSendError::Transient("blip".into())),
            Err(ApnsSendError::Transient("blip".into())),
        ]));
        let purge = std::sync::Arc::new(RecordingPurge::default());
        let service = ApnsPushService::with_clock(config, transport.clone(), || 1_752_412_802)
            .with_retry(3, Duration::ZERO)
            .with_purge(purge.clone());

        service
            .notify_offline(&PairingId::new("pair_1"), "tok", ApnsEnvironment::Sandbox)
            .await;

        assert_eq!(transport.calls(), 3, "capped at max_attempts");
        assert!(purge.purged.lock().unwrap().is_empty());
    }

    /// remote-control-0ef.14: a permanent 410/BadDeviceToken purges the token
    /// and does *not* retry.
    #[tokio::test]
    async fn permanent_failure_purges_and_does_not_retry() {
        let (config, _) = test_config();
        let transport = std::sync::Arc::new(ScriptedTransport::new(vec![Err(
            ApnsSendError::Permanent("apns status 410: Unregistered".into()),
        )]));
        let purge = std::sync::Arc::new(RecordingPurge::default());
        let service = ApnsPushService::with_clock(config, transport.clone(), || 1_752_412_802)
            .with_retry(3, Duration::ZERO)
            .with_purge(purge.clone());

        let pairing = PairingId::new("pair_1");
        service
            .notify_offline(&pairing, "tok", ApnsEnvironment::Sandbox)
            .await;

        assert_eq!(transport.calls(), 1, "permanent failure is not retried");
        assert_eq!(
            *purge.purged.lock().unwrap(),
            vec![pairing],
            "the dead token's pairing is purged"
        );
    }

    /// A permanent failure with no purge hook wired still stops (no retry, no
    /// panic) — purging is best-effort.
    #[tokio::test]
    async fn permanent_failure_without_purge_hook_is_safe() {
        let (config, _) = test_config();
        let transport = std::sync::Arc::new(ScriptedTransport::new(vec![Err(
            ApnsSendError::Permanent("apns status 410: BadDeviceToken".into()),
        )]));
        let service = ApnsPushService::with_clock(config, transport.clone(), || 1_752_412_802)
            .with_retry(3, Duration::ZERO);

        service
            .notify_offline(&PairingId::new("pair_1"), "tok", ApnsEnvironment::Sandbox)
            .await;

        assert_eq!(transport.calls(), 1);
    }
}
