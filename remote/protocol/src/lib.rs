//! FlightDeck Remote wire protocol — shared types for the relay, the FlightDeck
//! desktop app, and (mirrored in Swift) the iOS app.
//!
//! The protocol has **two cleanly separated planes**:
//!
//! * [`relay`] — plaintext, content-free frames between an endpoint and the
//!   hosted relay: version negotiation, device auth, pairing bootstrap, presence,
//!   queued delivery with per-pairing sequence numbers + resume, acks, latency
//!   ping/pong, push-token registration, desktop-requested encrypted pushes
//!   (`push_notify`, plus its HTTPS twin [`relay::PushRequest`]), and errors.
//! * [`e2e`] — the application messages between phone and desktop. These are the
//!   *plaintext*; they are serialized, then encrypted by a layer this crate does
//!   not implement, and carried as opaque ciphertext inside
//!   [`relay::EncryptedEnvelope`]. The relay never sees them. The one
//!   exception to "inside an envelope" is [`e2e::PushPayload`], which is sealed
//!   under a separate push key and carried by a push notification.
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
    AgentStatus, AgentType, GitFileChange, GitFileStatus, GitIndicators, GitStatusDetail,
    PushClass, Role, RollupDot, DIRECT_LINK_MIN_VERSION, MAX_SUPPORTED_VERSION,
    MIN_SUPPORTED_VERSION, PROTOCOL_VERSION, PUSH_NOTIFY_MIN_VERSION,
};
pub use e2e::{
    ActivityKind, AgentEvent, CommandAck, CommandBody, CommandOutcome, DeepLink, DesktopToPhone,
    EventKind, PermissionChoice, PermissionOption, PhoneCommand, ProjectRollup, ProjectState,
    PromptKind, PromptQuestion, PushPayload, QuestionAnswer, RollupUpdate, SessionState,
    SessionStatusDelta, ShellEvent, ShellEventKind, ShellOutput, ShellStream, StateSnapshot,
    StatusRollup, StatusUpdate, TranscriptFeed, TranscriptItem, FEATURE_IMAGE_ATTACH,
    MAX_IMAGE_CHUNK_BYTES,
};
pub use ids::{
    CommandId, DeviceId, EventId, ItemId, PairingId, ProjectId, PromptId, SessionId, ShellId,
};
pub use relay::{
    direct_auth_message, push_endpoint_url, push_request_signing_message, ApnsEnvironment,
    ClientInfo, DirectEndpoint, EncryptedEnvelope, PresenceState, PushErrorBody, PushNotifyBody,
    PushRequest, RelayErrorCode, RelayFrame, RelayMode,
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

    fn sample_push() -> PushNotifyBody {
        PushNotifyBody {
            pairing_id: PairingId::new("pair_x"),
            event_id: EventId::new("evt_1"),
            class: PushClass::NeedsInput,
            nonce: "bm9uY2U=".into(),
            ciphertext: "Y2lwaGVy".into(),
        }
    }

    #[test]
    fn push_notify_frame_flattens_fields() {
        // Like `Envelope`, the `PushNotify` newtype variant must put the body's
        // fields next to the `type` tag, not under a key.
        let frame = RelayFrame::PushNotify(sample_push());
        let v = serde_json::to_value(&frame).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "type": "push_notify",
                "pairing_id": "pair_x",
                "event_id": "evt_1",
                "class": "needs_input",
                "nonce": "bm9uY2U=",
                "ciphertext": "Y2lwaGVy",
            })
        );
        assert_eq!(serde_json::from_value::<RelayFrame>(v).unwrap(), frame);
    }

    #[test]
    fn push_request_signing_message_is_canonical() {
        let req = PushRequest {
            device_id: DeviceId::new("dev_mac"),
            timestamp_ms: 1_730_000_000_000,
            push: sample_push(),
            signature: "c2ln".into(),
        };
        let expected = "flightdeck-remote-push-v1\ndev_mac\n1730000000000\npair_x\nevt_1\nneeds_input\nbm9uY2U=\nY2lwaGVy";
        assert_eq!(req.signing_message(), expected);
        assert_eq!(
            push_request_signing_message(&req.device_id, req.timestamp_ms, &req.push),
            expected
        );
        assert!(!expected.ends_with('\n'), "no trailing newline");
        // A negative timestamp is still rendered base-10, sign included.
        assert!(push_request_signing_message(&req.device_id, -5, &req.push).contains("\n-5\n"));
    }

    #[test]
    fn direct_auth_message_prefixes_the_domain() {
        let nonce = [0u8, 1, 2, 0xff];
        let msg = direct_auth_message(&nonce);
        assert_eq!(&msg[..28], b"flightdeck-remote-direct-v1:");
        assert_eq!(&msg[28..], &nonce);
        assert_eq!(
            direct_auth_message(&[]),
            b"flightdeck-remote-direct-v1:".to_vec()
        );
    }

    #[test]
    fn push_endpoint_url_derives_from_relay_url() {
        assert_eq!(
            push_endpoint_url("wss://relay.example.com/v1/ws").as_deref(),
            Some("https://relay.example.com/v1/push")
        );
        assert_eq!(
            push_endpoint_url("ws://127.0.0.1:8080").as_deref(),
            Some("http://127.0.0.1:8080/v1/push")
        );
        assert_eq!(
            push_endpoint_url("wss://relay.example.com:8443?x=1").as_deref(),
            Some("https://relay.example.com:8443/v1/push")
        );
        assert_eq!(push_endpoint_url("https://relay.example.com"), None);
        assert_eq!(push_endpoint_url("wss:///path"), None);
    }

    #[test]
    fn direct_endpoint_ws_url() {
        let ep = DirectEndpoint {
            host: "ruuds-mbp.tail1234.ts.net".into(),
            port: 47_800,
        };
        assert_eq!(
            ep.ws_url(),
            "ws://ruuds-mbp.tail1234.ts.net:47800/remote/v1"
        );
        let v6 = DirectEndpoint {
            host: "fd7a:115c:a1e0::1".into(),
            port: 1,
        };
        assert_eq!(v6.ws_url(), "ws://[fd7a:115c:a1e0::1]:1/remote/v1");
    }

    #[test]
    fn relay_mode_parses_wire_spelling() {
        assert_eq!("full".parse::<RelayMode>(), Ok(RelayMode::Full));
        assert_eq!("push_only".parse::<RelayMode>(), Ok(RelayMode::PushOnly));
        assert!("pushonly".parse::<RelayMode>().is_err());
        assert_eq!(RelayMode::default(), RelayMode::Full);
        for mode in [RelayMode::Full, RelayMode::PushOnly] {
            assert_eq!(serde_json::to_value(mode).unwrap(), mode.as_str());
        }
        assert_eq!(
            serde_json::to_value(RelayErrorCode::RelayModePushOnly).unwrap(),
            "relay_mode_push_only"
        );
    }

    #[test]
    fn push_aad_is_canonical() {
        assert_eq!(
            e2e::push_aad(&PairingId::new("pair_x"), &EventId::new("evt_1")),
            "push:pair_x:evt_1"
        );
    }

    #[test]
    fn truncate_push_text_respects_byte_budget_and_char_boundaries() {
        use e2e::truncate_push_text;
        assert_eq!(truncate_push_text("short", 120), "short");
        assert_eq!(truncate_push_text("abcdef", 6), "abcdef");
        // 7 bytes into 6: keep 3 bytes, then the 3-byte ellipsis.
        assert_eq!(truncate_push_text("abcdefg", 6), "abc\u{2026}");
        // A multi-byte char straddling the cut is dropped whole.
        let t = truncate_push_text("ab\u{00e9}\u{00e9}\u{00e9}", 6); // a b é é é = 8 bytes
        assert_eq!(t, "ab\u{2026}");
        assert!(t.len() <= 6);
        // Budget too small for the mark: longest fitting prefix, no mark.
        assert_eq!(truncate_push_text("abcdef", 2), "ab");
        for max in 0..40 {
            let out = truncate_push_text("na\u{00ef}ve \u{1f680} rocket caf\u{00e9}", max);
            assert!(out.len() <= max, "max {max}: {out:?}");
        }
    }

    #[test]
    fn phone_command_flattens_body_beside_command_id() {
        let cmd = PhoneCommand {
            command_id: CommandId::new("cmd_1"),
            issued_at_ms: 999,
            body: CommandBody::Reply {
                session_id: SessionId::new("sess_1"),
                text: "go".into(),
                image_ids: Vec::new(),
            },
        };
        let v = serde_json::to_value(&cmd).unwrap();
        assert_eq!(v["command_id"], "cmd_1");
        assert_eq!(v["type"], "reply");
        assert_eq!(v["session_id"], "sess_1");
        assert!(
            v.get("image_ids").is_none(),
            "a plain reply is byte-identical to the pre-image wire"
        );
        assert_eq!(serde_json::from_value::<PhoneCommand>(v).unwrap(), cmd);
    }

    #[test]
    fn image_attach_is_additive_on_the_wire() {
        // A reply naming uploaded images, and one chunk of an upload.
        let reply: CommandBody = serde_json::from_value(serde_json::json!({
            "type": "reply", "session_id": "s", "text": "look", "image_ids": ["up_1"]
        }))
        .unwrap();
        assert_eq!(
            reply,
            CommandBody::Reply {
                session_id: SessionId::new("s"),
                text: "look".into(),
                image_ids: vec!["up_1".into()],
            }
        );
        let chunk = CommandBody::AttachImageChunk {
            session_id: SessionId::new("s"),
            upload_id: "up_1".into(),
            format: "jpg".into(),
            index: 0,
            count: 2,
            data: "AAEC".into(),
        };
        let v = serde_json::to_value(&chunk).unwrap();
        assert_eq!(v["type"], "attach_image_chunk");
        assert_eq!(serde_json::from_value::<CommandBody>(v).unwrap(), chunk);

        // A snapshot from an older desktop has no features; one without any
        // features serializes without the key.
        let old: StateSnapshot =
            serde_json::from_value(serde_json::json!({"server_time_ms": 1, "projects": []}))
                .unwrap();
        assert!(old.features.is_empty());
        assert!(serde_json::to_value(&old)
            .unwrap()
            .get("features")
            .is_none());
    }

    #[test]
    fn ids_are_distinct_types_but_transparent_json() {
        // Transparent: an id is just a JSON string on the wire.
        let sid = SessionId::new("s");
        assert_eq!(serde_json::to_value(&sid).unwrap(), serde_json::json!("s"));
    }
}
