//! Routing seam — **deliberately empty**.
//!
//! This module is where pairing-ID based ciphertext routing plugs in, once a
//! later task implements it. Documenting the expected shape here so this
//! scaffold doesn't have to be re-discovered:
//!
//! - **Registry**: a map from pairing ID to the desktop's live outbound
//!   `WebSocket` connection, so a phone connecting for the same pairing ID
//!   can be matched to it (and vice versa — either side may connect first).
//! - **Zero-knowledge forwarding**: frames are forwarded as opaque bytes.
//!   The relay must never deserialize, inspect, or log payload contents —
//!   see `specs/MOBILE_REMOTE_PRD.md` §9.1 ("zero-knowledge broker" /
//!   "blind pipe"). This is a hard security invariant, not a style choice.
//! - **Backpressure & queuing**: what happens to a message when its peer
//!   leg is absent. Per PRD §5.8 ("queued notifications"), the relay holds
//!   pending events and delivers them on reconnect (deduplicated,
//!   best-effort) rather than dropping them.
//! - **Auth boundary**: verifying the per-device identity keypair presented
//!   at connect time is a separate (auth) task's responsibility. This module
//!   only routes traffic for connections the auth layer has already accepted
//!   — it should not need to know how a connection was authenticated, only
//!   which pairing ID it belongs to.
//! - **Multi-Mac readiness**: per PRD §9.1, the routing model must key on
//!   (pairing ID → desktop) such that one-phone-to-many-Macs is a UI
//!   addition later, not a protocol change here.
//!
//! For now, `handlers::ws_handler` accepts a bare WebSocket connection with
//! no pairing ID and no forwarding: it just answers protocol-level pings
//! (handled automatically by the underlying `tungstenite` stack) and closes
//! cleanly when the peer closes or drops.
