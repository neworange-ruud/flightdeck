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
pub mod crypto;
pub mod feed;
pub mod identity;
pub mod notifier;
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
    /// A pairing was (re)confirmed active on the connection.
    Paired {
        /// The active pairing.
        pairing_id: PairingId,
        /// The peer device id, if the relay reported one.
        peer_device_id: Option<DeviceId>,
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
    /// Placeholder for a future desktop-initiated pairing offer. The v1 relay
    /// protocol has the *phone* redeem a claim token (`pairing_claim`); the
    /// desktop shows the code out of band. Wired now so the bridge/UI layer has
    /// a stable channel shape; currently a no-op the client logs and ignores.
    RequestPairing,
}
