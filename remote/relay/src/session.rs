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

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket};
use flightdeck_remote_protocol::{
    negotiate_version, ClientInfo, DeviceId, EncryptedEnvelope, PairingId, PresenceState,
    RelayErrorCode, RelayFrame, Role, MAX_SUPPORTED_VERSION, MIN_SUPPORTED_VERSION,
};
use futures_util::stream::{SplitStream, StreamExt};
use tokio::sync::{mpsc, Notify};
use tokio::time::{timeout_at, Duration, Instant};

use crate::auth;
use crate::claims::ClaimError;
use crate::ids;
use crate::queue::{AppendOutcome, QueueError};
use crate::router::{peer_role, ConnHandle, TrySendOutcome};
use crate::store::RevokeOutcome;
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

/// Maximum number of characters the relay will store/forward for a machine name
/// (spec §10.1). The name is untrusted display text; length-bounding it keeps a
/// misbehaving desktop from smuggling an oversized string through the relay to
/// the phone. The phone additionally sanitizes it before display.
const MAX_MACHINE_NAME_CHARS: usize = 64;

/// Length-bound and normalize an announced machine name: trim surrounding
/// whitespace, truncate to [`MAX_MACHINE_NAME_CHARS`] **characters** (never
/// splitting a UTF-8 scalar), and treat an empty result as "no name" (`None`).
/// Content sanitation for display is the phone's responsibility.
fn bound_machine_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(MAX_MACHINE_NAME_CHARS).collect())
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
    /// Fires when a newer leg supersedes one of this connection's slots in the
    /// registry, telling the read loop to tear down (remote-control-0ef.8).
    /// The same `Arc` is cloned into every [`ConnHandle`] this connection
    /// registers, so superseding any leg wakes the whole connection.
    shutdown: Arc<Notify>,
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
            shutdown: Arc::new(Notify::new()),
            phase: Phase::AwaitingHello,
        }
    }

    /// Build a registry handle for this connection: its outbound sender plus the
    /// shared shutdown signal, so superseding this handle later can tear the
    /// connection down (remote-control-0ef.8).
    fn handle(&self) -> ConnHandle {
        ConnHandle {
            connection_id: self.connection_id.clone(),
            tx: self.out_tx.clone(),
            shutdown: self.shutdown.clone(),
        }
    }

    /// Run the connection to completion, then clean up (detach from the registry
    /// and announce disconnect presence to any connected peer).
    pub async fn run(mut self, mut reader: SplitStream<WebSocket>) {
        let auth_deadline =
            Instant::now() + Duration::from_secs(self.state.config.auth_timeout_secs);
        let idle_timeout = self.state.config.idle_timeout;
        // Liveness clock: reset on every inbound frame. An authenticated
        // connection that goes silent past `idle_timeout` is torn down
        // (remote-control-0ef.1). Server-initiated WS pings (driven by the
        // writer task) keep a healthy-but-quiet client sending Pongs, which
        // count as inbound traffic and keep resetting this.
        let mut last_inbound = Instant::now();

        loop {
            let authed = matches!(self.phase, Phase::Authed { .. });

            let next = if authed {
                // Post-auth: liveness-bounded (remote-control-0ef.1) and
                // preemptible by supersession (remote-control-0ef.8).
                let idle_deadline = last_inbound + idle_timeout;
                tokio::select! {
                    biased;
                    _ = self.shutdown.notified() => {
                        tracing::info!(
                            conn = %self.connection_id,
                            "superseded by a newer connection for the same role; shutting down"
                        );
                        break;
                    }
                    res = timeout_at(idle_deadline, reader.next()) => match res {
                        Ok(msg) => msg,
                        Err(_) => {
                            tracing::info!(
                                conn = %self.connection_id,
                                "idle timeout: no inbound frame within the liveness deadline; tearing down"
                            );
                            break;
                        }
                    },
                }
            } else {
                // Pre-auth is bounded only by the auth deadline (spec §5.1); an
                // unauthenticated connection holds no registry slot, so it can
                // neither be superseded nor needs the idle sweep.
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

            // Any inbound frame — an app frame or the Pong answering a server
            // ping — proves the peer is alive; reset the liveness clock.
            last_inbound = Instant::now();

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

        tracing::info!(conn = %self.connection_id, ?role, ?device_id, version, "hello accepted");
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
                machine_name,
            } => {
                self.on_auth_response(device_id, signature, pairing_ids, machine_name)
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
            let handle = self.handle();
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
            Ok(c) => {
                tracing::info!(conn = %self.connection_id, token = %claim_token, pairing = ?c.pairing_id, "pairing_claim OK (token redeemed)");
                c
            }
            Err(ClaimError::Unknown | ClaimError::Expired) => {
                tracing::info!(conn = %self.connection_id, token = %claim_token, "pairing_claim REJECTED (token unknown or expired)");
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
        machine_name: Option<String>,
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
            tracing::info!(conn = %self.connection_id, ?device_id, "auth FAILED: unknown device (no offer/claim ever registered this key)");
            self.send_error(RelayErrorCode::AuthFailed, "unknown device", None)
                .await;
            return Flow::Close;
        };
        if auth::verify_challenge(&public_key, nonce, &signature).is_err() {
            tracing::info!(conn = %self.connection_id, ?device_id, "auth FAILED: signature verification failed");
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

        tracing::info!(conn = %self.connection_id, ?device_id, activated = activated.len(), "auth OK (auth_ok sent)");
        self.send(RelayFrame::AuthOk {
            pairing_ids: activated.clone(),
        })
        .await;

        // Attach each activated pairing to the live routing table and exchange
        // presence with any already-connected peer.
        let role = self.role.expect("role set at hello");
        let handle = self.handle();
        for pairing in &activated {
            let peer = self.state.registry.attach(pairing, role, handle.clone());
            tracing::info!(
                conn = %self.connection_id, pairing = ?pairing, role = ?role,
                peer_present = peer.is_some(), "DIAG leg ATTACHED (auth_ok)"
            );
            if let Some(peer) = peer {
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

        // Machine-name exchange (spec §10.1). A desktop announces its name on
        // *every* connect; the relay stores it per activated pairing and forwards
        // it to a connected phone (so a Mac rename propagates on reconnect). When
        // a phone authenticates, it receives each activated pairing's last-known
        // name so its per-pairing default is fresh.
        match role {
            Role::Desktop => {
                if let Some(name) = machine_name.as_deref().and_then(bound_machine_name) {
                    for pairing in &activated {
                        self.state
                            .store
                            .set_machine_name(pairing, name.clone())
                            .await;
                        if let Some(phone) = self.state.registry.peer(pairing, Role::Desktop) {
                            phone
                                .send(RelayFrame::MachineName {
                                    pairing_id: pairing.clone(),
                                    machine_name: name.clone(),
                                })
                                .await;
                        }
                    }
                }
            }
            Role::Phone => {
                for pairing in &activated {
                    if let Some(name) = self.state.store.machine_name(pairing).await {
                        self.send(RelayFrame::MachineName {
                            pairing_id: pairing.clone(),
                            machine_name: name,
                        })
                        .await;
                    }
                }
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
            RelayFrame::UnregisterPushToken { pairing_id } => {
                self.on_unregister_push_token(pairing_id).await
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
            RelayFrame::Revoke { pairing_id } => self.on_revoke(pairing_id).await,
            RelayFrame::Bye { .. } => Flow::Close,
            _ => {
                self.send_error(RelayErrorCode::BadFrame, "unexpected frame", None)
                    .await;
                Flow::Continue
            }
        }
    }

    /// Handle a phone-initiated (or member-initiated) unpair (spec §10.2).
    ///
    /// The store verifies membership and removes the pairing + all its state
    /// atomically. Only a member may revoke; a non-member is refused. Revocation
    /// is idempotent. On success the relay drops the pairing from the live
    /// routing table, notifies the former peer, confirms to the requester, and
    /// deactivates it on this connection — leaving the device's other pairings
    /// untouched.
    async fn on_revoke(&mut self, pairing_id: PairingId) -> Flow {
        let device_id = match &self.phase {
            Phase::Authed { device_id, .. } => device_id.clone(),
            _ => unreachable!("on_revoke only called in Authed"),
        };
        let role = self.role.expect("role set at hello");

        match self
            .state
            .store
            .revoke_pairing(&pairing_id, &device_id)
            .await
        {
            RevokeOutcome::Removed(_members) => {
                // Notify the former peer (if connected) before tearing the slot
                // down, so a desktop learns its phone unpaired it.
                if let Some(peer) = self.state.registry.peer(&pairing_id, role) {
                    peer.send(RelayFrame::PairingRevoked {
                        pairing_id: pairing_id.clone(),
                    })
                    .await;
                }
                self.state.registry.remove(&pairing_id);
                // Deactivate on this connection so later frames for it are
                // rejected as unknown.
                if let Phase::Authed { activated, .. } = &mut self.phase {
                    activated.retain(|p| p != &pairing_id);
                }
                // Confirm back to the requester (idempotent success).
                self.send(RelayFrame::PairingRevoked { pairing_id }).await;
                Flow::Continue
            }
            RevokeOutcome::AlreadyGone => {
                // Idempotent no-op success: still confirm to the requester.
                self.send(RelayFrame::PairingRevoked { pairing_id }).await;
                Flow::Continue
            }
            RevokeOutcome::NotMember => {
                // Security refusal (spec §10.2): the authenticated device is not
                // a member of this pairing. Advisory, non-fatal.
                self.send_error(
                    RelayErrorCode::UnknownPairing,
                    "not a member of this pairing",
                    Some(pairing_id),
                )
                .await;
                Flow::Continue
            }
        }
    }

    /// Handle a phone-initiated push-token deregistration (spec §5.5).
    ///
    /// Removes the pairing's APNs token **without** unpairing, so a phone can mute
    /// this pairing's pushes and keep the pairing. Mirrors [`Self::on_revoke`]'s
    /// membership invariant: only a member of `pairing_id` may unregister its
    /// token; a non-member (or an unknown pairing) is refused with
    /// `unknown_pairing` and nothing changes. Removal is idempotent — a member
    /// unregistering when no token is stored still succeeds with `push_token_ack`.
    async fn on_unregister_push_token(&mut self, pairing_id: PairingId) -> Flow {
        let device_id = match &self.phase {
            Phase::Authed { device_id, .. } => device_id.clone(),
            _ => unreachable!("on_unregister_push_token only called in Authed"),
        };

        // Membership check (same invariant the revoke path enforces): only a
        // member of the pairing may touch its push token. An unknown pairing has
        // no members, so it is refused the same way.
        let is_member = self
            .state
            .store
            .pairing_members(&pairing_id)
            .await
            .is_some_and(|m| m.contains(&device_id));
        if !is_member {
            self.send_error(
                RelayErrorCode::UnknownPairing,
                "not a member of this pairing",
                Some(pairing_id),
            )
            .await;
            return Flow::Continue;
        }

        // Idempotent: removing an absent token is a success no-op.
        self.state.store.unregister_push_token(&pairing_id).await;
        self.send(RelayFrame::PushTokenAck { pairing_id }).await;
        Flow::Continue
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
            Err(QueueError::SeqViolation { expected, got }) => {
                tracing::info!(
                    conn = %self.connection_id, pairing = ?pairing_id, role = ?role,
                    expected, got, "DIAG envelope REJECTED (seq_violation)"
                );
                // Recoverable, not a client bug: the endpoint's outbound cursor
                // is ahead of our (possibly restart-reset, in-memory) watermark.
                // Signal `SeqViolation` so the sender re-syncs instead of looping
                // on a fatal reconnect (remote-control-bbf).
                self.send_error(
                    RelayErrorCode::SeqViolation,
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
                // inspected). Offline phones pick it up via resume/replay — and
                // are woken by an APNs push so they reconnect promptly.
                deliver_or_push(&self.state, &env, role).await;
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
            tracing::info!(
                conn = %self.connection_id, pairing = ?pairing, role = ?role,
                "DIAG leg DETACHED (connection closed)"
            );
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

/// Deliver an accepted envelope to its peer, or — when the peer is an
/// **offline phone** — fire an APNs wake push so the phone reconnects and
/// `resume`s the queued envelope (spec §11 step 1).
///
/// Never inspects `ciphertext` (PRD §9.1): the peer receives the envelope
/// verbatim, and the push carries no user content (see [`crate::apns`], which
/// keeps the relay's zero-knowledge guarantee). Only a **phone** is ever
/// push-addressed — the desktop holds its own persistent outbound connection
/// and does not receive pushes.
async fn deliver_or_push(state: &AppState, env: &EncryptedEnvelope, sender: Role) {
    if let Some(peer) = state.registry.peer(&env.pairing_id, sender) {
        // Non-blocking forward (remote-control-0ef.6). A slow or half-open peer
        // whose outbound channel is full (or already closed) must NOT be awaited
        // here: awaiting would let that stuck receiver back-pressure the healthy
        // *sender's* read loop through this path, freezing a working leg. On
        // `Full`/`Closed` we drop the live forward — which is safe because the
        // envelope is already durably buffered in the store's `SenderQueue`
        // (this runs only after a successful `enqueue`). When the receiver next
        // notices the resulting seq gap, or reconnects, it issues
        // `resume { from_seq }` and pulls the dropped envelope from the buffer —
        // the same recovery the SeqViolation→resync path relies on. A healthy
        // peer never hits this: it has 256 frames of head room and drains
        // promptly.
        match peer.try_send(RelayFrame::Envelope(env.clone())) {
            TrySendOutcome::Sent => {
                tracing::info!(
                    pairing = ?env.pairing_id, sender = ?sender, seq = env.seq,
                    peer_conn = %peer.connection_id,
                    "DIAG deliver -> peer connected (forwarded)"
                );
            }
            TrySendOutcome::Full => {
                tracing::warn!(
                    pairing = ?env.pairing_id, sender = ?sender, seq = env.seq,
                    peer_conn = %peer.connection_id,
                    "peer outbound channel full (slow/half-open); dropped live forward, \
                     receiver will resume from the queue"
                );
            }
            TrySendOutcome::Closed => {
                tracing::info!(
                    pairing = ?env.pairing_id, sender = ?sender, seq = env.seq,
                    peer_conn = %peer.connection_id,
                    "peer writer gone; dropped live forward, receiver will resume from the queue"
                );
            }
        }
        return;
    }
    tracing::info!(
        pairing = ?env.pairing_id, sender = ?sender, seq = env.seq,
        "DIAG deliver -> NO PEER attached (envelope queued; push if desktop)"
    );
    if sender == Role::Desktop {
        if let Some((token, apns_env)) = state.store.push_token(&env.pairing_id).await {
            state
                .push
                .notify_offline(&env.pairing_id, &token, apns_env)
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apns::PushService;
    use crate::config::{Config, LogFormat};
    use crate::router::ConnHandle;
    use crate::AppState;
    use async_trait::async_trait;
    use flightdeck_remote_protocol::ApnsEnvironment;
    use std::sync::Arc;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    /// Records every offline-wake request so tests can assert whether — and for
    /// which pairing/token/env — a push was fired.
    #[derive(Default)]
    struct RecordingPush {
        fired: Mutex<Vec<(PairingId, String, ApnsEnvironment)>>,
    }

    #[async_trait]
    impl PushService for RecordingPush {
        async fn notify_offline(
            &self,
            pairing: &PairingId,
            token: &str,
            environment: ApnsEnvironment,
        ) {
            self.fired
                .lock()
                .unwrap()
                .push((pairing.clone(), token.to_string(), environment));
        }
    }

    fn state_with_push(push: Arc<RecordingPush>) -> AppState {
        AppState::with_push(Config::new(0, LogFormat::Pretty, "test"), push)
    }

    fn envelope(pairing: &str, sender: Role, seq: u64) -> EncryptedEnvelope {
        EncryptedEnvelope {
            pairing_id: PairingId::new(pairing),
            seq,
            sender,
            sent_at_ms: 0,
            nonce: "bm9uY2U=".into(),
            ciphertext: "opaque".into(),
        }
    }

    fn conn_handle(id: &str) -> (ConnHandle, mpsc::Receiver<RelayFrame>) {
        conn_handle_bounded(id, 8)
    }

    /// Like [`conn_handle`] but with an explicit channel bound, so a test can
    /// jam a peer's outbox to simulate a slow/stuck receiver.
    fn conn_handle_bounded(id: &str, bound: usize) -> (ConnHandle, mpsc::Receiver<RelayFrame>) {
        let (tx, rx) = mpsc::channel(bound);
        (
            ConnHandle {
                connection_id: id.into(),
                tx,
                shutdown: Arc::new(Notify::new()),
            },
            rx,
        )
    }

    #[test]
    fn bound_machine_name_trims_bounds_and_empties() {
        // Trims surrounding whitespace.
        assert_eq!(
            bound_machine_name("  Ruud's Mac  ").as_deref(),
            Some("Ruud's Mac")
        );
        // Empty / whitespace-only → None.
        assert_eq!(bound_machine_name(""), None);
        assert_eq!(bound_machine_name("   "), None);
        // Truncates to MAX_MACHINE_NAME_CHARS characters (not bytes).
        let long = "a".repeat(MAX_MACHINE_NAME_CHARS + 20);
        assert_eq!(
            bound_machine_name(&long).unwrap().chars().count(),
            MAX_MACHINE_NAME_CHARS
        );
        // Multi-byte scalars are counted as characters and never split.
        let emoji = "🦀".repeat(MAX_MACHINE_NAME_CHARS + 5);
        let bounded = bound_machine_name(&emoji).unwrap();
        assert_eq!(bounded.chars().count(), MAX_MACHINE_NAME_CHARS);
        assert!(bounded.chars().all(|c| c == '🦀'));
    }

    #[tokio::test]
    async fn offline_phone_with_token_is_woken() {
        let push = Arc::new(RecordingPush::default());
        let state = state_with_push(push.clone());
        let pairing = PairingId::new("pair");
        state
            .store
            .register_push_token(pairing.clone(), "tok".into(), ApnsEnvironment::Sandbox)
            .await;

        // Desktop sends; the phone leg is not attached (offline).
        deliver_or_push(&state, &envelope("pair", Role::Desktop, 1), Role::Desktop).await;

        let fired = push.fired.lock().unwrap();
        assert_eq!(
            *fired,
            vec![(pairing, "tok".to_string(), ApnsEnvironment::Sandbox)]
        );
    }

    #[tokio::test]
    async fn online_phone_is_forwarded_not_pushed() {
        let push = Arc::new(RecordingPush::default());
        let state = state_with_push(push.clone());
        let pairing = PairingId::new("pair");
        state
            .store
            .register_push_token(pairing.clone(), "tok".into(), ApnsEnvironment::Sandbox)
            .await;
        // Attach the phone leg → it's online.
        let (phone, mut rx) = conn_handle("conn_phone");
        state.registry.attach(&pairing, Role::Phone, phone);

        deliver_or_push(&state, &envelope("pair", Role::Desktop, 1), Role::Desktop).await;

        // Forwarded to the phone, and no push fired.
        assert!(matches!(rx.try_recv(), Ok(RelayFrame::Envelope(_))));
        assert!(push.fired.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn offline_phone_without_token_is_not_pushed() {
        let push = Arc::new(RecordingPush::default());
        let state = state_with_push(push.clone());
        deliver_or_push(&state, &envelope("pair", Role::Desktop, 1), Role::Desktop).await;
        assert!(push.fired.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stuck_peer_does_not_block_the_forwarding_sender() {
        // remote-control-0ef.6: a peer whose outbound channel is jammed (a slow
        // or half-open zombie that never drains) must not back-pressure the
        // healthy sender. `deliver_or_push` is the exact call the sender's read
        // loop awaits per envelope; it must return promptly instead of parking
        // on the full channel.
        let push = Arc::new(RecordingPush::default());
        let state = state_with_push(push.clone());
        let pairing = PairingId::new("pair");

        // Attach a phone peer with a bound-1 outbox and jam it. `_rx` is held but
        // never drained, so the channel stays full — the half-open-socket case.
        let (phone, _rx) = conn_handle_bounded("conn_phone_stuck", 1);
        phone
            .tx
            .try_send(RelayFrame::Pong {
                client_time_ms: 0,
                server_time_ms: 0,
            })
            .expect("prime the jammed channel");
        state.registry.attach(&pairing, Role::Phone, phone);

        // The forward must complete without awaiting the drained channel. A
        // regression (reverting to `send().await`) would hang here until the
        // test's timeout fires.
        tokio::time::timeout(
            Duration::from_secs(1),
            deliver_or_push(&state, &envelope("pair", Role::Desktop, 1), Role::Desktop),
        )
        .await
        .expect("deliver_or_push must not block on a jammed peer outbox");

        // The peer is 'connected' (just slow), so no APNs push is fired — the
        // dropped envelope stays buffered for the peer to resume.
        assert!(push.fired.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn offline_desktop_is_never_pushed() {
        let push = Arc::new(RecordingPush::default());
        let state = state_with_push(push.clone());
        let pairing = PairingId::new("pair");
        state
            .store
            .register_push_token(pairing.clone(), "tok".into(), ApnsEnvironment::Sandbox)
            .await;
        // A phone→desktop envelope with the desktop offline must NOT push (only
        // phones are push-addressed).
        deliver_or_push(&state, &envelope("pair", Role::Phone, 1), Role::Phone).await;
        assert!(push.fired.lock().unwrap().is_empty());
    }
}
