//! FlightDeck Remote — the desktop relay client.
//!
//! This module owns the desktop half of the phone <-> desktop link: a single
//! long-lived outbound WebSocket connection to the hosted relay. It mirrors the
//! update-check thread idiom ([`crate::update::start_check`]) — a detached
//! `std::thread` owning a **blocking** [`tungstenite`] socket — because the TUI
//! has no async runtime and must stay single-threaded and synchronous.
//!
//! Layers:
//! * [`identity`] — the per-device ECDSA P-256 keypair and its wire encodings.
//! * [`state`] — `~/.flightdeck/remote.json`: the private key, pairings, and the
//!   per-direction sequence cursors that make `resume`/`ack`/dedup work.
//! * [`client`] — the connection thread: connect → hello → auth → resume → pump,
//!   with exponential backoff + jitter reconnect and periodic latency pings.
//!
//! ## What this module does NOT do
//!
//! It is **crypto-agnostic about application content**. Envelopes carry opaque
//! `ciphertext` handed to it by the (later) bridge/pairing layers; this client
//! never seals or opens them. It speaks only the relay plane
//! ([`flightdeck_remote_protocol::relay`]): versioning, auth, presence, delivery,
//! acks, and latency. Sealing/opening the E2E payload is a separate task.
//!
//! ## Threading & channels
//!
//! [`client::RemoteHandle::start`] takes a [`Sender<RemoteInbound>`] (thread →
//! app) and a [`Receiver<RemoteOutbound>`] (app → thread). The app drains
//! [`RemoteInbound`] non-blockingly each render tick and never blocks on the
//! socket; the thread drains [`RemoteOutbound`] each ~100 ms poll. Shutdown is a
//! shared atomic flag flipped by [`client::RemoteHandle::stop`].

pub mod bridge;
pub mod client;
pub mod commands;
pub mod crypto;
pub mod feed;
pub mod identity;
pub mod notifier;
pub mod pairing;
pub mod shell;
pub mod state;
pub mod transcript;

pub use bridge::{ProjectView, RemoteBridge};
pub use client::{RemoteHandle, RemoteLinkState};
pub use identity::DeviceIdentity;
pub use state::{Pairing, RemoteState};

use flightdeck_remote_protocol::relay::{EncryptedEnvelope, PresenceState};
use flightdeck_remote_protocol::{DeviceId, PairingId, Role};

/// A message from the relay-client thread to the app (drained each tick).
#[derive(Debug, Clone)]
pub enum RemoteInbound {
    /// The connection state changed (drives the "Reconnecting…"/latency UI).
    Link(RemoteLinkState),
    /// A deduplicated application envelope arrived from the peer. The
    /// `ciphertext` is still sealed; the bridge/pairing layer opens it.
    Envelope(EncryptedEnvelope),
    /// The peer's presence for a pairing changed.
    Presence {
        /// Pairing the presence change is about.
        pairing_id: PairingId,
        /// Which role changed.
        peer: Role,
        /// New presence.
        state: PresenceState,
    },
    /// A pairing was (re)confirmed active on the connection (e.g. re-activated
    /// on reconnect via `auth_ok`). Drives the outbound bridge to send a fresh
    /// snapshot; does not by itself establish the E2E channel.
    Paired {
        /// The active pairing.
        pairing_id: PairingId,
        /// The peer device id, if the relay reported one.
        peer_device_id: Option<DeviceId>,
    },
    /// The relay minted a claim token for a desktop-initiated pairing offer
    /// (`pairing_offer_ok`, spec §5.2). Drives the pairing overlay to display
    /// the 4-digit code + QR and start the expiry countdown.
    PairingOffered {
        /// The pairing the relay provisioned.
        pairing_id: PairingId,
        /// The effective claim token (equals the requested 4-digit hint when
        /// honored). Shown as the manual code; its UTF-8 bytes are the E2E salt.
        claim_token: String,
        /// Relay wall-clock time (unix ms) after which the token is rejected.
        expires_at_ms: i64,
    },
    /// A phone redeemed the claim token and joined the pairing
    /// (`pairing_claimed`, spec §5.2). Carries the peer's key-agreement public
    /// key — the moment the desktop can derive the E2E channel (spec §7.1).
    PairingClaimed {
        /// The now-established pairing.
        pairing_id: PairingId,
        /// The peer (phone) device id, if the relay reported one.
        peer_device_id: Option<DeviceId>,
        /// The peer's key-agreement public key (base64 standard-padded, X9.63),
        /// fed into the static-static ECDH. `None` if the relay had not recorded
        /// it (then the channel cannot be derived and pairing has not completed).
        peer_key_agreement_public_key: Option<String>,
    },
    /// The relay repeatedly rejected authentication for a persisted pairing on
    /// the auth-first reconnect path — it no longer recognizes this device /
    /// pairing (e.g. its store was wiped by a restart/redeploy). The client has
    /// already dropped the stale pairing(s) from its persisted state, so the
    /// next connect bootstraps a fresh offer instead of looping forever on a
    /// dead pairing. The UI should surface a clear "re-pair needed" state rather
    /// than a silent, endless "reconnecting" (remote-control-1jy).
    PairingRejected {
        /// The pairing ids that were dropped from persisted state.
        pairing_ids: Vec<PairingId>,
    },
}

/// A message from the app to the relay-client thread.
#[derive(Debug, Clone)]
pub enum RemoteOutbound {
    /// Send an application payload to the peer. The client wraps it in an
    /// [`EncryptedEnvelope`], assigning and persisting the next gapless `seq`
    /// for the pairing (spec §6.1). The `ciphertext`/`nonce` are opaque here.
    SendEnvelope {
        /// Destination pairing.
        pairing_id: PairingId,
        /// The gapless per-pairing sequence number assigned by the outbound
        /// bridge (spec §6.1). The bridge owns this counter because it is the
        /// sole producer of outbound envelopes and it must seal under the exact
        /// header the envelope carries (the AEAD binds `seq`/`sent_at_ms` as AAD,
        /// spec §7.1). The client sends it verbatim and persists it as the
        /// high-water mark for `resume`.
        seq: u64,
        /// Sender wall-clock time (unix ms) the payload was sealed under.
        sent_at_ms: i64,
        /// Base64 (standard, padded) AEAD nonce chosen by the sealing layer.
        nonce: String,
        /// Base64 (standard, padded) sealed payload.
        ciphertext: String,
    },
    /// Acknowledge contiguous receipt of the peer's envelopes up to `cursor`
    /// (spec §6.2). The client normally acks automatically on receipt; this lets
    /// the app confirm durable handling explicitly.
    Ack {
        /// Pairing being acked.
        pairing_id: PairingId,
        /// Highest contiguous incoming `seq` durably handled.
        cursor: u64,
    },
    /// Desktop-initiated pairing offer (Settings → Remote, spec §5.2). The
    /// client sends a `pairing_offer` carrying its device + key-agreement public
    /// keys and this optional 4-digit `claim_token_hint`, then routes the
    /// resulting `pairing_offer_ok` back as [`RemoteInbound::PairingOffered`].
    RequestPairing {
        /// A short human-typeable code the desktop would like the relay to use
        /// as the claim token, or `None` to let the relay mint one.
        claim_token_hint: Option<String>,
    },
    /// Forget a pairing (Settings → Remote → Unpair). The client drops it from
    /// its persisted [`RemoteState`] so it is no longer activated on future
    /// connections. There is no relay-plane "unpair" frame in v1, so this is a
    /// local clear; the pairing simply stops being resumed and the peer sees the
    /// desktop as permanently absent for it.
    Unpair {
        /// The pairing to forget.
        pairing_id: PairingId,
    },
}
