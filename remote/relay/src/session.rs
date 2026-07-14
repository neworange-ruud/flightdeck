//! The per-connection state machine driving one `/ws` connection through the
//! spec §5 lifecycle:
//!
//! ```text
//!   AwaitingHello ──hello──▶ AwaitingAuth ──auth_response──▶ Authed
//!        │  (version negotiation)   │  (pairing bootstrap, then           │
//!        │                          │   ECDSA P-256 challenge verify)     │
//!        ▼                          ▼                                     ▼
//!    version_incompatible      pairing_offer / pairing_claim        envelope routing,
//!    (close)                   (self-register key, mint/redeem       ack pruning, resume
//!                               claim token), auth_challenge         replay, presence,
//!                                                                    ping/pong, push tokens
//! ```
//!
//! Wrong-order frames, unknown devices, and bad signatures are answered with a
//! [`RelayFrame::Error`] carrying the appropriate [`RelayErrorCode`] and, for
//! fatal cases (version/auth), the socket is closed. An unauthenticated
//! connection is dropped after `AUTH_TIMEOUT_SECS`.
//!
//! Application payloads ([`EncryptedEnvelope`]) are forwarded to the peer
//! **verbatim**; this module never base64-decodes or otherwise inspects
//! `ciphertext` (PRD §9.1).

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket};
use flightdeck_remote_protocol::{
    negotiate_version, ClientInfo, DeviceId, EncryptedEnvelope, PairingId, PresenceState,
    RelayErrorCode, RelayFrame, Role, MAX_SUPPORTED_VERSION, MIN_SUPPORTED_VERSION,
};
use futures_util::stream::{SplitStream, StreamExt};
use tokio::sync::mpsc;
use tokio::time::{timeout_at, Duration, Instant};

use crate::auth;
use crate::claims::ClaimError;
use crate::ids;
use crate::queue::{AppendOutcome, QueueError};
use crate::router::{peer_role, ConnHandle};
use crate::AppState;

/// Current wall-clock time in unix milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Maximum `pairing_claim` attempts a single connection may make before the
/// relay rate-limits it and closes the socket (spec §5.2 / §12: "rate-limit
/// `pairing_claim`"). A 4-digit claim token has only ~10^4 of entropy, so this
/// per-connection cap — together with the token's short TTL and single-use
/// semantics — keeps online brute force impractical: an attacker must pay a
/// fresh `hello`/`auth_challenge` round trip for every handful of guesses.
const MAX_CLAIM_ATTEMPTS_PER_CONN: u32 = 5;

/// Whether a desktop-supplied `claim_token_hint` is well-formed enough to issue
/// verbatim. Kept deliberately tight: short, printable ASCII, no whitespace —
/// enough for a 4-digit code or the relay's own `NNNN-<hex>` shape, but not an
/// avenue to smuggle arbitrary large strings into the claim table.
fn is_valid_claim_hint(hint: &str) -> bool {
    !hint.is_empty()
        && hint.len() <= 32
        && hint.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// What to do after handling one inbound frame.
enum Flow {
    /// Keep servicing the connection.
    Continue,
    /// Tear the connection down (a fatal error was already reported, or the
    /// peer said goodbye).
    Close,
}

/// The connection's position in the §5 lifecycle.
enum Phase {
    /// Waiting for the opening `hello` (version negotiation).
    AwaitingHello,
    /// `hello_ok` + `auth_challenge` sent; waiting for `auth_response` (with any
    /// number of `pairing_offer` / `pairing_claim` bootstraps in between).
    AwaitingAuth {
        /// Device id declared in `hello`; the eventual `auth_response` must match.
        device_id: DeviceId,
        /// The challenge nonce this connection must sign.
        nonce: [u8; auth::NONCE_LEN],
    },
    /// Authenticated; routing envelopes for `activated` pairings.
    Authed {
        /// The authenticated device.
        device_id: DeviceId,
        /// Pairings this connection is allowed to route (a subset of what it
        /// requested — only those it is actually a member of).
        activated: Vec<PairingId>,
    },
}

/// Drives a single WebSocket connection. Owns the read half; writes go through
/// the bounded outbound channel `out_tx` (drained by the writer task in
/// [`crate::handlers`]).
pub struct Connection {
    state: AppState,
    /// This connection's outbound sender; also cloned into the registry so the
    /// peer can reach us.
    out_tx: mpsc::Sender<RelayFrame>,
    /// Opaque id for logs and registry bookkeeping.
    connection_id: String,
    /// Role declared in `hello` (set once hello is accepted).
    role: Option<Role>,
    /// How many `pairing_claim` frames this connection has attempted, for the
    /// per-connection rate limit ([`MAX_CLAIM_ATTEMPTS_PER_CONN`]).
    claim_attempts: u32,
    phase: Phase,
}

impl Connection {
    /// Create a connection in its initial state.
    pub fn new(state: AppState, out_tx: mpsc::Sender<RelayFrame>) -> Self {
        Self {
            state,
            out_tx,
            connection_id: ids::connection_id(),
            role: None,
            claim_attempts: 0,
            phase: Phase::AwaitingHello,
        }
    }

    /// Run the connection to completion, then clean up (detach from the registry
    /// and announce disconnect presence to any connected peer).
    pub async fn run(mut self, mut reader: SplitStream<WebSocket>) {
        let auth_deadline =
            Instant::now() + Duration::from_secs(self.state.config.auth_timeout_secs);

        loop {
            let authed = matches!(self.phase, Phase::Authed { .. });

            // Only the pre-auth phase is deadline-bounded (spec §5.1).
            let next = if authed {
                reader.next().await
            } else {
                match timeout_at(auth_deadline, reader.next()).await {
                    Ok(msg) => msg,
                    Err(_) => {
                        self.send(RelayFrame::Error {
                            code: RelayErrorCode::NotAuthenticated,
                            message: "authentication timed out".into(),
                            pairing_id: None,
                        })
                        .await;
                        break;
                    }
                }
            };

            let Some(msg) = next else { break };
            let msg = match msg {
                Ok(m) => m,
                Err(_) => break, // transport error / abrupt drop
            };

            let flow = match msg {
                Message::Text(text) => self.on_text(&text).await,
                Message::Binary(_) => {
                    // The relay plane is JSON text only (spec §2).
                    self.send_error(
                        RelayErrorCode::BadFrame,
                        "binary frames are not accepted",
                        None,
                    )
                    .await;
                    Flow::Close
                }
                // The tungstenite stack answers Ping with Pong automatically; a
                // Pong needs no action. A Close begins shutdown.
                Message::Ping(_) | Message::Pong(_) => Flow::Continue,
                Message::Close(_) => Flow::Close,
            };

            if let Flow::Close = flow {
                break;
            }
        }

        self.cleanup().await;
    }

    /// Parse and dispatch one text frame.
    async fn on_text(&mut self, text: &str) -> Flow {
        let frame: RelayFrame = match serde_json::from_str(text) {
            Ok(f) => f,
            Err(_) => {
                self.send_error(RelayErrorCode::BadFrame, "unparseable relay frame", None)
                    .await;
                return Flow::Close;
            }
        };

        // Ping is answered in any phase (latency probe, spec §5.4).
        if let RelayFrame::Ping { client_time_ms } = frame {
            self.send(RelayFrame::Pong {
                client_time_ms,
                server_time_ms: now_ms(),
            })
            .await;
            return Flow::Continue;
        }

        match &self.phase {
            Phase::AwaitingHello => self.on_hello(frame).await,
            Phase::AwaitingAuth { .. } => self.on_pre_auth(frame).await,
            Phase::Authed { .. } => self.on_authed(frame).await,
        }
    }

    // ── AwaitingHello ─────────────────────────────────────────────────────

    async fn on_hello(&mut self, frame: RelayFrame) -> Flow {
        let RelayFrame::Hello {
            protocol_version,
            role,
            device_id,
            client,
        } = frame
        else {
            self.send_error(
                RelayErrorCode::BadFrame,
                "expected `hello` as the first frame",
                None,
            )
            .await;
            return Flow::Close;
        };

        let Some(version) = negotiate_version(
            MIN_SUPPORTED_VERSION,
            MAX_SUPPORTED_VERSION,
            protocol_version,
        ) else {
            self.send(RelayFrame::VersionIncompatible {
                your_version: protocol_version,
                min_supported: MIN_SUPPORTED_VERSION,
                max_supported: MAX_SUPPORTED_VERSION,
            })
            .await;
            return Flow::Close;
        };

        self.role = Some(role);
        let _ = &client as &ClientInfo; // build metadata is diagnostic only

        self.send(RelayFrame::HelloOk {
            protocol_version: version,
            server_time_ms: now_ms(),
            connection_id: self.connection_id.clone(),
        })
        .await;

        // Proactively issue the challenge (spec §5.1: the relay sends
        // auth_challenge right after hello_ok; bootstrap frames may precede the
        // client's auth_response).
        let nonce = auth::random_nonce();
        self.send(RelayFrame::AuthChallenge {
            nonce: auth::encode_b64(&nonce),
            server_time_ms: now_ms(),
        })
        .await;

        self.phase = Phase::AwaitingAuth { device_id, nonce };
        Flow::Continue
    }

    // ── AwaitingAuth ──────────────────────────────────────────────────────

    async fn on_pre_auth(&mut self, frame: RelayFrame) -> Flow {
        match frame {
            RelayFrame::PairingOffer {
                device_id,
                device_public_key,
                key_agreement_public_key,
                role,
                claim_token_hint,
            } => {
                self.on_pairing_offer(
                    device_id,
                    device_public_key,
                    key_agreement_public_key,
                    role,
                    claim_token_hint,
                )
                .await
            }
            RelayFrame::PairingClaim {
                claim_token,
                device_id,
                device_public_key,
                key_agreement_public_key,
                role,
            } => {
                self.on_pairing_claim(
                    claim_token,
                    device_id,
                    device_public_key,
                    key_agreement_public_key,
                    role,
                )
                .await
            }
            RelayFrame::AuthResponse {
                device_id,
                signature,
                pairing_ids,
            } => {
                self.on_auth_response(device_id, signature, pairing_ids)
                    .await
            }
            _ => {
                self.send_error(
                    RelayErrorCode::NotAuthenticated,
                    "frame not allowed before authentication",
                    None,
                )
                .await;
                Flow::Close
            }
        }
    }

    async fn on_pairing_offer(
        &mut self,
        device_id: DeviceId,
        device_public_key: String,
        key_agreement_public_key: String,
        role: Role,
        claim_token_hint: Option<String>,
    ) -> Flow {
        // The offer must come from the connection's own (desktop) identity.
        if !self.hello_device_matches(&device_id) || role != Role::Desktop {
            self.send_error(RelayErrorCode::BadFrame, "invalid pairing_offer", None)
                .await;
            return Flow::Close;
        }
        if auth::parse_public_key(&device_public_key).is_err() {
            self.send_error(
                RelayErrorCode::BadFrame,
                "malformed device public key",
                None,
            )
            .await;
            return Flow::Close;
        }
        // The KA key is not secret and is never used for signature verification,
        // but it must be a well-formed P-256 SEC1 point so the peer can ECDH.
        if auth::parse_public_key(&key_agreement_public_key).is_err() {
            self.send_error(
                RelayErrorCode::BadFrame,
                "malformed key-agreement public key",
                None,
            )
            .await;
            return Flow::Close;
        }

        // Self-register the desktop's identity + key-agreement keys so it can
        // authenticate right after and the phone can later receive the KA key.
        self.state
            .store
            .register_device(device_id.clone(), device_public_key)
            .await;
        self.state
            .store
            .register_key_agreement_key(device_id.clone(), key_agreement_public_key)
            .await;
        let pairing_id = self.state.store.create_pairing(device_id).await;

        let ttl = self.state.config.claim_token_ttl_secs as i64 * 1000;
        let expires_at_ms = now_ms() + ttl;
        // Honor a well-formed, currently-free `claim_token_hint` so the desktop
        // can show a short 4-digit code; otherwise mint a random token. Either
        // way the desktop displays the token we return here (spec §5.2).
        let token = match claim_token_hint {
            Some(hint)
                if is_valid_claim_hint(&hint)
                    && self.state.store.claim_token_is_free(&hint).await =>
            {
                hint
            }
            _ => ids::claim_token(),
        };
        // NB: the desktop peer id is recorded so the phone's claim can report it.
        let members = self
            .state
            .store
            .pairing_members(&pairing_id)
            .await
            .expect("pairing just created");
        self.state
            .store
            .issue_claim(
                token.clone(),
                pairing_id.clone(),
                members.desktop,
                expires_at_ms,
            )
            .await;

        // If the desktop is **already authenticated** (an on-demand pairing from
        // Settings → Remote, rather than a pre-auth bootstrap), activate and
        // attach the new pairing on this live connection right away. Without
        // this the desktop would not be in the routing table for the new
        // pairing, so the phone's later `pairing_claim` could not notify it and
        // envelopes would be rejected as inactive — the alternative being a full
        // reconnect just to pair. Pre-auth offers need nothing here: the pairing
        // is activated normally via the pending `auth_response`.
        if let Phase::Authed { activated, .. } = &mut self.phase {
            if !activated.contains(&pairing_id) {
                activated.push(pairing_id.clone());
            }
            let handle = ConnHandle {
                connection_id: self.connection_id.clone(),
                tx: self.out_tx.clone(),
            };
            self.state
                .registry
                .attach(&pairing_id, Role::Desktop, handle);
        }

        self.send(RelayFrame::PairingOfferOk {
            pairing_id,
            claim_token: token,
            expires_at_ms,
        })
        .await;
        Flow::Continue
    }

    async fn on_pairing_claim(
        &mut self,
        claim_token: String,
        device_id: DeviceId,
        device_public_key: String,
        key_agreement_public_key: String,
        role: Role,
    ) -> Flow {
        // Per-connection rate limit: cap online brute force of the short
        // (4-digit) claim token (spec §5.2 / §12). Once over the cap the relay
        // stops even looking the token up and closes the socket.
        self.claim_attempts += 1;
        if self.claim_attempts > MAX_CLAIM_ATTEMPTS_PER_CONN {
            self.send_error(
                RelayErrorCode::RateLimited,
                "too many pairing_claim attempts on this connection",
                None,
            )
            .await;
            return Flow::Close;
        }

        if !self.hello_device_matches(&device_id) || role != Role::Phone {
            self.send_error(RelayErrorCode::BadFrame, "invalid pairing_claim", None)
                .await;
            return Flow::Close;
        }
        if auth::parse_public_key(&device_public_key).is_err() {
            self.send_error(
                RelayErrorCode::BadFrame,
                "malformed device public key",
                None,
            )
            .await;
            return Flow::Close;
        }
        if auth::parse_public_key(&key_agreement_public_key).is_err() {
            self.send_error(
                RelayErrorCode::BadFrame,
                "malformed key-agreement public key",
                None,
            )
            .await;
            return Flow::Close;
        }

        let claim = match self.state.store.redeem_claim(&claim_token, now_ms()).await {
            Ok(c) => c,
            Err(ClaimError::Unknown | ClaimError::Expired) => {
                // Advisory, non-fatal: the user can re-enter a fresh code.
                self.send_error(
                    RelayErrorCode::PairingClaimRejected,
                    "claim token invalid or expired",
                    None,
                )
                .await;
                return Flow::Continue;
            }
        };

        // Register the phone's identity + key-agreement keys and attach it to
        // the pairing.
        self.state
            .store
            .register_device(device_id.clone(), device_public_key)
            .await;
        self.state
            .store
            .register_key_agreement_key(device_id.clone(), key_agreement_public_key)
            .await;
        let desktop_device = match self
            .state
            .store
            .add_phone_to_pairing(&claim.pairing_id, device_id.clone())
            .await
        {
            Ok(d) => d,
            Err(_) => {
                self.send_error(
                    RelayErrorCode::UnknownPairing,
                    "pairing no longer exists",
                    Some(claim.pairing_id.clone()),
                )
                .await;
                return Flow::Continue;
            }
        };

        // The phone needs the desktop's KA key; the desktop needs the phone's.
        // Both were self-registered during their respective bootstrap frames.
        let desktop_ka_key = self
            .state
            .store
            .device_key_agreement_key(&desktop_device)
            .await;
        let phone_ka_key = self.state.store.device_key_agreement_key(&device_id).await;

        // Tell the phone which pairing it joined, who its peer is, and the
        // desktop's key-agreement public key for the E2E ECDH (spec §7.1).
        self.send(RelayFrame::PairingClaimed {
            pairing_id: claim.pairing_id.clone(),
            peer_device_id: Some(desktop_device),
            peer_key_agreement_public_key: desktop_ka_key,
        })
        .await;

        // Notify the waiting desktop connection, if it is currently connected,
        // that a phone has joined this pairing, and hand it the phone's KA key.
        if let Some(desktop) = self.state.registry.peer(&claim.pairing_id, Role::Phone) {
            desktop
                .send(RelayFrame::PairingClaimed {
                    pairing_id: claim.pairing_id,
                    peer_device_id: Some(device_id),
                    peer_key_agreement_public_key: phone_ka_key,
                })
                .await;
        }
        Flow::Continue
    }

    async fn on_auth_response(
        &mut self,
        device_id: DeviceId,
        signature: String,
        pairing_ids: Vec<PairingId>,
    ) -> Flow {
        let Phase::AwaitingAuth {
            device_id: hello_device,
            nonce,
        } = &self.phase
        else {
            unreachable!("on_auth_response only called in AwaitingAuth");
        };

        if device_id != *hello_device {
            self.send_error(
                RelayErrorCode::AuthFailed,
                "auth_response device_id does not match hello",
                None,
            )
            .await;
            return Flow::Close;
        }

        // Verify possession of the registered private key.
        let Some(public_key) = self.state.store.device_public_key(&device_id).await else {
            self.send_error(RelayErrorCode::AuthFailed, "unknown device", None)
                .await;
            return Flow::Close;
        };
        if auth::verify_challenge(&public_key, nonce, &signature).is_err() {
            self.send_error(
                RelayErrorCode::AuthFailed,
                "signature verification failed",
                None,
            )
            .await;
            return Flow::Close;
        }

        // Activate only pairings this device is actually a member of.
        let mut activated = Vec::new();
        for pairing in pairing_ids {
            if let Some(members) = self.state.store.pairing_members(&pairing).await {
                if members.contains(&device_id) {
                    activated.push(pairing);
                }
            }
        }

        self.send(RelayFrame::AuthOk {
            pairing_ids: activated.clone(),
        })
        .await;

        // Attach each activated pairing to the live routing table and exchange
        // presence with any already-connected peer.
        let role = self.role.expect("role set at hello");
        let handle = ConnHandle {
            connection_id: self.connection_id.clone(),
            tx: self.out_tx.clone(),
        };
        for pairing in &activated {
            if let Some(peer) = self.state.registry.attach(pairing, role, handle.clone()) {
                // We can see the peer …
                self.send(RelayFrame::PeerPresence {
                    pairing_id: pairing.clone(),
                    peer: peer_role(role),
                    state: PresenceState::Connected,
                    at_ms: now_ms(),
                })
                .await;
                // … and the peer can see us.
                peer.send(RelayFrame::PeerPresence {
                    pairing_id: pairing.clone(),
                    peer: role,
                    state: PresenceState::Connected,
                    at_ms: now_ms(),
                })
                .await;
            }
        }

        self.phase = Phase::Authed {
            device_id,
            activated,
        };
        Flow::Continue
    }

    // ── Authed ────────────────────────────────────────────────────────────

    async fn on_authed(&mut self, frame: RelayFrame) -> Flow {
        match frame {
            RelayFrame::Envelope(env) => self.on_envelope(env).await,
            RelayFrame::Ack { pairing_id, cursor } => self.on_ack(pairing_id, cursor).await,
            RelayFrame::Resume {
                pairing_id,
                from_seq,
            } => self.on_resume(pairing_id, from_seq).await,
            RelayFrame::RegisterPushToken {
                pairing_id,
                token,
                environment,
            } => {
                if !self.is_activated(&pairing_id) {
                    self.send_error(
                        RelayErrorCode::UnknownPairing,
                        "pairing not active",
                        Some(pairing_id),
                    )
                    .await;
                    return Flow::Continue;
                }
                self.state
                    .store
                    .register_push_token(pairing_id.clone(), token, environment)
                    .await;
                self.send(RelayFrame::PushTokenAck { pairing_id }).await;
                Flow::Continue
            }
            // A desktop already authed may offer additional pairings (e.g. to
            // pair another phone) without reconnecting.
            RelayFrame::PairingOffer {
                device_id,
                device_public_key,
                key_agreement_public_key,
                role,
                claim_token_hint,
            } => {
                self.on_pairing_offer(
                    device_id,
                    device_public_key,
                    key_agreement_public_key,
                    role,
                    claim_token_hint,
                )
                .await
            }
            RelayFrame::Bye { .. } => Flow::Close,
            _ => {
                self.send_error(RelayErrorCode::BadFrame, "unexpected frame", None)
                    .await;
                Flow::Continue
            }
        }
    }

    async fn on_envelope(&mut self, env: EncryptedEnvelope) -> Flow {
        let role = self.role.expect("role set at hello");
        if !self.is_activated(&env.pairing_id) {
            self.send_error(
                RelayErrorCode::UnknownPairing,
                "envelope for an inactive pairing",
                Some(env.pairing_id.clone()),
            )
            .await;
            return Flow::Continue;
        }
        if env.sender != role {
            self.send_error(
                RelayErrorCode::BadFrame,
                "envelope sender does not match connection role",
                Some(env.pairing_id.clone()),
            )
            .await;
            return Flow::Continue;
        }

        let pairing_id = env.pairing_id.clone();
        match self.state.store.enqueue(env.clone()).await {
            Err(QueueError::SeqViolation { .. }) => {
                self.send_error(
                    RelayErrorCode::BadFrame,
                    "envelope seq is not gapless/monotonic",
                    Some(pairing_id),
                )
                .await;
            }
            Ok(AppendOutcome::Duplicate) => {
                // Already held; the peer has it (or will via replay). Drop.
            }
            Ok(AppendOutcome::Accepted { overflow }) => {
                if overflow {
                    // Advisory back-pressure: oldest un-acked envelope shed.
                    self.send_error(
                        RelayErrorCode::RateLimited,
                        "pairing queue overflow: oldest envelope dropped",
                        Some(pairing_id.clone()),
                    )
                    .await;
                }
                // Forward verbatim to the peer if connected (ciphertext never
                // inspected). Offline peers pick it up via resume/replay.
                if let Some(peer) = self.state.registry.peer(&pairing_id, role) {
                    peer.send(RelayFrame::Envelope(env)).await;
                }
            }
        }
        Flow::Continue
    }

    async fn on_ack(&mut self, pairing_id: PairingId, cursor: u64) -> Flow {
        let role = self.role.expect("role set at hello");
        if !self.is_activated(&pairing_id) {
            self.send_error(
                RelayErrorCode::UnknownPairing,
                "ack for an inactive pairing",
                Some(pairing_id),
            )
            .await;
            return Flow::Continue;
        }
        // An ack acknowledges the *peer's* outbound stream.
        self.state
            .store
            .ack(&pairing_id, peer_role(role), cursor)
            .await;
        Flow::Continue
    }

    async fn on_resume(&mut self, pairing_id: PairingId, from_seq: u64) -> Flow {
        let role = self.role.expect("role set at hello");
        if !self.is_activated(&pairing_id) {
            self.send_error(
                RelayErrorCode::UnknownPairing,
                "resume for an inactive pairing",
                Some(pairing_id),
            )
            .await;
            return Flow::Continue;
        }
        // Replay the peer's queued envelopes with seq > from_seq, in order.
        let replay = self
            .state
            .store
            .replay(&pairing_id, peer_role(role), from_seq)
            .await;
        for env in replay {
            self.send(RelayFrame::Envelope(env)).await;
        }
        Flow::Continue
    }

    // ── helpers ───────────────────────────────────────────────────────────

    fn hello_device_matches(&self, device_id: &DeviceId) -> bool {
        match &self.phase {
            Phase::AwaitingAuth {
                device_id: hello, ..
            } => hello == device_id,
            Phase::Authed {
                device_id: authed, ..
            } => authed == device_id,
            Phase::AwaitingHello => false,
        }
    }

    fn is_activated(&self, pairing_id: &PairingId) -> bool {
        matches!(&self.phase, Phase::Authed { activated, .. } if activated.contains(pairing_id))
    }

    async fn send(&self, frame: RelayFrame) {
        // A full/closed outbound channel means the writer is gone; the read
        // loop will notice the connection ending on its next turn.
        let _ = self.out_tx.send(frame).await;
    }

    async fn send_error(&self, code: RelayErrorCode, message: &str, pairing_id: Option<PairingId>) {
        self.send(RelayFrame::Error {
            code,
            message: message.to_string(),
            pairing_id,
        })
        .await;
    }

    /// Detach from the registry and announce a disconnect to connected peers.
    async fn cleanup(&mut self) {
        let (role, activated) = match (&self.role, &self.phase) {
            (Some(role), Phase::Authed { activated, .. }) => (*role, activated.clone()),
            _ => return, // never authenticated → nothing attached
        };
        for pairing in &activated {
            if let Some(peer) = self
                .state
                .registry
                .detach(pairing, role, &self.connection_id)
            {
                peer.send(RelayFrame::PeerPresence {
                    pairing_id: pairing.clone(),
                    peer: role,
                    state: PresenceState::Disconnected,
                    at_ms: now_ms(),
                })
                .await;
            }
        }
    }
}
