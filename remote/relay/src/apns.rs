//! APNs push sender (PRD §5.2/§9.1, spec §5.5/§11).
//!
//! **The desktop decides pushes; the relay only delivers them (protocol v4).**
//! When an agent needs input, hits an error, or is done, the desktop seals a
//! `PushPayload` under the pairing's push key (spec §7.2) and asks the relay for
//! exactly one visible alert with `push_notify` (over its WebSocket) or
//! `POST /v1/push` (over HTTPS) — see [`crate::push`]. The relay builds one APNs
//! request per accepted push ([`ApnsRequest::encrypted_alert`]) and sends it
//! through the [`PushService`] seam. Queued envelopes trigger **no** push: an
//! offline phone catches up on its next open/reconnect.
//!
//! **Zero-knowledge is preserved.** The alert's visible text is generic per
//! class ([`generic_alert_body`], title [`ALERT_TITLE`]); the real title, body
//! and deep link travel sealed in the `fd_push` object, which the phone's
//! Notification Service Extension opens (`mutable-content: 1`) to replace the
//! generic text. The relay copies `nonce`/`ciphertext` verbatim and never opens
//! them; the only plaintext it sees is the pairing id, the event id and the
//! `class`.
//!
//! The pre-v4 silent `content-available` wake push (one per envelope queued for
//! an offline phone) and the plaintext `AgentEvent → alert` mapping are gone:
//! iOS throttles and drops silent pushes, and one wake per envelope was noisy.
//!
//! **Credentials are injected, never hardcoded.** [`ApnsConfig`] carries the
//! `.p8` ES256 auth key (PEM), key id, team id, and topic; a real network
//! transport ([`ApnsTransport`]) is only compiled under the `apns-live` feature
//! (it needs Apple credentials to exercise and is the deployment's manual
//! step). Everything else — JWT construction, request construction, retry and
//! dead-token purge — is unit-tested without any Apple secret, using a
//! generated test key and a recording transport.

use crate::store::RelayStore;
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use flightdeck_remote_protocol::{ApnsEnvironment, PairingId, PushClass, PushNotifyBody};
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
    /// [`PushService::push_alert`]).
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
    /// disabled) unless *all* required pieces are present. Missing or unreadable
    /// pieces disable push rather than failing relay startup — the relay still
    /// routes and queues; it just cannot deliver the desktop's alerts.
    ///
    /// Required: `APNS_TEAM_ID`, `APNS_KEY_ID`, `APNS_TOPIC`, and the auth key
    /// via **either** `APNS_AUTH_KEY_PEM` (the `.p8` contents inline) or
    /// `APNS_AUTH_KEY_PATH` (a path to it). The inline form wins when both are
    /// set: it is what a container platform can inject from a secret store
    /// without also provisioning a volume, which is how the deployed relay is
    /// configured. The path form stays for local runs, where the key is a file
    /// on disk anyway.
    ///
    /// Optional: `APNS_ENVIRONMENT` (`sandbox` | `production`, default
    /// `production`). Note this is only the fallback for tokens that arrive
    /// without one — the phone reports its own environment per token, so a
    /// Debug build and a TestFlight build are routed correctly regardless.
    pub fn from_env() -> Option<Self> {
        Self::from_vars(|key| std::env::var(key).ok())
    }

    /// The body of [`Self::from_env`], with the environment lookup injected.
    ///
    /// Split out to be testable without mutating process environment, which is
    /// global state shared by every test in the binary. Returning `None` here is
    /// what silently disables push in production, so it is worth asserting
    /// directly rather than inferring from a relay whose alerts simply never
    /// arrive.
    fn from_vars(var: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let non_empty = |key: &str| var(key).filter(|s| !s.trim().is_empty());

        let team_id = non_empty("APNS_TEAM_ID")?;
        let key_id = non_empty("APNS_KEY_ID")?;
        let topic = non_empty("APNS_TOPIC")?;
        let auth_key_pem = match non_empty("APNS_AUTH_KEY_PEM") {
            Some(pem) => pem,
            None => std::fs::read_to_string(non_empty("APNS_AUTH_KEY_PATH")?).ok()?,
        };
        let default_environment = match non_empty("APNS_ENVIRONMENT").as_deref() {
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

/// Store-and-forward window for an alert push, in seconds (spec §5.5.2: now +
/// 24 h). The `apns-expiration` header is an *absolute* unix time; a non-zero
/// value tells APNs to hold and retry delivery to an unreachable phone rather
/// than deliver-once-or-discard (`apns-expiration: 0`). A day covers a phone
/// that is off or out of coverage for a while; the collapse id keeps a retried
/// push for the same event from stacking up.
pub const ALERT_EXPIRATION_WINDOW_SECS: i64 = 24 * 60 * 60;

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
    /// Standard delivery (error / done).
    Active,
}

impl InterruptionLevel {
    /// The level for a push class (spec §5.5.2): `time-sensitive` for
    /// `needs_input`, `active` for `error` and `done`.
    pub fn for_class(class: PushClass) -> Self {
        match class {
            PushClass::NeedsInput => InterruptionLevel::TimeSensitive,
            PushClass::Error | PushClass::Done => InterruptionLevel::Active,
        }
    }

    /// The `aps."interruption-level"` wire string.
    pub fn wire(self) -> &'static str {
        match self {
            InterruptionLevel::TimeSensitive => "time-sensitive",
            InterruptionLevel::Active => "active",
        }
    }
}

/// The generic `aps.alert.title` of every push (spec §5.5.2). Never content:
/// the phone's Notification Service Extension replaces it with the sealed
/// title.
pub const ALERT_TITLE: &str = "FlightDeck";

/// The system default notification sound.
pub const DEFAULT_SOUND: &str = "default";

/// Version of the `fd_push` object layout in the APNs payload (spec §5.5.2).
pub const FD_PUSH_VERSION: u32 = 1;

/// The generic `aps.alert.body` for a push class (spec §5.5.2). It is what the
/// phone shows if its Notification Service Extension cannot open the sealed
/// payload, so it names the class and nothing else.
pub fn generic_alert_body(class: PushClass) -> &'static str {
    match class {
        PushClass::NeedsInput => "An agent needs your input",
        PushClass::Error => "An agent hit an error",
        PushClass::Done => "An agent is done",
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
    /// Build the **encrypted alert** request for a desktop-requested push
    /// (spec §5.5.2): a visible alert with generic, per-class text, plus the
    /// sealed payload in `fd_push` for the phone's Notification Service
    /// Extension to open (`mutable-content: 1`).
    ///
    /// The relay stays zero-knowledge: the only plaintext beyond the generic
    /// strings is the `class`, the pairing id (also the `thread-id`, grouping
    /// one Mac's alerts) and the event id. `nonce` and `ciphertext` are copied
    /// verbatim and never opened.
    ///
    /// Headers: `apns-push-type: alert`, `apns-priority: 10`,
    /// `apns-collapse-id: <event_id>` (a retried push for the same event
    /// replaces the alert instead of duplicating it) and a non-zero
    /// `apns-expiration` of `now_secs + `[`ALERT_EXPIRATION_WINDOW_SECS`] so
    /// APNs stores and forwards to a phone that is offline for a while.
    pub fn encrypted_alert(
        config: &ApnsConfig,
        jwt: &str,
        device_token: &str,
        environment: ApnsEnvironment,
        push: &PushNotifyBody,
        now_secs: i64,
    ) -> Self {
        let body = serde_json::json!({
            "aps": {
                "alert": {
                    "title": ALERT_TITLE,
                    "body": generic_alert_body(push.class),
                },
                "mutable-content": 1,
                "sound": DEFAULT_SOUND,
                "interruption-level": InterruptionLevel::for_class(push.class).wire(),
                "thread-id": push.pairing_id.as_str(),
            },
            "fd_push": {
                "v": FD_PUSH_VERSION,
                "pairing_id": push.pairing_id.as_str(),
                "event_id": push.event_id.as_str(),
                "class": push.class.as_str(),
                "nonce": push.nonce,
                "ciphertext": push.ciphertext,
            },
        });
        let expiration = (now_secs + ALERT_EXPIRATION_WINDOW_SECS).to_string();
        let mut headers = base_headers(config, jwt, "alert", "10", &expiration);
        headers.push((
            "apns-collapse-id".to_string(),
            push.event_id.as_str().to_string(),
        ));
        Self {
            authority: host(environment).to_string(),
            path: format!("/3/device/{device_token}"),
            headers,
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
    /// propagated into the connection state machine (a dropped alert is
    /// reported in the [`PushOutcome`]; the phone still catches up on its next
    /// open).
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

/// What happened to one push attempt, for the per-push log line (spec §5.5.2,
/// remote-control-ygi) and for tests. Never carries content or the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// APNs accepted the request.
    Sent,
    /// APNs is not configured on this relay (no `.p8` credentials, or a build
    /// without the `apns-live` feature): nothing was sent.
    NotConfigured,
    /// The provider JWT could not be minted (bad `.p8` key): nothing was sent.
    JwtFailed,
    /// Every attempt failed with a transient error; the push was dropped.
    TransientFailed {
        /// How many send attempts were made.
        attempts: u32,
    },
    /// APNs rejected the device token permanently (410 / `BadDeviceToken`);
    /// it was not retried. `purged` says whether the token was removed from
    /// the store (it is whenever a purge hook is wired, i.e. in production).
    Permanent {
        /// Whether the dead token was purged from the store.
        purged: bool,
    },
}

impl PushOutcome {
    /// A short, stable label for log lines.
    pub fn label(self) -> &'static str {
        match self {
            PushOutcome::Sent => "sent",
            PushOutcome::NotConfigured => "apns_not_configured",
            PushOutcome::JwtFailed => "jwt_failed",
            PushOutcome::TransientFailed { .. } => "transient_failed_after_retries",
            PushOutcome::Permanent { purged: true } => "permanent_token_purged",
            PushOutcome::Permanent { purged: false } => "permanent",
        }
    }
}

impl std::fmt::Display for PushOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// The relay's push-delivery seam: send one encrypted alert (spec §5.5.2) for
/// a desktop-requested push. Called only from [`crate::push`], after the push
/// was authenticated and validated and a token was found. Kept as a trait so
/// the default build wires a no-op and tests wire a recording double.
#[async_trait]
pub trait PushService: Send + Sync {
    /// Deliver `push` as a visible alert to the phone holding `token` in
    /// `environment`, and report what happened. Never fails the caller: a
    /// dropped push is reported in the outcome and logged by the caller.
    async fn push_alert(
        &self,
        token: &str,
        environment: ApnsEnvironment,
        push: &PushNotifyBody,
    ) -> PushOutcome;
}

/// The default [`PushService`]: does nothing. Used when APNs is not configured
/// (no `.p8` credentials) — the relay still authenticates and validates pushes,
/// it just cannot deliver them.
pub struct NoopPushService;

#[async_trait]
impl PushService for NoopPushService {
    async fn push_alert(
        &self,
        _token: &str,
        _environment: ApnsEnvironment,
        _push: &PushNotifyBody,
    ) -> PushOutcome {
        PushOutcome::NotConfigured
    }
}

/// The live [`PushService`]: mints (and caches) a provider JWT and sends the
/// encrypted alert through the injected [`ApnsTransport`], retrying transient
/// failures and purging a permanently-dead token.
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
    /// drives both JWT caching and the alert expiration.
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
    async fn push_alert(
        &self,
        token: &str,
        environment: ApnsEnvironment,
        push: &PushNotifyBody,
    ) -> PushOutcome {
        let now = (self.now_secs)();
        let jwt = match self.provider_jwt(now) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(?e, pairing = %push.pairing_id, "apns: could not mint JWT");
                return PushOutcome::JwtFailed;
            }
        };
        let request =
            ApnsRequest::encrypted_alert(&self.config, &jwt, token, environment, push, now);

        for attempt in 1..=self.max_attempts {
            match self.transport.send(request.clone()).await {
                Ok(()) => return PushOutcome::Sent,
                Err(ApnsSendError::Permanent(reason)) => {
                    // The token is dead: stop firing at it and purge so the
                    // phone is prompted to re-register (remote-control-0ef.14).
                    tracing::debug!(
                        %reason, pairing = %push.pairing_id,
                        "apns: token permanently rejected (no retry)"
                    );
                    let purged = match &self.purge {
                        Some(purge) => {
                            purge.purge(&push.pairing_id).await;
                            true
                        }
                        None => false,
                    };
                    return PushOutcome::Permanent { purged };
                }
                Err(ApnsSendError::Transient(reason)) => {
                    if attempt >= self.max_attempts {
                        tracing::debug!(
                            %reason, pairing = %push.pairing_id, attempts = self.max_attempts,
                            "apns: push failed after retries; giving up"
                        );
                        return PushOutcome::TransientFailed {
                            attempts: self.max_attempts,
                        };
                    }
                    tracing::debug!(
                        %reason, pairing = %push.pairing_id, attempt,
                        "apns: transient push failure; retrying"
                    );
                    if !self.backoff.is_zero() {
                        tokio::time::sleep(self.backoff).await;
                    }
                }
            }
        }
        // `max_attempts >= 1`, so the loop always returns; kept total anyway.
        PushOutcome::TransientFailed {
            attempts: self.max_attempts,
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
    use flightdeck_remote_protocol::EventId;
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

    /// A complete, valid set of the variables the deployed relay injects.
    fn full_vars() -> std::collections::HashMap<&'static str, &'static str> {
        std::collections::HashMap::from([
            ("APNS_TEAM_ID", "7NKCS4AZS9"),
            ("APNS_KEY_ID", "DH74K2J4CN"),
            ("APNS_TOPIC", "agency.neworange.flightdeck.remote"),
            ("APNS_AUTH_KEY_PEM", "-----BEGIN PRIVATE KEY-----\nx\n"),
        ])
    }

    fn from_map(
        vars: &std::collections::HashMap<&'static str, &'static str>,
    ) -> Option<ApnsConfig> {
        ApnsConfig::from_vars(|key| vars.get(key).map(|v| (*v).to_string()))
    }

    /// The inline key is what the container platform injects from its secret
    /// store; no file, no volume.
    #[test]
    fn config_reads_the_inline_auth_key() {
        let config = from_map(&full_vars()).expect("complete config should enable push");
        assert_eq!(config.team_id, "7NKCS4AZS9");
        assert_eq!(config.key_id, "DH74K2J4CN");
        assert_eq!(config.topic, "agency.neworange.flightdeck.remote");
        assert!(config
            .auth_key_pem
            .starts_with("-----BEGIN PRIVATE KEY-----"));
        // Unset APNS_ENVIRONMENT must mean production: a sandbox default would
        // send TestFlight pushes to the wrong APNs host and fail as
        // BadDeviceToken.
        assert_eq!(config.default_environment, ApnsEnvironment::Production);
    }

    /// Each variable is individually load-bearing. Dropping any one disables
    /// push *silently* — which is how the deployed relay ran without APNs
    /// configured until 1.12.0 — so assert every one of them is required.
    #[test]
    fn config_requires_every_variable() {
        for missing in ["APNS_TEAM_ID", "APNS_KEY_ID", "APNS_TOPIC"] {
            let mut vars = full_vars();
            vars.remove(missing);
            assert!(
                from_map(&vars).is_none(),
                "push should be disabled when {missing} is unset"
            );
        }
        // No key material at all, by either route.
        let mut vars = full_vars();
        vars.remove("APNS_AUTH_KEY_PEM");
        assert!(
            from_map(&vars).is_none(),
            "push should be disabled with no auth key"
        );
    }

    /// An empty or whitespace value is what an unset CI variable expands to, and
    /// must be treated as absent rather than accepted as a blank credential.
    #[test]
    fn config_rejects_blank_values() {
        for blank in ["", "   ", "\n"] {
            let mut vars = full_vars();
            vars.insert("APNS_KEY_ID", blank);
            assert!(
                from_map(&vars).is_none(),
                "blank APNS_KEY_ID ({blank:?}) should disable push"
            );
        }
    }

    #[test]
    fn config_honours_sandbox_environment_override() {
        let mut vars = full_vars();
        vars.insert("APNS_ENVIRONMENT", "SandBox");
        let config = from_map(&vars).expect("config should still be complete");
        assert_eq!(config.default_environment, ApnsEnvironment::Sandbox);
    }

    /// The path form has to keep working for local runs, where the `.p8` is a
    /// file on disk. A path that does not exist disables push rather than
    /// panicking at startup.
    #[test]
    fn config_falls_back_to_the_key_path() {
        let dir = std::env::temp_dir().join(format!("fd-apns-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("AuthKey_TEST.p8");
        std::fs::write(&key, "-----BEGIN PRIVATE KEY-----\nfile\n").unwrap();

        let mut vars = full_vars();
        vars.remove("APNS_AUTH_KEY_PEM");
        let path = key.to_str().unwrap().to_string();
        let config = ApnsConfig::from_vars(|k| {
            if k == "APNS_AUTH_KEY_PATH" {
                Some(path.clone())
            } else {
                vars.get(k).map(|v| (*v).to_string())
            }
        })
        .expect("a readable key path should enable push");
        assert!(config.auth_key_pem.contains("file"));

        let missing = dir.join("nope.p8").to_str().unwrap().to_string();
        assert!(
            ApnsConfig::from_vars(|k| {
                if k == "APNS_AUTH_KEY_PATH" {
                    Some(missing.clone())
                } else {
                    vars.get(k).map(|v| (*v).to_string())
                }
            })
            .is_none(),
            "an unreadable key path should disable push, not panic"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Both forms set: the inline key wins, so a stale file cannot shadow what
    /// the secret store injected.
    #[test]
    fn inline_key_takes_precedence_over_path() {
        let mut vars = full_vars();
        vars.insert("APNS_AUTH_KEY_PATH", "/nonexistent/AuthKey.p8");
        let config = from_map(&vars).expect("inline key should be used");
        assert!(config.auth_key_pem.contains('x'));
    }

    fn push(class: PushClass) -> PushNotifyBody {
        PushNotifyBody {
            pairing_id: PairingId::new("pair_ruud_mbp"),
            event_id: EventId::new("6f9a1c2e-3b4d-4e5f-8a7b-1c2d3e4f5a6b"),
            class,
            nonce: "AAECAwQFBgcICQoL".into(),
            ciphertext: "AP5g0H3CJyAB8Vf8b/H9pSudjvO+qoKbU/jVgc5XAl4S".into(),
        }
    }

    fn header_map(req: &ApnsRequest) -> std::collections::HashMap<String, String> {
        req.headers.iter().cloned().collect()
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

    /// Spec §5.5.2: the encrypted alert's headers — alert push type, priority
    /// 10, collapse id = event id, a 24 h store-and-forward expiration, topic.
    #[test]
    fn encrypted_alert_headers() {
        let (config, _) = test_config();
        let now = 1_752_412_802;
        let req = ApnsRequest::encrypted_alert(
            &config,
            "jwt-abc",
            "devicetoken123",
            ApnsEnvironment::Sandbox,
            &push(PushClass::NeedsInput),
            now,
        );

        // Sandbox environment picks the sandbox host; path targets the token.
        assert_eq!(req.authority, "api.sandbox.push.apple.com");
        assert_eq!(req.path, "/3/device/devicetoken123");

        let headers = header_map(&req);
        assert_eq!(headers["authorization"], "bearer jwt-abc");
        assert_eq!(headers["apns-topic"], config.topic);
        assert_eq!(headers["apns-push-type"], "alert");
        assert_eq!(headers["apns-priority"], "10");
        assert_eq!(
            headers["apns-collapse-id"],
            "6f9a1c2e-3b4d-4e5f-8a7b-1c2d3e4f5a6b"
        );
        let expiration: i64 = headers["apns-expiration"].parse().unwrap();
        assert_eq!(expiration, now + 24 * 60 * 60, "now + 24 h, never 0");
        assert_eq!(headers.len(), 6, "no unexpected headers: {headers:?}");
    }

    /// Spec §5.5.2: the body is exactly the generic alert + `fd_push`, and the
    /// whole thing is the documented shape — nothing else leaks into it.
    #[test]
    fn encrypted_alert_body_is_exactly_the_spec_shape() {
        let (config, _) = test_config();
        let p = push(PushClass::NeedsInput);
        let req =
            ApnsRequest::encrypted_alert(&config, "jwt", "tok", ApnsEnvironment::Production, &p, 0);
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        let expected = serde_json::json!({
            "aps": {
                "alert": { "title": "FlightDeck", "body": "An agent needs your input" },
                "mutable-content": 1,
                "sound": "default",
                "interruption-level": "time-sensitive",
                "thread-id": "pair_ruud_mbp",
            },
            "fd_push": {
                "v": 1,
                "pairing_id": "pair_ruud_mbp",
                "event_id": "6f9a1c2e-3b4d-4e5f-8a7b-1c2d3e4f5a6b",
                "class": "needs_input",
                "nonce": p.nonce,
                "ciphertext": p.ciphertext,
            },
        });
        assert_eq!(body, expected);
    }

    /// Generic text and interruption level per class (spec §5.5.2).
    #[test]
    fn encrypted_alert_generic_text_and_level_per_class() {
        let (config, _) = test_config();
        for (class, text, level) in [
            (
                PushClass::NeedsInput,
                "An agent needs your input",
                "time-sensitive",
            ),
            (PushClass::Error, "An agent hit an error", "active"),
            (PushClass::Done, "An agent is done", "active"),
        ] {
            let req = ApnsRequest::encrypted_alert(
                &config,
                "jwt",
                "tok",
                ApnsEnvironment::Production,
                &push(class),
                0,
            );
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["aps"]["alert"]["title"], ALERT_TITLE);
            assert_eq!(body["aps"]["alert"]["body"], text);
            assert_eq!(body["aps"]["interruption-level"], level);
            assert_eq!(body["fd_push"]["class"], class.as_str());
        }
    }

    /// The largest push the relay accepts still fits APNs' 4 KiB payload cap.
    #[test]
    fn largest_accepted_push_fits_apns_payload_limit() {
        let (config, _) = test_config();
        let mut p = push(PushClass::NeedsInput);
        p.ciphertext = "A".repeat(flightdeck_remote_protocol::relay::MAX_PUSH_CIPHERTEXT_B64_LEN);
        p.event_id = EventId::new("e".repeat(crate::push::MAX_EVENT_ID_BYTES));
        p.pairing_id = PairingId::new("p".repeat(64));
        let req =
            ApnsRequest::encrypted_alert(&config, "jwt", "tok", ApnsEnvironment::Production, &p, 0);
        assert!(req.body.len() <= 4096, "payload {} bytes", req.body.len());
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
    async fn push_service_sends_one_encrypted_alert() {
        let (config, _) = test_config();
        let transport = std::sync::Arc::new(RecordingTransport::default());
        let service = ApnsPushService::with_clock(config, transport.clone(), || 1_752_412_802);
        let outcome = service
            .push_alert("tok_1", ApnsEnvironment::Sandbox, &push(PushClass::Done))
            .await;
        assert_eq!(outcome, PushOutcome::Sent);

        let sent = transport.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].authority, "api.sandbox.push.apple.com");
        assert_eq!(sent[0].path, "/3/device/tok_1");
        let headers = header_map(&sent[0]);
        assert_eq!(headers["apns-push-type"], "alert");
        assert_eq!(
            headers["apns-collapse-id"],
            "6f9a1c2e-3b4d-4e5f-8a7b-1c2d3e4f5a6b"
        );
        assert!(headers["authorization"].starts_with("bearer "));
    }

    #[tokio::test]
    async fn noop_service_reports_not_configured() {
        let outcome = NoopPushService
            .push_alert("tok", ApnsEnvironment::Sandbox, &push(PushClass::Error))
            .await;
        assert_eq!(outcome, PushOutcome::NotConfigured);
        assert_eq!(outcome.label(), "apns_not_configured");
    }

    #[tokio::test]
    async fn bad_key_reports_jwt_failed_and_sends_nothing() {
        let (mut config, _) = test_config();
        config.auth_key_pem = "not a pem".into();
        let transport = std::sync::Arc::new(RecordingTransport::default());
        let service = ApnsPushService::with_clock(config, transport.clone(), || 1);
        let outcome = service
            .push_alert("tok", ApnsEnvironment::Sandbox, &push(PushClass::Error))
            .await;
        assert_eq!(outcome, PushOutcome::JwtFailed);
        assert!(transport.sent.lock().unwrap().is_empty());
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

        let p = push(PushClass::NeedsInput);
        // First push mints.
        service
            .push_alert("tok", ApnsEnvironment::Sandbox, &p)
            .await;
        // A second push a minute later reuses (inside the 20-min window).
        now.store(1_000_000 + 60, Ordering::SeqCst);
        service
            .push_alert("tok", ApnsEnvironment::Sandbox, &p)
            .await;
        // A push past the window re-mints.
        now.store(1_000_000 + JWT_REFRESH_SECS as usize + 1, Ordering::SeqCst);
        service
            .push_alert("tok", ApnsEnvironment::Sandbox, &p)
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

        let outcome = service
            .push_alert("tok", ApnsEnvironment::Sandbox, &push(PushClass::Done))
            .await;

        assert_eq!(outcome, PushOutcome::Sent);
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

        let outcome = service
            .push_alert("tok", ApnsEnvironment::Sandbox, &push(PushClass::Done))
            .await;

        assert_eq!(outcome, PushOutcome::TransientFailed { attempts: 3 });
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

        let p = push(PushClass::NeedsInput);
        let outcome = service
            .push_alert("tok", ApnsEnvironment::Sandbox, &p)
            .await;

        assert_eq!(outcome, PushOutcome::Permanent { purged: true });
        assert_eq!(outcome.label(), "permanent_token_purged");
        assert_eq!(transport.calls(), 1, "permanent failure is not retried");
        assert_eq!(
            *purge.purged.lock().unwrap(),
            vec![p.pairing_id],
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

        let outcome = service
            .push_alert("tok", ApnsEnvironment::Sandbox, &push(PushClass::Error))
            .await;

        assert_eq!(outcome, PushOutcome::Permanent { purged: false });
        assert_eq!(transport.calls(), 1);
    }
}
