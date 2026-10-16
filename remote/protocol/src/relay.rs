//! The **relay plane**: plaintext, content-free frames exchanged between an
//! endpoint (desktop or phone) and the hosted relay.
//!
//! The relay is zero-knowledge. Everything here is metadata the relay legitimately
//! needs to authenticate endpoints and route ciphertext: version negotiation,
//! device authentication, pairing bootstrap, presence, queued delivery with
//! per-pairing sequence numbers, acks, latency ping/pong, push-token registration,
//! desktop-requested encrypted pushes, and errors. The relay never sees
//! application content — that travels inside the opaque [`EncryptedEnvelope`]
//! (or, for a push, the sealed [`PushNotifyBody::ciphertext`]) and is defined in
//! [`crate::e2e`].
//!
//! The same frame set is also spoken on the **direct link** (spec §5.10): a
//! phone in a direct-mode pairing connects straight to the Mac, which plays the
//! relay's role for that pairing's data plane.

use serde::{Deserialize, Serialize};

use crate::common::{PushClass, Role};
use crate::ids::{DeviceId, EventId, PairingId};

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
    /// The sender's envelope `seq` diverged from the relay's expected next value
    /// (not `high_water + 1`, and not an idempotent re-send of the head). Unlike
    /// [`Self::BadFrame`] this is **recoverable**, not a client bug: the usual
    /// cause is the relay losing its in-memory per-pairing seq watermark across a
    /// restart/redeploy while the endpoint kept its persisted cursor. The
    /// This advisory covers **two different faults**, told apart by whether
    /// [`RelayFrame::Error::expected_seq`] is present:
    ///
    /// - `expected_seq: Some(n)` — raised on the *enqueue* path. The SENDER's
    ///   outbound numbering diverged; it must resume sending at `n`. Realigning
    ///   to a relay-*specified* position always terminates: `n` is the relay's
    ///   own `high_water + 1`, so the very next envelope is accepted, and
    ///   `high_water` only ever advances on an accept. This is expressly not the
    ///   blind "restart at `seq = 1`" that livelocked against a relay which
    ///   persists its watermark (remote-control-arg) — that rewind was rejected,
    ///   which drove another rewind, forever.
    /// - `expected_seq: None` — raised on the *resume* path. The RECEIVER's
    ///   inbound cursor sits above anything this stream holds; it should drop
    ///   the cursor and ask its peer for a fresh snapshot (remote-control-bbf).
    ///
    /// Before `expected_seq` existed both faults shared one code, and endpoints
    /// only ever implemented the receiver half — so a sender that ran ahead was
    /// never told to come back and both directions wedged permanently
    /// (remote-control-zv3).
    SeqViolation,
    /// The relay runs in [`RelayMode::PushOnly`] and does not carry the data
    /// plane: `envelope`, `resume` and `ack` are refused with this code (spec
    /// §5.9). Not fatal — the connection stays open for the control frames a
    /// push-only relay serves. Wire value: `relay_mode_push_only`.
    RelayModePushOnly,
    /// An unexpected relay-side failure.
    Internal,
}

/// Which planes a relay serves (spec §5.9), announced in
/// [`RelayFrame::HelloOk::relay_mode`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayMode {
    /// The routing relay: control plane **and** the data plane (queued
    /// envelopes, seq/ack/resume). The default, and what a relay that omits
    /// `relay_mode` (pre-v4) is. Wire value: `full`.
    #[default]
    Full,
    /// Control plane only: hello/auth, pairing bootstrap, machine name, revoke,
    /// push-token registration, ping, presence and `push_notify` (WS + HTTPS).
    /// `envelope`/`resume`/`ack` are refused with
    /// [`RelayErrorCode::RelayModePushOnly`]. Wire value: `push_only`.
    PushOnly,
}

impl RelayMode {
    /// The `snake_case` wire spelling — identical to the serde form and to the
    /// relay's `RELAY_MODE` config value.
    pub fn as_str(self) -> &'static str {
        match self {
            RelayMode::Full => "full",
            RelayMode::PushOnly => "push_only",
        }
    }
}

impl std::fmt::Display for RelayMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for RelayMode {
    type Err = String;

    /// Parses the wire spelling (`full` | `push_only`), e.g. the relay's
    /// `RELAY_MODE` environment variable. Anything else is an error naming the
    /// accepted values.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "full" => Ok(RelayMode::Full),
            "push_only" => Ok(RelayMode::PushOnly),
            other => Err(format!(
                "unknown relay mode {other:?} (expected \"full\" or \"push_only\")"
            )),
        }
    }
}

/// Path of the WebSocket endpoint the desktop serves for the direct link
/// (spec §5.10): `ws://<host>:<port>/remote/v1`.
pub const DIRECT_LINK_PATH: &str = "/remote/v1";

/// Domain-separation prefix for the direct-link auth signature (spec §5.10).
/// See [`direct_auth_message`].
pub const DIRECT_AUTH_DOMAIN: &[u8] = b"flightdeck-remote-direct-v1:";

/// Where a phone reaches the Mac directly (spec §5.2, §5.10). Carried in
/// [`RelayFrame::PairingOffer::direct_endpoint`] and copied by the relay into the
/// phone-bound [`RelayFrame::PairingClaimed::direct_endpoint`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DirectEndpoint {
    /// The Mac's Tailscale MagicDNS name (e.g. `ruuds-mbp.tail1234.ts.net`) or
    /// Tailscale address (e.g. `100.101.102.103`).
    pub host: String,
    /// TCP port the desktop's direct-link WebSocket listens on.
    pub port: u16,
}

impl DirectEndpoint {
    /// The WebSocket URL a phone dials: `ws://<host>:<port>/remote/v1`. An IPv6
    /// literal host (it contains `:`) is bracketed, per RFC 3986.
    pub fn ws_url(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("ws://[{}]:{}{DIRECT_LINK_PATH}", self.host, self.port)
        } else {
            format!("ws://{}:{}{DIRECT_LINK_PATH}", self.host, self.port)
        }
    }
}

/// The exact bytes a phone signs to authenticate on the direct link (spec
/// §5.10): `b"flightdeck-remote-direct-v1:" ‖ nonce`, where `nonce` is the
/// **decoded** `auth_challenge.nonce` bytes. Signed with the phone's identity key
/// (ECDSA P-256 / SHA-256, raw `r ‖ s`), verified by the Mac against the
/// `peer_device_public_key` it stored at pairing time.
///
/// The domain prefix makes a direct-link signature useless as a relay
/// `auth_response` (which signs the bare nonce, §5.1) and vice versa, so a Mac
/// that ran a malicious challenge could not replay the phone's answer to the
/// relay.
pub fn direct_auth_message(nonce: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(DIRECT_AUTH_DOMAIN.len() + nonce.len());
    message.extend_from_slice(DIRECT_AUTH_DOMAIN);
    message.extend_from_slice(nonce);
    message
}

/// Largest base64 `ciphertext` a push may carry, in characters (spec §7.2). The
/// relay rejects a larger `push_notify` with `error { code: bad_frame }` (WS) or
/// HTTP 413 (HTTPS). It keeps the APNs request under Apple's 4 KiB payload cap.
pub const MAX_PUSH_CIPHERTEXT_B64_LEN: usize = 3000;

/// Path of the relay's HTTPS push endpoint (spec §5.5).
pub const PUSH_HTTP_PATH: &str = "/v1/push";

/// Derive the HTTPS push endpoint from the relay's WebSocket URL (spec §5.5):
/// `wss://host[:port]/anything` → `https://host[:port]/v1/push`, and
/// `ws://…` → `http://…`. The relay URL's path, query and fragment are dropped;
/// the authority (host and any port) is kept verbatim. Returns `None` for any
/// other scheme or an empty authority.
pub fn push_endpoint_url(relay_ws_url: &str) -> Option<String> {
    let (http_scheme, rest) = if let Some(rest) = relay_ws_url.strip_prefix("wss://") {
        ("https", rest)
    } else if let Some(rest) = relay_ws_url.strip_prefix("ws://") {
        ("http", rest)
    } else {
        return None;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return None;
    }
    Some(format!("{http_scheme}://{authority}{PUSH_HTTP_PATH}"))
}

/// HTTP header carrying the optional shared relay password on the HTTPS push
/// endpoint — the HTTPS twin of [`RelayFrame::Hello::relay_password`].
pub const RELAY_PASSWORD_HEADER: &str = "x-flightdeck-relay-password";

/// Largest allowed `|relay_now - timestamp_ms|` for an HTTPS [`PushRequest`],
/// in milliseconds (5 minutes). Bounds replay of a captured request.
pub const PUSH_REQUEST_MAX_SKEW_MS: i64 = 300_000;

/// Domain-separation label that opens every HTTPS push signing message. See
/// [`push_request_signing_message`].
pub const PUSH_SIGNING_DOMAIN: &str = "flightdeck-remote-push-v1";

/// The five fields of a desktop-requested push (spec §5.5). Flattened next to
/// `type` in [`RelayFrame::PushNotify`] and nested under `push` in the HTTPS
/// [`PushRequest`] body — one struct so the WS and HTTPS paths cannot drift.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushNotifyBody {
    /// The pairing whose phone should be alerted. The requesting desktop must be
    /// a member.
    pub pairing_id: PairingId,
    /// The event this push announces. Used as the APNs collapse id, so a retried
    /// push for the same event replaces rather than duplicates the alert, and
    /// bound into the payload AAD (spec §7.2).
    pub event_id: EventId,
    /// The only plaintext hint: picks the APNs interruption level and generic
    /// alert text.
    pub class: PushClass,
    /// Base64 (standard, padded) 12-byte AEAD nonce.
    pub nonce: String,
    /// Base64 (standard, padded) ChaCha20-Poly1305 output (tag appended) of a
    /// serialized [`crate::e2e::PushPayload`], sealed under the pairing's push
    /// key (spec §7.2). At most [`MAX_PUSH_CIPHERTEXT_B64_LEN`] characters. The
    /// relay copies it into the APNs payload verbatim and never opens it.
    pub ciphertext: String,
}

/// The JSON body of `POST /v1/push` (spec §5.5): a desktop asking the relay to
/// send a push without (or regardless of) a live WebSocket. Authenticated by
/// [`Self::signature`] over [`push_request_signing_message`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushRequest {
    /// The requesting desktop device. Must be registered, role `desktop`, and a
    /// member of `push.pairing_id`.
    pub device_id: DeviceId,
    /// Desktop wall-clock time (unix ms) at signing. The relay rejects a request
    /// outside ±[`PUSH_REQUEST_MAX_SKEW_MS`] of its own clock.
    pub timestamp_ms: i64,
    /// The push itself — the same fields as [`RelayFrame::PushNotify`].
    pub push: PushNotifyBody,
    /// Base64 (standard, padded) ECDSA P-256 / SHA-256 signature, raw `r ‖ s`
    /// (64 bytes), by the device identity key over the UTF-8 bytes of
    /// [`Self::signing_message`].
    pub signature: String,
}

impl PushRequest {
    /// The canonical message [`Self::signature`] covers. Shorthand for
    /// [`push_request_signing_message`] over this request's fields.
    pub fn signing_message(&self) -> String {
        push_request_signing_message(&self.device_id, self.timestamp_ms, &self.push)
    }
}

/// The canonical string an HTTPS push request signs (spec §5.5) — the single
/// implementation the desktop (signer) and the relay (verifier) share:
///
/// ```text
/// flightdeck-remote-push-v1\n{device_id}\n{timestamp_ms}\n{pairing_id}\n{event_id}\n{class}\n{nonce}\n{ciphertext}
/// ```
///
/// `\n`-joined, no trailing newline; `timestamp_ms` is base-10, `class` is the
/// `snake_case` wire value. None of the fields can contain `\n` (ids are
/// relay/desktop-minted tokens; `nonce`/`ciphertext` are base64), so the join is
/// unambiguous. Sign/verify the UTF-8 bytes of the returned string.
pub fn push_request_signing_message(
    device_id: &DeviceId,
    timestamp_ms: i64,
    push: &PushNotifyBody,
) -> String {
    format!(
        "{PUSH_SIGNING_DOMAIN}\n{device_id}\n{timestamp_ms}\n{}\n{}\n{}\n{}\n{}",
        push.pairing_id,
        push.event_id,
        push.class.as_str(),
        push.nonce,
        push.ciphertext,
    )
}

/// The JSON body of every non-2xx response from `POST /v1/push` (spec §5.5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushErrorBody {
    /// Machine-readable code — the same vocabulary as [`RelayFrame::Error`].
    pub code: RelayErrorCode,
    /// Human-readable detail for logs/support. Never contains push content.
    pub message: String,
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
        /// **Optional shared relay password** (remote-control-uq7). A relay that
        /// is configured with `FLIGHTDECK_RELAY_PASSWORD` gates access on this
        /// value: it rejects the connection (a plain `error { code: auth_failed }`
        /// then close) when this field is missing or does not match, comparing in
        /// constant time. A relay with **no** password configured ignores this
        /// field entirely and stays open, so local/dev relays and older clients
        /// keep working. `Option` + `#[serde(default)]` for backward compatibility:
        /// an older client omits it (deserialized as `None`), which an
        /// unconfigured relay accepts and a configured relay rejects.
        ///
        /// This is a coarse network-admission secret shared by every client and
        /// the relay; it is **not** a substitute for the per-device
        /// challenge-response auth (spec §5.1) or the end-to-end crypto — those
        /// still run underneath. It never appears in any `hello_ok`/log line.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relay_password: Option<String>,
    },

    /// relay -> endpoint. Accepts the connection at a negotiated version.
    HelloOk {
        /// Version both sides will use (min of the two preferred, within range).
        protocol_version: u16,
        /// Relay wall-clock time (unix ms) for coarse clock-skew awareness.
        server_time_ms: i64,
        /// Opaque per-connection id for logging/support.
        connection_id: String,
        /// **v4, optional.** Which planes this relay serves (spec §5.9). Absent
        /// (`None`) from a pre-v4 relay, which is [`RelayMode::Full`]. The Mac's
        /// direct-link server (spec §5.10) also sends `None` — it is not a relay
        /// and carries the full data plane for its own pairings. Omitted from the
        /// wire when `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        relay_mode: Option<RelayMode>,
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
        /// **Desktop only, optional.** The desktop's self-asserted, human-readable
        /// machine name (e.g. `"Ruud's MacBook Pro"`), sent on **every** connect
        /// so the phone's per-pairing default name auto-updates when the Mac is
        /// renamed (spec §10.1). It applies to **all** the `pairing_ids` activated
        /// on this connection (one Mac → one name). The relay stores it per
        /// pairing and forwards it to the paired phone via [`Self::MachineName`].
        ///
        /// This is **untrusted display text**: the relay length-bounds it and the
        /// phone sanitizes it before display; it is never used for routing or
        /// authorization. The phone sends `None` here. `Option` for backward
        /// compatibility — an older desktop omits it (deserialized as `None`) and
        /// the phone simply keeps its previous/fallback name.
        #[serde(default)]
        machine_name: Option<String>,
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
        /// **v4, optional.** Where the phone can reach this Mac directly over
        /// Tailscale (spec §5.10). When `Some`, the relay copies it verbatim into
        /// the **phone-bound** [`Self::PairingClaimed::direct_endpoint`], and the
        /// pairing is in **direct mode** on both ends. `None` (and every pre-v4
        /// desktop) makes a relay-mode pairing. The relay never dials it and
        /// never uses it for routing. Omitted from the wire when `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direct_endpoint: Option<DirectEndpoint>,
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
    ///
    /// Sent to **both** sides of a fresh pairing, and the two copies carry
    /// different peer material:
    ///
    /// | field | phone-bound copy (answer to `pairing_claim`) | desktop-bound copy (notification) |
    /// |---|---|---|
    /// | `peer_device_id` | the desktop's id | the phone's id |
    /// | `peer_key_agreement_public_key` | the desktop's KA key | the phone's KA key |
    /// | `direct_endpoint` | copied from the offer, if any | always `None` |
    /// | `peer_device_public_key` | always `None` | the phone's identity key |
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
        /// **v4, optional, phone-bound copy only.** The desktop's
        /// [`DirectEndpoint`], copied by the relay from the pairing's
        /// [`Self::PairingOffer::direct_endpoint`]. `Some` puts the pairing in
        /// **direct mode** (spec §5.10): the phone carries the whole data plane
        /// over the direct link and uses the relay only for push-token
        /// registration, revoke and pushes. The relay never sets it on the
        /// desktop-bound copy. Omitted from the wire when `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        direct_endpoint: Option<DirectEndpoint>,
        /// **v4, optional, desktop-bound copy only.** The phone's **identity**
        /// public key (the `device_public_key` from its `pairing_claim`; base64
        /// standard-padded X9.63 uncompressed SEC1, 65 bytes), so the Mac can
        /// verify the phone's direct-link auth signature itself (spec §5.10). The
        /// relay never sets it on the phone-bound copy. Omitted from the wire when
        /// `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer_device_public_key: Option<String>,
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

    /// relay -> endpoint (phone). The desktop's self-asserted, human-readable
    /// machine name for a specific pairing (spec §10.1). Forwarded from the
    /// desktop's [`Self::AuthResponse`] `machine_name`, scoped to one
    /// `pairing_id` so a phone paired with several Macs can attribute each name
    /// correctly. The relay emits it:
    /// * to the phone right after it authenticates, for each activated pairing
    ///   whose desktop has already announced a name, **and**
    /// * to the connected phone whenever the desktop (re)connects and announces
    ///   its name (so a Mac rename propagates on the desktop's next connect).
    ///
    /// `machine_name` is **untrusted display text**: the relay length-bounds it
    /// (see the relay's `MAX_MACHINE_NAME_CHARS`) and the phone must sanitize it
    /// before display. It is display-only — never used for routing or auth. The
    /// phone treats this value as the pairing's new default name (a user override
    /// still wins). A pairing for which no name has been announced simply never
    /// receives this frame, and the phone keeps its previous/fallback name.
    MachineName {
        /// The pairing the name applies to.
        pairing_id: PairingId,
        /// The desktop's current display name (already length-bounded).
        machine_name: String,
    },

    /// endpoint -> relay. Phone-initiated unpair: revoke a pairing this device is
    /// a member of (spec §10.2). The relay **must** verify the authenticated
    /// device is a member of `pairing_id` before removing it; a non-member's
    /// revoke is refused with `error { code: unknown_pairing }` and changes
    /// nothing. On success the relay drops the pairing and all of its state
    /// (membership, claim tokens, queued envelopes, push token) and notifies the
    /// peer with [`Self::PairingRevoked`]. Revocation is **idempotent**: revoking
    /// a pairing that is already gone is a success no-op (the relay still replies
    /// [`Self::PairingRevoked`] to the requester). Only this one pairing is
    /// affected; the device's other pairings are untouched.
    Revoke {
        /// The pairing to revoke.
        pairing_id: PairingId,
    },

    /// relay -> endpoint. A pairing was revoked and no longer exists on the
    /// relay. Sent to the **peer** of the revoker (so a desktop learns its phone
    /// unpaired it and can return to an unpaired, re-pairable state) and, as an
    /// idempotent success confirmation, back to the **revoker**. The recipient
    /// should drop its local record for `pairing_id` and tear down that pairing's
    /// E2E channel; its other pairings are unaffected.
    PairingRevoked {
        /// The pairing that was revoked.
        pairing_id: PairingId,
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

    /// relay -> endpoint. Confirms a push token was stored **or removed** — the
    /// shared confirmation for both [`Self::RegisterPushToken`] and
    /// [`Self::UnregisterPushToken`] (both act on the same per-pairing token
    /// slot, so one ack keeps the wire minimal and symmetric).
    PushTokenAck {
        /// Pairing the token was stored/removed for.
        pairing_id: PairingId,
    },

    /// phone -> relay. Deregisters the pairing's APNs token **without unpairing**
    /// (spec §5.5), so a phone can stop this pairing's pushes (e.g. a per-machine
    /// push mute) while keeping the pairing itself intact. The relay **must**
    /// verify the authenticated device is a member of `pairing_id` before
    /// removing anything — a non-member's request is refused with
    /// `error { code: unknown_pairing }` and changes nothing, mirroring
    /// [`Self::Revoke`]'s membership invariant. Removal is **idempotent**:
    /// unregistering when no token is stored is a success no-op. On success the
    /// relay drops the token and confirms with [`Self::PushTokenAck`]. Only this
    /// pairing's token is affected; the device's other pairings are untouched and
    /// the pairing stays paired (unlike [`Self::Revoke`]).
    UnregisterPushToken {
        /// Pairing whose push token should be removed.
        pairing_id: PairingId,
    },

    /// desktop -> relay (**v4**). Ask the relay to send one visible APNs alert
    /// to the paired phone (spec §5.5). `type` is `push_notify`; the
    /// [`PushNotifyBody`] fields are flattened alongside it, exactly like
    /// [`Self::Envelope`].
    ///
    /// The desktop alone decides when a push is warranted (needs input, error,
    /// done); the relay sends no pushes of its own. Accepted only from an
    /// authenticated desktop that is a member of `pairing_id` (else
    /// `error { code: unknown_pairing }`; from a phone, `bad_frame`).
    /// Fire-and-forget: there is no success reply; failures surface as `error`
    /// frames. A pairing with no registered push token is a silent no-op. Served
    /// in both relay modes, and over HTTPS as [`PushRequest`].
    PushNotify(PushNotifyBody),

    /// relay -> endpoint. A relay-plane error. Fatal-ness is implied by the code.
    Error {
        /// Machine-readable code.
        code: RelayErrorCode,
        /// Human-readable detail for logs/support.
        message: String,
        /// The pairing the error relates to, if any.
        pairing_id: Option<PairingId>,
        /// For [`RelayErrorCode::SeqViolation`] raised on the **enqueue** path:
        /// the `seq` the relay will accept next on the sender's stream, i.e.
        /// `high_water + 1`. Present only when the sender's OUTBOUND numbering
        /// is what diverged; absent when the advisory is about the receiver's
        /// stale INBOUND cursor (the `resume` path). The sender realigns its
        /// outbound counter to this value — see the [`RelayErrorCode::SeqViolation`]
        /// docs for why an unconditional rewind is not safe but this is
        /// (remote-control-zv3).
        ///
        /// Optional and omitted when absent, so an endpoint built before this
        /// field existed simply sees the advisory it already understood.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_seq: Option<u64>,
    },

    /// Both directions. Graceful shutdown notice before closing the socket.
    Bye {
        /// Optional human-readable reason.
        reason: Option<String>,
    },
}
