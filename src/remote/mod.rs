//! FlightDeck Remote — the desktop relay client.
//!
//! This module owns the desktop half of the phone <-> desktop link: a single
//! long-lived outbound WebSocket connection to the hosted relay. It is a
//! [`tokio`] task on the process's one shared runtime ([`runtime`]) — the same
//! runtime the embedded web server uses (`specs/WEB_INTERFACE.md` D6) — while the
//! TUI event loop itself stays single-threaded and synchronous and talks to the
//! task over `std::sync::mpsc` channels.
//!
//! Layers:
//! * [`identity`] — the per-device ECDSA P-256 keypair and its wire encodings.
//! * [`state`] — `~/.flightdeck/remote.json`: the private key, pairings, and the
//!   per-direction sequence cursors that make `resume`/`ack`/dedup work.
//! * [`runtime`] — the one async runtime, owned by a dedicated thread and shared
//!   with `src/web/server.rs` so the binary never starts a second one.
//! * [`client`] — the connection task: connect → hello → auth → resume → pump,
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
//! [`client::RemoteHandle::start`] takes a [`Sender<RemoteInbound>`] (task → app)
//! and a [`Receiver<RemoteOutbound>`] (app → task) — plain `std::sync::mpsc`,
//! unchanged by the move to tokio. The app drains [`RemoteInbound`]
//! non-blockingly each render tick and never blocks on the socket; the task is
//! woken by whichever channel or timer fires, rather than polling on a tick.
//! Shutdown is [`client::RemoteHandle::stop`] (or simply dropping the handle).

pub mod bridge;
pub mod client;
pub mod commands;
pub mod crypto;
pub mod debuglog;
pub mod feed;
pub mod identity;
pub mod notifier;
pub mod opencode;
pub mod pairing;
pub mod runtime;
pub mod shell;
pub mod state;
pub mod transcript;

pub use bridge::{PeerLiveness, ProjectView, RemoteBridge};
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
    /// The phone unpaired this Mac: the relay revoked the pairing and notified
    /// this desktop (`pairing_revoked`, spec §10.2). The client has already
    /// dropped the pairing from persisted state; the app should tear down that
    /// pairing's E2E channel and return to an unpaired, re-pairable state. Only
    /// this pairing is affected — any other pairings continue unchanged.
    PairingRevoked {
        /// The pairing the phone revoked.
        pairing_id: PairingId,
    },
    /// The relay sent a `seq_violation` advisory for this pairing: our **inbound**
    /// cursor is stale, because the peer restarted its outbound stream or the
    /// relay shed envelopes we still needed. The client has already zeroed this
    /// pairing's persisted `last_received_seq` and re-issued `resume { from_seq:
    /// 0 }`; the outbound bridge must re-send a full snapshot so the peer's view
    /// is rebuilt from a known-good state rather than from deltas it may have
    /// missed.
    ///
    /// The outbound stream is **not** renumbered. It was, once
    /// (remote-control-bbf), back when a restarted relay came back with an empty
    /// in-memory watermark — and against a *persistent* relay that rewind is what
    /// deadlocked the stream (remote-control-arg). The relay now adopts an
    /// unknown stream's starting seq and absorbs a peer's rewind itself, so
    /// `out_seq` keeps counting up.
    SeqResync {
        /// The pairing that must re-send a full snapshot.
        pairing_id: PairingId,
    },
    /// The relay rejected our **outbound** envelopes because our `seq` ran ahead
    /// of its watermark, and told us the seq it will accept next. The outbound
    /// bridge must realign `out_seq` so the next envelope is `next_seq`, and
    /// re-send a full snapshot — everything emitted while we were ahead was
    /// dropped by the relay and never reached the peer.
    ///
    /// Distinct from [`Self::SeqResync`], which is the mirror-image fault: there
    /// our *inbound* cursor is stale and our outbound stream is fine. Both used
    /// to arrive as the same bare `seq_violation`, and because only the inbound
    /// half was ever implemented, a runaway sender was never corrected and the
    /// pairing wedged permanently (remote-control-zv3).
    SeqRealign {
        /// The pairing whose outbound stream must be realigned.
        pairing_id: PairingId,
        /// The `seq` the relay will accept next; the next envelope must use it.
        next_seq: u64,
    },
    /// The peer acknowledged our outbound envelopes up to `cursor` (cumulative,
    /// spec §6.2) — the relay forwarded the phone's `ack` for **our** stream.
    ///
    /// This is the desktop's only *end-to-end* evidence that the phone is
    /// actually receiving: the link indicator is driven by the relay `pong`,
    /// which measures the desktop↔relay hop and stays healthy while the phone is
    /// dark. The bridge feeds it into an ack-based peer-liveness deadline so a
    /// phone that never acks stops being treated as present (remote-control-5qu).
    ///
    /// A `cursor` of 0 is meaningful and expected: the relay echoes the stored
    /// ack cursor for each activated pairing right after `auth_ok`, so the very
    /// first one may say "your peer has acked nothing". Receiving *any* of these
    /// is what tells the desktop this relay forwards peer acks at all — a relay
    /// built before that change never sends one, and the bridge then leaves the
    /// guard disarmed rather than declaring every phone dark.
    PeerAck {
        /// Pairing whose outbound stream was acknowledged.
        pairing_id: PairingId,
        /// Highest contiguous outbound `seq` the peer has durably handled.
        cursor: u64,
    },
    /// The relay's queue for this pairing overflowed: it shed the oldest
    /// un-acked envelope because the peer has not drained ~1000 of them
    /// (`rate_limited`, spec §6 amendment). Independent of [`Self::PeerAck`] —
    /// and available from every already-deployed relay — this is proof that the
    /// peer is not consuming what we send, so the bridge treats it as immediate
    /// loss of peer liveness instead of ignoring the advisory and shovelling on
    /// (remote-control-5qu).
    PeerBacklog {
        /// The pairing whose queue is overflowing.
        pairing_id: PairingId,
    },
    /// The relay handshake ended before `auth_ok`, so the connection never went
    /// live: no pairing code can be minted and no envelope can flow. Carries a
    /// short, human-readable `reason` so the pairing overlay can say *why*
    /// instead of sitting on "Requesting a pairing code from the relay…"
    /// forever while the supervisor backoff-loops invisibly.
    ///
    /// `retrying` separates the two kinds of failure the UI must word
    /// differently:
    /// * `true` — a reconnect could plausibly fix it (DNS/TCP/TLS failure, the
    ///   relay closing or timing out mid-handshake). The overlay keeps waiting
    ///   and just explains the delay.
    /// * `false` — the relay actively *refused* this device (a missing or wrong
    ///   `relay_password`, an unknown device). No amount of backoff clears a
    ///   configuration problem, so the overlay fails the attempt and says so.
    HandshakeFailed {
        /// Short human-readable cause, safe to show in the UI (no secrets).
        reason: String,
        /// Whether reconnecting could plausibly succeed (see above).
        retrying: bool,
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
