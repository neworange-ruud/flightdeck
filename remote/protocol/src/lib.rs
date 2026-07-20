//! FlightDeck Remote wire protocol — shared types for the relay, the FlightDeck
//! desktop app, and (mirrored in Swift) the iOS app.
//!
//! The protocol has **two cleanly separated planes**:
//!
//! * [`relay`] — plaintext, content-free frames between an endpoint and the
//!   hosted relay: version negotiation, device auth, pairing bootstrap, presence,
//!   queued delivery with per-pairing sequence numbers + resume, acks, latency
//!   ping/pong, push-token registration, and errors.
//! * [`e2e`] — the application messages between phone and desktop. These are the
//!   *plaintext*; they are serialized, then encrypted by a layer this crate does
//!   not implement, and carried as opaque ciphertext inside
//!   [`relay::EncryptedEnvelope`]. The relay never sees them.
//!
//! Wire format for v1 is JSON. Enums are **internally tagged** by a `type` field
//! (or `state` for [`common::AgentStatus`]), and all names are `snake_case`.
//! Binary blobs (ciphertext, nonces, keys, signatures) are base64 strings.
//!
//! The golden JSON fixtures under `tests/fixtures/` pin the wire format and are
//! the cross-language contract the Swift mirror is checked against.
//!
//! See `specs/REMOTE_PROTOCOL.md` for the full narrative spec.

pub mod common;
pub mod e2e;
pub mod ids;
pub mod relay;

pub use common::{
    AgentStatus, AgentType, GitFileChange, GitFileStatus, GitIndicators, GitStatusDetail, Role,
    RollupDot, MAX_SUPPORTED_VERSION, MIN_SUPPORTED_VERSION, PROTOCOL_VERSION,
};
pub use e2e::{
    ActivityKind, AgentEvent, CommandAck, CommandBody, CommandOutcome, DeepLink, DesktopToPhone,
    EventKind, PermissionChoice, PermissionOption, PhoneCommand, ProjectRollup, ProjectState,
    PromptKind,
    RollupUpdate, SessionState, SessionStatusDelta, ShellEvent, ShellEventKind, ShellOutput,
    ShellStream, StateSnapshot, StatusRollup, StatusUpdate, TranscriptFeed, TranscriptItem,
};
pub use ids::{
    CommandId, DeviceId, EventId, ItemId, PairingId, ProjectId, PromptId, SessionId, ShellId,
};
pub use relay::{
    ApnsEnvironment, ClientInfo, EncryptedEnvelope, PresenceState, RelayErrorCode, RelayFrame,
};

/// Returns the protocol version both peers will use, or `None` if the ranges do
/// not overlap. Negotiation rule: the highest version supported by both. A relay
/// or desktop implements this against the peer's advertised `protocol_version`.
///
/// `local_min`/`local_max` are this build's supported range (see
/// [`MIN_SUPPORTED_VERSION`]/[`MAX_SUPPORTED_VERSION`]); `peer_version` is the
/// single version the peer advertised in its `hello`.
pub fn negotiate_version(local_min: u16, local_max: u16, peer_version: u16) -> Option<u16> {
    if peer_version < local_min {
        // Peer is too old for us. If the peer's max were >= local_min we could
        // still meet, but `hello` advertises a single preferred version, so we
        // treat below-min as incompatible.
        None
    } else if peer_version > local_max {
        // Peer prefers newer than we support; fall back to our max.
        Some(local_max)
    } else {
        // Peer's preferred version is within our range; use it.
        Some(peer_version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_negotiation() {
        // Same version: agree.
        assert_eq!(negotiate_version(1, 1, 1), Some(1));
        // Peer newer than us: fall back to our max.
        assert_eq!(negotiate_version(1, 1, 2), Some(1));
        // Peer within a wider local range: use the peer's preferred version.
        assert_eq!(negotiate_version(1, 3, 2), Some(2));
        // Peer older than our floor: incompatible.
        assert_eq!(negotiate_version(2, 3, 1), None);
    }

    #[test]
    fn manual_status_carries_label() {
        let s = AgentStatus::Manual {
            label: "reviewing".into(),
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["state"], "manual");
        assert_eq!(v["label"], "reviewing");
        assert_eq!(serde_json::from_value::<AgentStatus>(v).unwrap(), s);
    }

    #[test]
    fn envelope_frame_flattens_fields() {
        // The `Envelope` newtype variant must flatten EncryptedEnvelope fields
        // next to the `type` tag (not nest them under a key).
        let frame = RelayFrame::Envelope(EncryptedEnvelope {
            pairing_id: PairingId::new("pair_x"),
            seq: 7,
            sender: Role::Phone,
            sent_at_ms: 123,
            nonce: "bm9uY2U=".into(),
            ciphertext: "Y2lwaGVy".into(),
        });
        let v = serde_json::to_value(&frame).unwrap();
        assert_eq!(v["type"], "envelope");
        assert_eq!(v["pairing_id"], "pair_x");
        assert_eq!(v["seq"], 7);
        assert!(v.get("0").is_none(), "must not nest the newtype payload");
        assert_eq!(serde_json::from_value::<RelayFrame>(v).unwrap(), frame);
    }

    #[test]
    fn phone_command_flattens_body_beside_command_id() {
        let cmd = PhoneCommand {
            command_id: CommandId::new("cmd_1"),
            issued_at_ms: 999,
            body: CommandBody::Reply {
                session_id: SessionId::new("sess_1"),
                text: "go".into(),
            },
        };
        let v = serde_json::to_value(&cmd).unwrap();
        assert_eq!(v["command_id"], "cmd_1");
        assert_eq!(v["type"], "reply");
        assert_eq!(v["session_id"], "sess_1");
        assert_eq!(serde_json::from_value::<PhoneCommand>(v).unwrap(), cmd);
    }

    #[test]
    fn ids_are_distinct_types_but_transparent_json() {
        // Transparent: an id is just a JSON string on the wire.
        let sid = SessionId::new("s");
        assert_eq!(serde_json::to_value(&sid).unwrap(), serde_json::json!("s"));
    }
}
