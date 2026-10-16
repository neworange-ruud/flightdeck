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
//!                                                                    ping/pong, push tokens,
//!                                                                    push_notify (desktop)
//! ```
//!
//! Queued envelopes never trigger a push (spec §5.5, v4); a desktop asks for
//! one explicitly with `push_notify`, handled by [`crate::push`].
//!
//! **Push-only mode** (spec §5.9, `RELAY_MODE=push_only`): the lifecycle above
//! is unchanged except that the data plane is switched off — `envelope`,
//! `resume` and `ack` are answered with an advisory
//! `error { code: relay_mode_push_only }` (the socket stays open), nothing is
//! forwarded to the peer, and the store's queue methods (`enqueue`, `resume`,
//! `ack`, `ack_cursor`, `reset_stream`) are never called, so no sender queue or
//! seq high-water mark is created. The auth-time ack-cursor frame is not sent
//! either: it advertises a relay that forwards acks, which this one does not.
//!
//! Wrong-order frames, unknown devices, and bad signatures are answered with a
//! [`RelayFrame::Error`] carrying the appropriate [`RelayErrorCode`] and, for
//! fatal cases (version/auth), the socket is closed. An unauthenticated
//! connection is dropped after `AUTH_TIMEOUT_SECS`.
//!
//! Application payloads ([`EncryptedEnvelope`]) are forwarded to the peer
//! **verbatim**; this module never base64-decodes or otherwise inspects
//! `ciphertext` (PRD §9.1).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket};
use flightdeck_remote_protocol::{
    negotiate_version, ClientInfo, DeviceId, DirectEndpoint, EncryptedEnvelope, PairingId,
    PresenceState, PushNotifyBody, RelayErrorCode, RelayFrame, Role, DIRECT_LINK_MIN_VERSION,
    MAX_SUPPORTED_VERSION, MIN_SUPPORTED_VERSION,
};
use futures_util::stream::{SplitStream, StreamExt};
use tokio::sync::{mpsc, Notify};
use tokio::time::{timeout_at, Duration, Instant};

use crate::auth;
use crate::claims::ClaimError;
use crate::ids;
use crate::queue::{AppendOutcome, QueueError, ResumeOutcome};
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
    /// Protocol version negotiated in `hello_ok` (0 until hello is accepted).
    /// Gates the direct link on the claiming phone's capability (spec §5.2):
    /// only a v4+ phone can reach the Mac directly.
    negotiated_version: u16,
    /// How many `pairing_claim` frames this connection has attempted, for the
    /// per-connection rate limit ([`MAX_CLAIM_ATTEMPTS_PER_CONN`]).
    claim_attempts: u32,
    /// Fires when a newer leg supersedes one of this connection's slots in the
    /// registry, telling the read loop to tear down (remote-control-0ef.8).
    /// The same `Arc` is cloned into every [`ConnHandle`] this connection
    /// registers, so superseding any leg wakes the whole connection.
    shutdown: Arc<Notify>,
    /// Consecutive `seq_violation` rejections per pairing on this connection,
    /// reset by the first accepted envelope. A healthy realignment shows up as a
    /// streak of 1; anything sustained means the sender is not acting on the
    /// advisory, which is the failure that went unnoticed for 65k rejections
    /// across 17 days (remote-control-zv3). Escalated to WARN so it is visible
    /// without turning on per-envelope diagnostics.
    seq_reject_streak: HashMap<String, u64>,
    /// How many data-plane frames a push-only relay has refused on this
    /// connection. The first is logged at WARN (a client treating a push-only
    /// relay as a router is misconfigured — likely a relay-mode pairing); the
    /// rest at DEBUG so a chatty client cannot flood the log.
    push_only_refusals: u64,
    phase: Phase,
}

/// Streak length at which a sender that keeps ignoring the realign advisory is
/// escalated from INFO to WARN, and every multiple thereafter. High enough that
/// a normal one-round-trip realignment never warns.
const SEQ_REJECT_WARN_EVERY: u64 = 50;

/// The `message` of the `relay_mode_push_only` refusal (spec §5.9; matches the
/// `relay/error_relay_mode_push_only.json` fixture).
const PUSH_ONLY_REFUSAL: &str = "this relay runs in push_only mode and does not carry envelopes";

/// The `pairing_claim_rejected` message a push-only relay sends when the
/// desktop offered a direct link but the claiming phone negotiated below
/// [`DIRECT_LINK_MIN_VERSION`] (spec §5.9): only a newer app can pair here.
pub const PUSH_ONLY_CLAIM_PHONE_TOO_OLD: &str =
    "this relay is push-only; update the iPhone app to pair over Tailscale";

/// The `pairing_claim_rejected` message a push-only relay sends when the offer
/// carried no direct endpoint (spec §5.9): the Mac must pair over its direct
/// link here.
pub const PUSH_ONLY_CLAIM_NO_DIRECT_OFFER: &str =
    "this relay is push-only; turn on the Mac's direct link (Tailscale) and pair again";

impl Connection {
    /// Create a connection in its initial state.
    pub fn new(state: AppState, out_tx: mpsc::Sender<RelayFrame>) -> Self {
        Self {
            state,
            out_tx,
            connection_id: ids::connection_id(),
            role: None,
            negotiated_version: 0,
            claim_attempts: 0,
            shutdown: Arc::new(Notify::new()),
            seq_reject_streak: HashMap::new(),
            push_only_refusals: 0,
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
                            expected_seq: None,
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
            relay_password,
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

        // Shared relay-password gate (remote-control-uq7). Only enforced when a
        // password is configured; an unconfigured relay stays open (local/dev,
        // existing tests). The presented value is compared to the configured one
        // in constant time so a network attacker cannot recover the secret from
        // response-timing. On failure the relay reports a plain `auth_failed` and
        // closes — deliberately identical for a missing vs a wrong password, so
        // no signal leaks about which was the problem. The secret never appears
        // in any log line.
        if !auth::relay_password_ok(
            self.state.config.relay_password.as_deref(),
            relay_password.as_deref(),
        ) {
            tracing::warn!(
                conn = %self.connection_id,
                ?role,
                "hello rejected: missing or incorrect relay password"
            );
            self.send_error(RelayErrorCode::AuthFailed, "relay password required", None)
                .await;
            return Flow::Close;
        }

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
        self.negotiated_version = version;
        let _ = &client as &ClientInfo; // build metadata is diagnostic only

        self.send(RelayFrame::HelloOk {
            protocol_version: version,
            server_time_ms: now_ms(),
            connection_id: self.connection_id.clone(),
            // Spec §5.9: the configured `RELAY_MODE`, so a client learns up
            // front whether this relay carries the data plane.
            relay_mode: Some(self.state.relay_mode()),
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
                direct_endpoint,
            } => {
                self.on_pairing_offer(
                    device_id,
                    device_public_key,
                    key_agreement_public_key,
                    role,
                    claim_token_hint,
                    direct_endpoint,
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
        direct_endpoint: Option<DirectEndpoint>,
    ) -> Flow {
        // The offer must come from the connection's own (desktop) identity.
        if !self.hello_device_matches(&device_id) || role != Role::Desktop {
            self.send_error(RelayErrorCode::BadFrame, "invalid pairing_offer", None)
                .await;
            return Flow::Close;
        }
        // The relay never dials the direct endpoint, but it stores it and hands
        // it to the phone, so keep it to a plausible host:port (spec §5.2).
        if direct_endpoint
            .as_ref()
            .is_some_and(|e| !is_valid_direct_endpoint(e))
        {
            self.send_error(RelayErrorCode::BadFrame, "malformed direct_endpoint", None)
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
                    && self.state.store.claim_token_is_free(&hint, now_ms()).await =>
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
                direct_endpoint,
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

        // Direct mode needs a direct-capable phone (spec §5.2): the offer must
        // have carried an endpoint AND this phone's connection must have
        // negotiated v4+ (a v4 build is the one that dials the direct link). An
        // older phone would ignore the endpoint and keep using the relay, so it
        // gets neither copy and the pairing is relay mode on both ends.
        let offered_direct = claim.direct_endpoint.is_some();
        let direct_endpoint = claim
            .direct_endpoint
            .clone()
            .filter(|_| self.negotiated_version >= DIRECT_LINK_MIN_VERSION);

        // A push-only relay carries no data plane (spec §5.9), so a relay-mode
        // pairing made here could never exchange a single envelope. Refuse the
        // claim instead of minting one: the phone hears why, the waiting
        // desktop's offer is failed with the same words, and the half-made
        // pairing is removed. The token is already spent (redemption is
        // single-use), which is fine — retrying cannot succeed either.
        if self.state.is_push_only() && direct_endpoint.is_none() {
            let message = if offered_direct {
                PUSH_ONLY_CLAIM_PHONE_TOO_OLD
            } else {
                PUSH_ONLY_CLAIM_NO_DIRECT_OFFER
            };
            tracing::info!(
                conn = %self.connection_id, pairing = ?claim.pairing_id,
                version = self.negotiated_version, offered_direct,
                "pairing_claim REFUSED: push-only relay, the pairing would be relay mode"
            );
            if let Some(desktop) = self.state.registry.peer(&claim.pairing_id, Role::Phone) {
                desktop
                    .send(RelayFrame::Error {
                        code: RelayErrorCode::PairingClaimRejected,
                        message: message.to_string(),
                        pairing_id: Some(claim.pairing_id.clone()),
                        expected_seq: None,
                    })
                    .await;
            }
            self.state.registry.remove(&claim.pairing_id);
            let _ = self
                .state
                .store
                .revoke_pairing(&claim.pairing_id, &claim.desktop_device)
                .await;
            self.send_error(RelayErrorCode::PairingClaimRejected, message, None)
                .await;
            return Flow::Continue;
        }

        // Register the phone's identity + key-agreement keys and attach it to
        // the pairing. The identity key is also handed to the desktop below
        // (`peer_device_public_key`, v4) so it can authenticate the phone on the
        // direct link (spec §5.10).
        self.state
            .store
            .register_device(device_id.clone(), device_public_key.clone())
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

        if offered_direct && direct_endpoint.is_none() {
            tracing::info!(
                conn = %self.connection_id, pairing = ?claim.pairing_id,
                version = self.negotiated_version,
                "pairing_claim: phone below v{DIRECT_LINK_MIN_VERSION}, direct endpoint withheld (relay mode)"
            );
        }

        // Tell the phone which pairing it joined, who its peer is, the
        // desktop's key-agreement public key for the E2E ECDH (spec §7.1) and,
        // for a direct-mode pairing, where to reach the Mac (spec §5.2, v4).
        self.send(RelayFrame::PairingClaimed {
            pairing_id: claim.pairing_id.clone(),
            peer_device_id: Some(desktop_device),
            peer_key_agreement_public_key: desktop_ka_key,
            direct_endpoint: direct_endpoint.clone(),
            peer_device_public_key: None,
        })
        .await;

        // Notify the waiting desktop connection, if it is currently connected,
        // that a phone has joined this pairing, and hand it the phone's KA key
        // and identity key (the latter on every v4 claim, spec §5.2). The
        // endpoint is echoed exactly when the phone got it: that echo is what
        // puts the desktop's side of the pairing in direct mode.
        if let Some(desktop) = self.state.registry.peer(&claim.pairing_id, Role::Phone) {
            desktop
                .send(RelayFrame::PairingClaimed {
                    pairing_id: claim.pairing_id,
                    peer_device_id: Some(device_id),
                    peer_key_agreement_public_key: phone_ka_key,
                    direct_endpoint,
                    peer_device_public_key: Some(device_public_key),
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
        let push_only = self.state.is_push_only();
        for pairing in &activated {
            // Hand this leg its OWN stream's ack position before anything else.
            // Sent unconditionally, including the very common `cursor: 0`, for two
            // reasons (remote-control-5qu): it is reconnect catch-up for a cursor
            // the endpoint does not persist reliably, and — because a relay built
            // before this change never sends an unsolicited ack — receiving one at
            // all is how an endpoint learns that THIS relay forwards peer acks and
            // that an ack-based liveness deadline is therefore safe to enforce.
            // Without that signal a desktop could not distinguish "my phone has
            // acked nothing" from "this relay never relays acks", and enforcing a
            // deadline against an older relay would declare every phone dark.
            //
            // A push-only relay skips it (spec §5.9): it forwards no acks, so
            // advertising that it does would arm the desktop's ack deadline
            // against a relay that can never satisfy it — and reading the cursor
            // is a queue lookup this mode never makes.
            if !push_only {
                let ack_cursor = self.state.store.ack_cursor(pairing, role).await;
                self.send(RelayFrame::Ack {
                    pairing_id: pairing.clone(),
                    cursor: ack_cursor,
                })
                .await;
            }
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
        // Push-only relays refuse the data plane before any other check (spec
        // §5.9): the mode, not the pairing, is the reason, and nothing about
        // the frame is looked up, queued or forwarded.
        if self.state.is_push_only() {
            let refused = match &frame {
                RelayFrame::Envelope(env) => Some(("envelope", env.pairing_id.clone())),
                RelayFrame::Resume { pairing_id, .. } => Some(("resume", pairing_id.clone())),
                RelayFrame::Ack { pairing_id, .. } => Some(("ack", pairing_id.clone())),
                _ => None,
            };
            if let Some((kind, pairing_id)) = refused {
                return self.refuse_push_only(kind, pairing_id).await;
            }
        }
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
                direct_endpoint,
            } => {
                self.on_pairing_offer(
                    device_id,
                    device_public_key,
                    key_agreement_public_key,
                    role,
                    claim_token_hint,
                    direct_endpoint,
                )
                .await
            }
            RelayFrame::PushNotify(push) => self.on_push_notify(push).await,
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

    /// Handle a desktop-requested push (spec §5.5.1). Fire-and-forget: success
    /// sends nothing back; a refusal is an advisory `error` frame and the
    /// connection stays open.
    ///
    /// A phone connection is refused with `bad_frame` before anything else (the
    /// role it declared in `hello` is enough). Everything after that —
    /// membership, field limits, rate limit, token lookup and the APNs send — is
    /// [`crate::push::accept_push`], shared with `POST /v1/push`.
    async fn on_push_notify(&mut self, push: PushNotifyBody) -> Flow {
        let device_id = match &self.phase {
            Phase::Authed { device_id, .. } => device_id.clone(),
            _ => unreachable!("on_push_notify only called in Authed"),
        };
        let pairing_id = push.pairing_id.clone();
        if self.role != Some(Role::Desktop) {
            self.send_error(
                RelayErrorCode::BadFrame,
                "push_notify is accepted only from a desktop",
                Some(pairing_id),
            )
            .await;
            return Flow::Continue;
        }
        if let Err(rejection) =
            crate::push::accept_push(&self.state, &device_id, push, crate::push::Via::WebSocket)
                .await
        {
            self.send_error(rejection.code(), rejection.message(), Some(pairing_id))
                .await;
        }
        Flow::Continue
    }

    /// Refuse one data-plane frame (`kind` = `envelope` | `resume` | `ack`) on a
    /// push-only relay with the advisory `relay_mode_push_only` error. The
    /// connection stays open for the control frames this mode serves.
    async fn refuse_push_only(&mut self, kind: &'static str, pairing_id: PairingId) -> Flow {
        self.push_only_refusals += 1;
        if self.push_only_refusals == 1 {
            tracing::warn!(
                conn = %self.connection_id, role = ?self.role, pairing = ?pairing_id, frame = kind,
                "push_only relay refused a data-plane frame (client is using the relay as a \
                 router: relay-mode pairing or misconfigured client?)"
            );
        } else {
            tracing::debug!(
                conn = %self.connection_id, pairing = ?pairing_id, frame = kind,
                refusals = self.push_only_refusals,
                "push_only relay refused a data-plane frame"
            );
        }
        self.send_error(
            RelayErrorCode::RelayModePushOnly,
            PUSH_ONLY_REFUSAL,
            Some(pairing_id),
        )
        .await;
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
        let mut outcome = self.state.store.enqueue(env.clone()).await;

        // A sender whose seq REWOUND lost its outbound cursor and restarted its
        // stream (reinstall, restored backup). Answering `SeqViolation` — "drop
        // your stale cursor and resync" — cannot help: it has no cursor left to
        // drop and nothing to move forward to, so it restarts at the same low
        // seq and is rejected again, forever (remote-control-arg). The relay
        // holds the stale state here, so the relay is what must give way:
        // abandon our epoch, adopt the sender's, and take the envelope.
        if let Err(QueueError::Rewind { expected, got }) = outcome {
            tracing::info!(
                conn = %self.connection_id, pairing = ?pairing_id, role = ?role,
                expected, got, "DIAG sender seq REWOUND; resetting stream to its epoch"
            );
            self.state.store.reset_stream(&pairing_id, role, got).await;
            // Tell the PEER before the restarted envelope reaches it. Both go
            // through the peer's single outbound channel in order, so the
            // advisory is guaranteed to land first — which matters, because the
            // peer's inbound cursor is still on the abandoned epoch and would
            // otherwise dedup every seq of the new one away as a duplicate.
            self.advise_peer(
                &pairing_id,
                role,
                "peer restarted its stream; drop your inbound cursor and resync",
            );
            outcome = self.state.store.enqueue(env.clone()).await;
        }

        match outcome {
            Err(QueueError::Gap { expected, got }) => {
                tracing::info!(
                    conn = %self.connection_id, pairing = ?pairing_id, role = ?role,
                    expected, got, "DIAG envelope REJECTED (seq_violation)"
                );
                // Recoverable, not a client bug: the endpoint's outbound cursor
                // ran ahead of ours and the envelopes in between never arrived.
                // Hand back the seq we will accept so the sender can realign in
                // one round trip (remote-control-zv3); a bare advisory left it
                // guessing, and it guessed "reset my inbound cursor", which for a
                // runaway sender is a no-op — so the stream never recovered.
                self.note_seq_rejection(&pairing_id, role, expected, got);
                self.send_seq_realign(pairing_id, expected).await;
            }
            // Unreachable: the rewind branch above resets the stream so the
            // re-enqueue accepts `got`. Handled as a gap rather than panicking so
            // a future change here degrades to the old advisory, not a crash.
            Err(QueueError::Rewind { expected, got }) => {
                tracing::warn!(
                    conn = %self.connection_id, pairing = ?pairing_id, role = ?role,
                    expected, got, "DIAG rewind survived a stream reset"
                );
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
                // The stream is moving again — stop counting toward the wedged
                // -stream WARN so a recovered pairing does not keep escalating.
                self.clear_seq_rejection(&pairing_id);
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
                // inspected). An offline peer picks it up via resume/replay on
                // its next connect; queuing never triggers a push (spec §5.5,
                // v4 — the desktop asks for pushes itself with `push_notify`).
                deliver_to_peer(&self.state, &env, role);
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
        // FORWARD it to that peer. The frame is documented as travelling in both
        // directions and the sender needs it — it is the only end-to-end evidence
        // that its envelopes are actually being received — but the relay used to
        // consume it purely to trim its own queue. With nothing coming back, the
        // desktop's `last_acked_by_peer` was structurally pinned at 0 no matter
        // what the phone did, so it could not tell a receiving phone from a dark
        // one and fed a dead pairing 33,000 envelopes over 17 days
        // (remote-control-5qu). Non-blocking, exactly like the envelope forward
        // in `deliver_to_peer`: a stuck peer must never back-pressure this leg,
        // and a dropped ack costs nothing (the next one is cumulative, and the
        // cursor is re-sent on the peer's next auth).
        if let Some(peer) = self.state.registry.peer(&pairing_id, role) {
            let outcome = peer.try_send(RelayFrame::Ack {
                pairing_id: pairing_id.clone(),
                cursor,
            });
            if outcome != TrySendOutcome::Sent {
                tracing::info!(
                    conn = %self.connection_id, pairing = ?pairing_id, role = ?role,
                    ?outcome, cursor, "DIAG peer ack not forwarded; next ack or auth will carry it"
                );
            }
        }
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
        // Replay the peer's queued envelopes with seq > from_seq, in order —
        // unless a drop-oldest overflow shed seqs this receiver still needs.
        match self
            .state
            .store
            .resume(&pairing_id, peer_role(role), from_seq)
            .await
        {
            ResumeOutcome::Replay(replay) => {
                for env in replay {
                    self.send(RelayFrame::Envelope(env)).await;
                }
            }
            ResumeOutcome::Resync => {
                // The receiver asked to resume from before the oldest envelope
                // we still hold: overflow dropped the in-between seqs and they
                // are gone (remote-control-0ef.7). Replaying would hand the
                // receiver a stream with a permanent hole its gapless-seq
                // enforcement stalls on. Reuse the existing SeqViolation→resync
                // advisory instead of inventing a new frame: the receiver drops
                // its stale cursor for this pairing and requests a fresh
                // snapshot from its peer (the same recovery the enqueue-side
                // SeqViolation triggers), so the stream restarts cleanly rather
                // than silently losing data.
                tracing::info!(
                    conn = %self.connection_id, pairing = ?pairing_id, role = ?role,
                    from_seq, "resume precedes oldest retained envelope; signaling resync"
                );
                self.send_error(
                    RelayErrorCode::SeqViolation,
                    "resume precedes oldest retained envelope; resync required",
                    Some(pairing_id),
                )
                .await;
            }
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
            expected_seq: None,
        })
        .await;
    }

    /// Count a `seq_violation` for `pairing_id` and escalate a sustained streak.
    ///
    /// One rejection is normal — it is how a realignment starts. A streak means
    /// the sender is not applying `expected_seq`, i.e. the stream is wedged, and
    /// that is worth a WARN: the previous build emitted only INFO per envelope,
    /// so a permanently dead pairing looked identical to routine chatter.
    fn note_seq_rejection(&mut self, pairing_id: &PairingId, role: Role, expected: u64, got: u64) {
        let n = self
            .seq_reject_streak
            .entry(pairing_id.as_str().to_string())
            .and_modify(|n| *n += 1)
            .or_insert(1);
        if n.is_multiple_of(SEQ_REJECT_WARN_EVERY) {
            tracing::warn!(
                conn = %self.connection_id, pairing = ?pairing_id, role = ?role,
                expected, got, streak = *n,
                "sender is not realigning to expected_seq; stream is wedged"
            );
        }
    }

    /// Clear the rejection streak for `pairing_id` — called when an envelope is
    /// accepted, so a recovered stream stops counting toward the WARN.
    fn clear_seq_rejection(&mut self, pairing_id: &PairingId) {
        self.seq_reject_streak.remove(pairing_id.as_str());
    }

    /// Tell the SENDER its outbound stream has run ahead of ours and hand it the
    /// exact `seq` we will accept next (`high_water + 1`).
    ///
    /// This is the recovery `send_error(SeqViolation, ..)` could never provide.
    /// A bare advisory is ambiguous — endpoints implemented it as "my *inbound*
    /// cursor is stale", which does nothing for a sender that is ahead — so a
    /// forward gap wedged the stream permanently: we kept demanding
    /// `high_water + 1`, the sender kept incrementing past it, and neither side
    /// was allowed to move (remote-control-zv3). Naming `expected` makes the
    /// realignment terminate in one round trip: the sender jumps exactly here,
    /// the next envelope matches, and `high_water` advances again.
    async fn send_seq_realign(&self, pairing_id: PairingId, expected: u64) {
        self.send(RelayFrame::Error {
            code: RelayErrorCode::SeqViolation,
            message: format!("envelope seq is not gapless/monotonic; resume at {expected}"),
            pairing_id: Some(pairing_id),
            expected_seq: Some(expected),
        })
        .await;
    }

    /// Send a `SeqViolation` advisory to the *peer* of `role` on `pairing_id` —
    /// "your inbound cursor for this pairing is stale; drop it and resync".
    ///
    /// Non-blocking ([`ConnHandle::try_send`]) for the same reason the envelope
    /// forward is (remote-control-0ef.6): this runs on the *sender's* read loop,
    /// and awaiting a jammed peer's outbox here would let a stuck receiver
    /// back-pressure a healthy sender. A peer that is offline, full, or wedged
    /// simply misses the advisory — [`SenderQueue::resume`] catches it on the
    /// peer's next `resume`, whose `from_seq` now sits above the reset stream's
    /// high-water and so returns `Resync`.
    ///
    /// [`ConnHandle::try_send`]: crate::router::ConnHandle::try_send
    /// [`SenderQueue::resume`]: crate::queue::SenderQueue::resume
    fn advise_peer(&self, pairing_id: &PairingId, role: Role, message: &str) {
        let Some(peer) = self.state.registry.peer(pairing_id, role) else {
            return;
        };
        let outcome = peer.try_send(RelayFrame::Error {
            code: RelayErrorCode::SeqViolation,
            message: message.to_string(),
            pairing_id: Some(pairing_id.clone()),
            // Receiver-side advisory: this peer's INBOUND cursor is stale. The
            // sender-side realignment carries `expected_seq`; this one must not,
            // or the peer would rewind an outbound stream that is perfectly fine.
            expected_seq: None,
        });
        if outcome != TrySendOutcome::Sent {
            tracing::info!(
                conn = %self.connection_id, pairing = ?pairing_id, role = ?role,
                ?outcome, "DIAG peer resync advisory not delivered; resume will catch it"
            );
        }
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

/// Forward an accepted (already durably queued) envelope to its peer, if the
/// peer is connected. An offline peer gets nothing now: it `resume`s the queued
/// envelope on its next connect (spec §6.3).
///
/// **No push.** Before protocol v4 this fired an APNs wake push for every
/// envelope queued for an offline phone; that path is gone (spec §5.5). The
/// desktop alone decides what is push-worthy and asks with `push_notify`
/// ([`crate::push`]).
///
/// Never inspects `ciphertext` (PRD §9.1): the peer receives the envelope
/// verbatim. Synchronous on purpose — it never awaits (see below).
fn deliver_to_peer(state: &AppState, env: &EncryptedEnvelope, sender: Role) {
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
        "DIAG deliver -> NO PEER attached (envelope queued for resume; no push)"
    );
}

/// Whether a `pairing_offer.direct_endpoint` is plausible enough to store and
/// hand to the phone (spec §5.2): a non-zero port and a host of 1–253 bytes
/// with no whitespace or control characters (a DNS name, IPv4 or IPv6
/// literal). The relay never dials it; this only keeps junk out of the store
/// and the phone's `pairing_claimed`.
fn is_valid_direct_endpoint(endpoint: &DirectEndpoint) -> bool {
    endpoint.port != 0
        && !endpoint.host.is_empty()
        && endpoint.host.len() <= 253
        && endpoint
            .host
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apns::{PushOutcome, PushService};
    use crate::config::{Config, LogFormat};
    use crate::router::ConnHandle;
    use crate::AppState;
    use async_trait::async_trait;
    use flightdeck_remote_protocol::ApnsEnvironment;
    use std::sync::Arc;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    /// Records every push the relay asks to send, so tests can assert that the
    /// envelope path never pushes.
    #[derive(Default)]
    struct RecordingPush {
        fired: Mutex<Vec<(String, ApnsEnvironment, PushNotifyBody)>>,
    }

    #[async_trait]
    impl PushService for RecordingPush {
        async fn push_alert(
            &self,
            token: &str,
            environment: ApnsEnvironment,
            push: &PushNotifyBody,
        ) -> PushOutcome {
            self.fired
                .lock()
                .unwrap()
                .push((token.to_string(), environment, push.clone()));
            PushOutcome::Sent
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

    #[test]
    fn direct_endpoint_validation() {
        let ep = |host: &str, port: u16| DirectEndpoint {
            host: host.into(),
            port,
        };
        assert!(is_valid_direct_endpoint(&ep(
            "ruuds-mbp.tail1234.ts.net",
            47800
        )));
        assert!(is_valid_direct_endpoint(&ep("100.101.102.103", 1)));
        assert!(is_valid_direct_endpoint(&ep("fd7a:115c:a1e0::1", 47800)));
        assert!(!is_valid_direct_endpoint(&ep("", 47800)), "empty host");
        assert!(!is_valid_direct_endpoint(&ep("host", 0)), "port 0");
        assert!(!is_valid_direct_endpoint(&ep("bad host", 1)), "whitespace");
        assert!(!is_valid_direct_endpoint(&ep("bad\nhost", 1)), "control");
        assert!(
            !is_valid_direct_endpoint(&ep(&"a".repeat(254), 1)),
            "over 253 bytes"
        );
    }

    /// Spec §5.5 (v4): an envelope queued for an offline phone triggers no push
    /// at all, even with a registered token — and neither does one queued for
    /// an offline desktop.
    #[tokio::test]
    async fn queued_envelope_to_offline_peer_never_pushes() {
        let push = Arc::new(RecordingPush::default());
        let state = state_with_push(push.clone());
        let pairing = PairingId::new("pair");
        state
            .store
            .register_push_token(pairing.clone(), "tok".into(), ApnsEnvironment::Sandbox)
            .await;

        // Desktop sends; the phone leg is not attached (offline).
        deliver_to_peer(&state, &envelope("pair", Role::Desktop, 1), Role::Desktop);
        // Phone sends; the desktop leg is not attached either.
        deliver_to_peer(&state, &envelope("pair", Role::Phone, 1), Role::Phone);

        assert!(push.fired.lock().unwrap().is_empty());
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

        deliver_to_peer(&state, &envelope("pair", Role::Desktop, 1), Role::Desktop);

        // Forwarded to the phone, and no push fired.
        assert!(matches!(rx.try_recv(), Ok(RelayFrame::Envelope(_))));
        assert!(push.fired.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn stuck_peer_does_not_block_the_forwarding_sender() {
        // remote-control-0ef.6: a peer whose outbound channel is jammed (a slow
        // or half-open zombie that never drains) must not back-pressure the
        // healthy sender. `deliver_to_peer` is the exact call the sender's read
        // loop makes per envelope; it is synchronous and must return instead of
        // parking on the full channel.
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

        // Returns (it cannot await); the dropped envelope stays buffered for
        // the peer to resume, and nothing is pushed.
        deliver_to_peer(&state, &envelope("pair", Role::Desktop, 1), Role::Desktop);
        assert!(push.fired.lock().unwrap().is_empty());
    }
}
