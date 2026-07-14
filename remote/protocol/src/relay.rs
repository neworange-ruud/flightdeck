//! The **relay plane**: plaintext, content-free frames exchanged between an
//! endpoint (desktop or phone) and the hosted relay.
//!
//! The relay is zero-knowledge. Everything here is metadata the relay legitimately
//! needs to authenticate endpoints and route ciphertext: version negotiation,
//! device authentication, pairing bootstrap, presence, queued delivery with
//! per-pairing sequence numbers, acks, latency ping/pong, push-token registration,
//! and errors. The relay never sees application content — that travels inside the
//! opaque [`EncryptedEnvelope`] and is defined in [`crate::e2e`].

use serde::{Deserialize, Serialize};

use crate::common::Role;
use crate::ids::{DeviceId, PairingId};

/// Presence of the peer endpoint for a pairing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceState {
    /// The peer is connected to the relay.
    Connected,
    /// The peer is not connected.
    Disconnected,
}

/// APNs environment a push token belongs to. Opaque to the E2E layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApnsEnvironment {
    /// Development (sandbox) APNs.
    Sandbox,
    /// Production APNs.
    Production,
}

/// Machine-readable relay error codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayErrorCode {
    /// The requested protocol version is outside the relay's supported range.
    UnsupportedVersion,
    /// Challenge-response authentication failed (bad signature / unknown key).
    AuthFailed,
    /// The referenced pairing id is unknown to the relay.
    UnknownPairing,
    /// A frame requiring authentication arrived before `auth_ok`.
    NotAuthenticated,
    /// The pairing claim token is invalid or expired.
    PairingClaimRejected,
    /// The peer is not currently reachable (frame will be queued if applicable).
    PeerUnavailable,
    /// The client exceeded a rate limit.
    RateLimited,
    /// A frame could not be parsed or violated the protocol.
    BadFrame,
    /// An unexpected relay-side failure.
    Internal,
}

/// Non-secret client build metadata, sent in `hello` for diagnostics. Opaque to
/// routing; never used for authorization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// App/build version string, e.g. `1.7.1`.
    pub app_version: String,
    /// Platform, e.g. `ios`, `macos`.
    pub platform: String,
    /// OS version string, if known.
    pub os_version: Option<String>,
}

/// An opaque, end-to-end-encrypted application payload. The relay routes it by
/// [`Self::pairing_id`] and never decrypts it.
///
/// The serialized header fields (`pairing_id`, `seq`, `sender`, `sent_at_ms`)
/// are intended to be authenticated as additional data (AAD) by the sealing
/// layer so the relay cannot tamper with routing/ordering undetected. This crate
/// carries the types only; it performs no cryptography.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedEnvelope {
    /// Pairing this payload belongs to.
    pub pairing_id: PairingId,
    /// Monotonic per-pairing, per-sender sequence number (starts at 1). Enables
    /// ordered delivery, resume-from-cursor, and receiver-side dedup.
    pub seq: u64,
    /// Which role sealed this payload.
    pub sender: Role,
    /// Sender's wall-clock time (unix milliseconds) when sealed.
    pub sent_at_ms: i64,
    /// Base64 (standard, padded) AEAD nonce.
    pub nonce: String,
    /// Base64 (standard, padded) ciphertext of a serialized [`crate::e2e`]
    /// message (`DesktopToPhone` or `PhoneCommand`).
    pub ciphertext: String,
}

/// A single frame on the relay plane. Internally tagged by `type` (snake_case).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayFrame {
    /// endpoint -> relay. First frame on a new connection; opens version
    /// negotiation and declares the connecting role + device.
    Hello {
        /// Highest protocol version the client prefers to speak.
        protocol_version: u16,
        /// Whether this connection is the desktop or the phone.
        role: Role,
        /// The client's device identity.
        device_id: DeviceId,
        /// Non-secret build metadata.
        client: ClientInfo,
    },

    /// relay -> endpoint. Accepts the connection at a negotiated version.
    HelloOk {
        /// Version both sides will use (min of the two preferred, within range).
        protocol_version: u16,
        /// Relay wall-clock time (unix ms) for coarse clock-skew awareness.
        server_time_ms: i64,
        /// Opaque per-connection id for logging/support.
        connection_id: String,
    },

    /// relay -> endpoint. The client's version is outside the supported range;
    /// the connection will be closed after this frame.
    VersionIncompatible {
        /// The version the client offered.
        your_version: u16,
        /// Oldest version the relay supports.
        min_supported: u16,
        /// Newest version the relay supports.
        max_supported: u16,
    },

    /// relay -> endpoint. Challenge nonce to be signed with the device's private
    /// identity key (ECDSA P-256 with SHA-256; see spec §5.1 — P-256 because the
    /// phone key is Secure-Enclave-resident and the SE only supports P-256).
    AuthChallenge {
        /// Base64 (standard, padded) random challenge nonce.
        nonce: String,
        /// Relay wall-clock time (unix ms).
        server_time_ms: i64,
    },

    /// endpoint -> relay. Signature over the challenge nonce, proving possession
    /// of the device private key, plus the pairings this device claims.
    AuthResponse {
        /// The authenticating device.
        device_id: DeviceId,
        /// Base64 (standard, padded) ECDSA P-256 signature over the `nonce`
        /// bytes, raw `r ‖ s` form (64 bytes).
        signature: String,
        /// Pairing ids this device wants to activate on this connection.
        pairing_ids: Vec<PairingId>,
    },

    /// relay -> endpoint. Authentication succeeded; these pairings are active.
    AuthOk {
        /// Pairings now routable on this connection.
        pairing_ids: Vec<PairingId>,
    },

    /// endpoint (desktop) -> relay. Desktop-side pairing bootstrap: registers
    /// the desktop's device public key and asks the relay to mint a
    /// short-lived, single-use **claim token** to display as the 4-digit code
    /// / QR (spec §5.2). The phone later redeems that token with
    /// [`Self::PairingClaim`].
    ///
    /// The spec's §5.2 flow only shows the phone redeeming a token; it does not
    /// pin down how the desktop *obtains* one. This frame (with
    /// [`Self::PairingOfferOk`]) closes that gap symmetrically with the phone
    /// side: it is sent after `hello_ok` and before the desktop's own
    /// `auth_response`, self-registering the desktop's key just as
    /// `pairing_claim` self-registers the phone's.
    PairingOffer {
        /// The desktop device requesting a pairing.
        device_id: DeviceId,
        /// Base64 (standard, padded) ECDSA P-256 public key to register for
        /// routing, X9.63 uncompressed SEC1 form (65 bytes, `0x04 ‖ x ‖ y`).
        device_public_key: String,
        /// Base64 (standard, padded) **key-agreement** P-256 public key,
        /// same X9.63 uncompressed SEC1 encoding as `device_public_key`
        /// (65 bytes, `0x04 ‖ x ‖ y`). This is the point the peer feeds into
        /// the static-static ECDH that bootstraps the E2E channel (spec §7.1);
        /// the private scalar never leaves this device and never transits the
        /// relay. On desktop this MAY equal `device_public_key` (the keystore
        /// identity key is usable for ECDH); on iOS it MUST be a **separate
        /// software P-256 key**, because the device identity key is a
        /// Secure-Enclave *signing* key whose scalar cannot be used for key
        /// agreement.
        key_agreement_public_key: String,
        /// The role making the offer (normally `desktop`).
        role: Role,
        /// Optional desired claim token the desktop asks the relay to mint (spec
        /// §5.2 amendment). When `Some` and the string is **free** (not a live
        /// token) and well-formed, the relay issues exactly this token so the
        /// desktop can display a short, human-typeable **4-digit code**; when
        /// `None`, unusable, or already taken, the relay mints its own random
        /// token and returns it in [`Self::PairingOfferOk`]. Either way the
        /// desktop displays whatever token the relay returns, so the two sides
        /// never disagree. A 4-digit token is low-entropy, so the relay pins it
        /// to a short TTL + single use + a per-connection claim rate limit; the
        /// E2E channel's confidentiality never rests on this token (spec §7.1).
        claim_token_hint: Option<String>,
    },

    /// relay -> endpoint (desktop). A pairing was provisioned; `claim_token` is
    /// the one-time secret to encode in the 4-digit code / QR, valid until
    /// `expires_at_ms`. The relay notifies this same desktop connection with a
    /// [`Self::PairingClaimed`] once a phone redeems the token.
    PairingOfferOk {
        /// The pairing id created for this offer.
        pairing_id: PairingId,
        /// One-time, short-TTL token to hand to the phone out of band.
        claim_token: String,
        /// Relay wall-clock time (unix ms) after which the token is rejected.
        expires_at_ms: i64,
    },

    /// endpoint -> relay. Short-lived pairing bootstrap: redeem the code/QR token
    /// shown on the desktop, registering this device's public key against a new
    /// pairing. Used once, during pairing.
    PairingClaim {
        /// One-time token carried by the 4-digit code / QR (short TTL).
        claim_token: String,
        /// The device redeeming the token.
        device_id: DeviceId,
        /// Base64 (standard, padded) ECDSA P-256 public key to register for
        /// routing, X9.63 uncompressed SEC1 form (65 bytes, `0x04 ‖ x ‖ y`).
        device_public_key: String,
        /// Base64 (standard, padded) **key-agreement** P-256 public key,
        /// same X9.63 uncompressed SEC1 encoding as `device_public_key`
        /// (65 bytes, `0x04 ‖ x ‖ y`). This is the point the desktop feeds into
        /// the static-static ECDH that bootstraps the E2E channel (spec §7.1);
        /// the private scalar never leaves this device and never transits the
        /// relay. On desktop this MAY equal `device_public_key`; on iOS it MUST
        /// be a **separate software P-256 key**, because the device identity
        /// key is a Secure-Enclave *signing* key whose scalar cannot be used
        /// for key agreement.
        key_agreement_public_key: String,
        /// The role redeeming (normally `phone`).
        role: Role,
    },

    /// relay -> endpoint. Pairing bootstrap succeeded; here is the assigned id.
    PairingClaimed {
        /// The pairing id now shared by both devices.
        pairing_id: PairingId,
        /// The peer device id, if already known to the relay.
        peer_device_id: Option<DeviceId>,
        /// The peer's **key-agreement** public key (base64 standard-padded,
        /// X9.63 uncompressed SEC1, 65 bytes), if the relay has recorded it.
        /// The phone receives the desktop's KA key here; the desktop's
        /// notification receives the phone's. Each endpoint feeds the peer's KA
        /// key into the static-static ECDH of spec §7.1. `Option` to match
        /// `peer_device_id`'s shape (the relay may not yet hold the peer's key).
        peer_key_agreement_public_key: Option<String>,
    },

    /// relay -> endpoint. The peer for a pairing connected or disconnected.
    PeerPresence {
        /// The affected pairing.
        pairing_id: PairingId,
        /// Which role the presence change is about.
        peer: Role,
        /// New presence state.
        state: PresenceState,
        /// Relay wall-clock time (unix ms) of the change.
        at_ms: i64,
    },

    /// Both directions. Carries an opaque E2E payload. `type` is `envelope`; the
    /// envelope's own fields are flattened alongside it.
    Envelope(EncryptedEnvelope),

    /// Both directions. Acknowledges contiguous receipt of a peer's envelopes up
    /// to and including `cursor` (the highest in-order `seq` durably handled).
    /// The relay may drop queued envelopes at or below the cursor.
    Ack {
        /// Pairing being acked.
        pairing_id: PairingId,
        /// Highest contiguous `seq` the sender has durably processed.
        cursor: u64,
    },

    /// endpoint -> relay. After (re)connecting, ask the relay to replay any
    /// queued envelopes for this pairing with `seq` greater than `from_seq`.
    Resume {
        /// Pairing to resume.
        pairing_id: PairingId,
        /// Highest `seq` already held by this endpoint; replay strictly above it.
        from_seq: u64,
    },

    /// endpoint -> relay. Latency probe carrying the client's send time.
    Ping {
        /// Client wall-clock time (unix ms) at send.
        client_time_ms: i64,
    },

    /// relay -> endpoint. Echoes the client time and adds the relay time so the
    /// client can display round-trip latency.
    Pong {
        /// Echoed client time from the matching `ping`.
        client_time_ms: i64,
        /// Relay wall-clock time (unix ms) when the pong was sent.
        server_time_ms: i64,
    },

    /// phone -> relay. Registers/refreshes the APNs token for a pairing so the
    /// relay/desktop can drive pushes. The token is opaque and never encrypted.
    RegisterPushToken {
        /// Pairing the token is for.
        pairing_id: PairingId,
        /// The opaque APNs device token (hex string).
        token: String,
        /// Which APNs environment the token belongs to.
        environment: ApnsEnvironment,
    },

    /// relay -> endpoint. Confirms a push token was stored.
    PushTokenAck {
        /// Pairing the token was stored for.
        pairing_id: PairingId,
    },

    /// relay -> endpoint. A relay-plane error. Fatal-ness is implied by the code.
    Error {
        /// Machine-readable code.
        code: RelayErrorCode,
        /// Human-readable detail for logs/support.
        message: String,
        /// The pairing the error relates to, if any.
        pairing_id: Option<PairingId>,
    },

    /// Both directions. Graceful shutdown notice before closing the socket.
    Bye {
        /// Optional human-readable reason.
        reason: Option<String>,
    },
}
