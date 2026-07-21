//! Golden-fixture round-trip tests.
//!
//! Every `tests/fixtures/<plane>/*.json` file is a hand-written example of one
//! message variant. For each file we:
//!   1. parse it into the matching Rust type,
//!   2. re-serialize the parsed value back to JSON,
//!   3. assert the result is *semantically equal* to the original JSON.
//!
//! This proves (a) every variant deserializes, (b) serialization is lossless and
//! byte-stable in shape, and (c) the fixtures are valid JSON. The fixtures are
//! the cross-language contract: the iOS Swift mirror must produce and consume
//! byte-compatible JSON, so keep them exhaustive and readable.

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use flightdeck_remote_protocol::{
    CommandBody, DesktopToPhone, PermissionOption, PhoneCommand, PromptKind, RelayFrame,
    TranscriptItem,
};
use flightdeck_remote_protocol::{CommandId, ItemId, PromptId, SessionId};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Read every `*.json` file in `dir`, sorted by name, as (name, parsed Value).
fn read_fixtures(dir: &Path) -> Vec<(String, Value)> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    entries.sort();
    assert!(
        !entries.is_empty(),
        "no fixtures found in {}",
        dir.display()
    );
    entries
        .into_iter()
        .map(|p| {
            let raw = fs::read_to_string(&p).unwrap();
            let value: Value = serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", p.display()));
            (p.file_name().unwrap().to_string_lossy().into_owned(), value)
        })
        .collect()
}

/// Round-trip every fixture in `<fixtures>/<plane>` through type `T`.
fn check_plane<T>(plane: &str)
where
    T: DeserializeOwned + Serialize,
{
    let dir = fixtures_dir().join(plane);
    for (name, original) in read_fixtures(&dir) {
        let parsed: T = serde_json::from_value(original.clone())
            .unwrap_or_else(|e| panic!("{plane}/{name}: deserialize failed: {e}"));
        let reserialized =
            serde_json::to_value(&parsed).unwrap_or_else(|e| panic!("{plane}/{name}: {e}"));
        assert_eq!(
            reserialized, original,
            "{plane}/{name}: re-serialized JSON differs from the fixture"
        );
    }
}

#[test]
fn relay_frames_round_trip() {
    check_plane::<RelayFrame>("relay");
}

#[test]
fn desktop_to_phone_round_trips() {
    check_plane::<DesktopToPhone>("desktop_to_phone");
}

#[test]
fn phone_to_desktop_round_trips() {
    check_plane::<PhoneCommand>("phone_to_desktop");
}

/// Every relay fixture must be tagged with a `type` field (internally-tagged
/// invariant) — a quick guard so a malformed fixture can't silently pass by
/// deserializing into the wrong shape.
#[test]
fn relay_fixtures_have_type_tag() {
    for (name, value) in read_fixtures(&fixtures_dir().join("relay")) {
        assert!(
            value.get("type").and_then(Value::as_str).is_some(),
            "relay/{name}: missing string `type` tag"
        );
    }
}

/// Every phone command fixture must carry a `command_id` (the idempotency key)
/// and a flattened `type`.
#[test]
fn phone_commands_carry_command_id_and_type() {
    for (name, value) in read_fixtures(&fixtures_dir().join("phone_to_desktop")) {
        assert!(
            value.get("command_id").and_then(Value::as_str).is_some(),
            "phone_to_desktop/{name}: missing `command_id`"
        );
        assert!(
            value.get("type").and_then(Value::as_str).is_some(),
            "phone_to_desktop/{name}: missing flattened `type`"
        );
    }
}

/// A Question prompt with three described options (no `choice`, free-text
/// allowed) survives a JSON round-trip unchanged.
#[test]
fn question_prompt_round_trips() {
    let item = TranscriptItem::PermissionPrompt {
        item_id: ItemId::new("item_q1"),
        prompt_id: PromptId::new("prompt_q1"),
        kind: PromptKind::Question,
        command: "Which database should the login service use?".into(),
        options: vec![
            PermissionOption {
                index: 0,
                choice: None,
                label: "Postgres".into(),
                description: Some("Use the existing shared Postgres cluster.".into()),
            },
            PermissionOption {
                index: 1,
                choice: None,
                label: "SQLite".into(),
                description: Some("Embed a local SQLite file for now.".into()),
            },
            PermissionOption {
                index: 2,
                choice: None,
                label: "Redis".into(),
                description: Some("Store sessions in Redis with a short TTL.".into()),
            },
        ],
        allow_free_text: true,
        at_ms: 1752412740000,
    };
    let v = serde_json::to_value(&item).unwrap();
    assert_eq!(v["kind"], "question");
    assert_eq!(v["allow_free_text"], true);
    assert_eq!(v["options"][2]["index"], 2);
    assert!(v["options"][0].get("choice").is_none());
    assert_eq!(serde_json::from_value::<TranscriptItem>(v).unwrap(), item);
}

/// A multi-option `PermissionDecision` answered by option index round-trips and
/// omits the binary `choice` field.
#[test]
fn option_index_decision_round_trips() {
    let cmd = PhoneCommand {
        command_id: CommandId::new("cmd_oi"),
        issued_at_ms: 1752412811000,
        body: CommandBody::PermissionDecision {
            session_id: SessionId::new("sess_fix_login"),
            prompt_id: PromptId::new("prompt_q1"),
            choice: None,
            option_index: Some(2),
            free_text: None,
        },
    };
    let v = serde_json::to_value(&cmd).unwrap();
    assert_eq!(v["type"], "permission_decision");
    assert_eq!(v["option_index"], 2);
    assert!(v.get("choice").is_none(), "binary choice must be omitted");
    assert!(v.get("free_text").is_none());
    assert_eq!(serde_json::from_value::<PhoneCommand>(v).unwrap(), cmd);
}

/// A free-text `PermissionDecision` round-trips and omits `choice`/`option_index`.
#[test]
fn free_text_decision_round_trips() {
    let cmd = PhoneCommand {
        command_id: CommandId::new("cmd_ft"),
        issued_at_ms: 1752412811000,
        body: CommandBody::PermissionDecision {
            session_id: SessionId::new("sess_fix_login"),
            prompt_id: PromptId::new("prompt_q1"),
            choice: None,
            option_index: None,
            free_text: Some("Use CockroachDB instead.".into()),
        },
    };
    let v = serde_json::to_value(&cmd).unwrap();
    assert_eq!(v["type"], "permission_decision");
    assert_eq!(v["free_text"], "Use CockroachDB instead.");
    assert!(v.get("choice").is_none());
    assert!(v.get("option_index").is_none());
    assert_eq!(serde_json::from_value::<PhoneCommand>(v).unwrap(), cmd);
}

/// Count guard: keep fixtures exhaustive as variants are added. Bump these when
/// the taxonomy grows (and add the matching fixture!).
#[test]
fn fixture_counts_are_exhaustive() {
    let count = |plane: &str| read_fixtures(&fixtures_dir().join(plane)).len();
    assert_eq!(
        count("relay"),
        24,
        "expected one fixture per RelayFrame variant"
    );
    assert_eq!(
        count("desktop_to_phone"),
        10,
        "expected one fixture per DesktopToPhone variant"
    );
    assert_eq!(
        count("phone_to_desktop"),
        17,
        "expected one fixture per CommandBody variant"
    );
}
