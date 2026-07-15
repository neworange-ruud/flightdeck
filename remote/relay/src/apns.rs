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

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use flightdeck_remote_protocol::{AgentEvent, ApnsEnvironment, EventKind, PairingId};
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use p256::pkcs8::DecodePrivateKey;

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
        let team_id = std::env::var("APNS_TEAM_ID").ok().filter(|s| !s.is_empty())?;
        let key_id = std::env::var("APNS_KEY_ID").ok().filter(|s| !s.is_empty())?;
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
            let ready = if *ready_to_push { " · ready to push" } else { "" };
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
        let mut headers = base_headers(config, jwt, "alert", "10");
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
    pub fn background_wake(
        config: &ApnsConfig,
        jwt: &str,
        device_token: &str,
        environment: ApnsEnvironment,
    ) -> Self {
        let body = serde_json::json!({ "aps": { "content-available": 1 } });
        Self {
            authority: host(environment).to_string(),
            path: format!("/3/device/{device_token}"),
            headers: base_headers(config, jwt, "background", "5"),
            body: serde_json::to_vec(&body).expect("json serialization is infallible here"),
        }
    }
}

/// The headers common to every APNs request: bearer auth, topic, push type,
/// priority, and a zero expiration (deliver-once, don't store).
fn base_headers(config: &ApnsConfig, jwt: &str, push_type: &str, priority: &str) -> Vec<(String, String)> {
    vec![
        ("authorization".to_string(), format!("bearer {jwt}")),
        ("apns-topic".to_string(), config.topic.clone()),
        ("apns-push-type".to_string(), push_type.to_string()),
        ("apns-priority".to_string(), priority.to_string()),
        ("apns-expiration".to_string(), "0".to_string()),
    ]
}

/// The seam that actually puts an [`ApnsRequest`] on the wire. The real
/// HTTP/2-over-TLS implementation is compiled only under the `apns-live`
/// feature (it needs Apple credentials to be useful); tests inject a recording
/// double.
#[async_trait]
pub trait ApnsTransport: Send + Sync {
    /// Deliver one request to APNs. Best-effort: a failure is logged by the
    /// caller and never propagated into the connection state machine (a missed
    /// wake push is recovered by the phone's own reconnect).
    async fn send(&self, request: ApnsRequest) -> Result<(), String>;
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
    async fn notify_offline(&self, _pairing: &PairingId, _token: &str, _environment: ApnsEnvironment) {}
}

/// The live [`PushService`]: mints a JWT and sends a background-wake push
/// through the injected [`ApnsTransport`].
pub struct ApnsPushService<T: ApnsTransport> {
    config: ApnsConfig,
    transport: T,
    now_secs: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl<T: ApnsTransport> ApnsPushService<T> {
    /// Build a push service over `transport` with the default wall-clock.
    pub fn new(config: ApnsConfig, transport: T) -> Self {
        Self {
            config,
            transport,
            now_secs: Box::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            }),
        }
    }

    /// Build with an injected clock (tests).
    pub fn with_clock(
        config: ApnsConfig,
        transport: T,
        now_secs: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            config,
            transport,
            now_secs: Box::new(now_secs),
        }
    }
}

#[async_trait]
impl<T: ApnsTransport> PushService for ApnsPushService<T> {
    async fn notify_offline(&self, pairing: &PairingId, token: &str, environment: ApnsEnvironment) {
        let jwt = match build_jwt(&self.config, (self.now_secs)()) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(?e, %pairing, "apns: could not mint JWT; skipping wake push");
                return;
            }
        };
        let request = ApnsRequest::background_wake(&self.config, &jwt, token, environment);
        if let Err(err) = self.transport.send(request).await {
            // Best-effort: the phone's own reconnect recovers a missed wake.
            tracing::warn!(%err, %pairing, "apns: wake push failed");
        }
    }
}

/// The real HTTP/2-over-TLS APNs transport. Compiled only with `apns-live`
/// because exercising it requires Apple credentials + network egress to
/// `api.push.apple.com` — the deployment's manual step. It is a thin adapter:
/// all request *shape* (JWT, headers, body, path) is built and tested above.
#[cfg(feature = "apns-live")]
pub mod live {
    use super::{ApnsRequest, ApnsTransport};
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
        async fn send(&self, request: ApnsRequest) -> Result<(), String> {
            let url = format!("https://{}{}", request.authority, request.path);
            let mut req = self.client.post(url).body(request.body);
            for (name, value) in request.headers {
                req = req.header(name, value);
            }
            let resp = req.send().await.map_err(|e| e.to_string())?;
            if resp.status().is_success() {
                Ok(())
            } else {
                Err(format!("apns responded {}", resp.status()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flightdeck_remote_protocol::{DeepLink, EventId, ItemId, ProjectId, SessionId};
    use p256::ecdsa::{signature::Verifier, VerifyingKey};
    use p256::pkcs8::EncodePrivateKey;
    use p256::SecretKey;
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
        assert!(verifying.verify(signing_input.as_bytes(), &signature).is_ok());
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
        assert_eq!(content.body, "18 files changed · ready to push · SpecAssistant");
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
        );
        assert_eq!(req.authority, "api.push.apple.com");
        let headers: std::collections::HashMap<_, _> = req.headers.iter().cloned().collect();
        assert_eq!(headers["apns-push-type"], "background");
        assert_eq!(headers["apns-priority"], "5");
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["aps"]["content-available"], 1);
        assert!(body.get("deep_link").is_none(), "zero-knowledge: no content");
    }

    /// Recording transport: captures every request so tests can assert on the
    /// live push service's behavior without any network or Apple secret.
    #[derive(Default)]
    struct RecordingTransport {
        sent: Mutex<Vec<ApnsRequest>>,
    }

    #[async_trait]
    impl ApnsTransport for RecordingTransport {
        async fn send(&self, request: ApnsRequest) -> Result<(), String> {
            self.sent.lock().unwrap().push(request);
            Ok(())
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
}

// `ApnsTransport` is object-safe and used behind `Arc<dyn ...>`; provide the
// impl so an `Arc<T: ApnsTransport>` satisfies the trait in wiring/tests.
#[async_trait]
impl<T: ApnsTransport + ?Sized> ApnsTransport for std::sync::Arc<T> {
    async fn send(&self, request: ApnsRequest) -> Result<(), String> {
        (**self).send(request).await
    }
}
