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
    CommandBody, DesktopToPhone, PermissionOption, PhoneCommand, PromptKind, PromptQuestion,
    QuestionAnswer, RelayFrame, TranscriptItem,
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
        multi_select: false,
        questions: vec![],
        at_ms: 1752412740000,
    };
    let v = serde_json::to_value(&item).unwrap();
    assert_eq!(v["kind"], "question");
    assert_eq!(v["allow_free_text"], true);
    assert_eq!(v["multi_select"], false);
    assert_eq!(v["options"][2]["index"], 2);
    assert!(v["options"][0].get("choice").is_none());
    assert!(
        v.get("questions").is_none(),
        "an empty questions list is omitted from the wire"
    );
    assert_eq!(serde_json::from_value::<TranscriptItem>(v).unwrap(), item);
}

/// A multi-select (checklist) `PermissionPrompt` with `multi_select = true`
/// survives a JSON round-trip, and a v2 payload without the field defaults to
/// single-select.
#[test]
fn multi_select_question_round_trips() {
    let item = TranscriptItem::PermissionPrompt {
        item_id: ItemId::new("item_ms1"),
        prompt_id: PromptId::new("prompt_ms1"),
        kind: PromptKind::Question,
        command: "Which checks should run before merge?".into(),
        options: vec![
            PermissionOption {
                index: 0,
                choice: None,
                label: "Tests".into(),
                description: None,
            },
            PermissionOption {
                index: 1,
                choice: None,
                label: "Clippy".into(),
                description: None,
            },
        ],
        allow_free_text: false,
        multi_select: true,
        questions: vec![],
        at_ms: 1752412740000,
    };
    let v = serde_json::to_value(&item).unwrap();
    assert_eq!(v["multi_select"], true);
    assert_eq!(serde_json::from_value::<TranscriptItem>(v).unwrap(), item);

    // A v2 frame (no `multi_select`) parses as single-select.
    let legacy = serde_json::json!({
        "type": "permission_prompt",
        "item_id": "item_legacy",
        "prompt_id": "prompt_legacy",
        "kind": "question",
        "command": "Pick one.",
        "options": [{ "index": 0, "label": "A" }],
        "at_ms": 1752412740000i64,
    });
    match serde_json::from_value::<TranscriptItem>(legacy).unwrap() {
        TranscriptItem::PermissionPrompt { multi_select, .. } => assert!(!multi_select),
        other => panic!("expected PermissionPrompt, got {other:?}"),
    }
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
            option_indices: None,
            free_text: None,
            answers: None,
        },
    };
    let v = serde_json::to_value(&cmd).unwrap();
    assert_eq!(v["type"], "permission_decision");
    assert_eq!(v["option_index"], 2);
    assert!(v.get("choice").is_none(), "binary choice must be omitted");
    assert!(v.get("option_indices").is_none());
    assert!(v.get("free_text").is_none());
    assert!(v.get("answers").is_none());
    assert_eq!(serde_json::from_value::<PhoneCommand>(v).unwrap(), cmd);
}

/// A multi-select `PermissionDecision` answered by option indices round-trips
/// and omits the single-select `option_index`/`choice` fields.
#[test]
fn option_indices_decision_round_trips() {
    let cmd = PhoneCommand {
        command_id: CommandId::new("cmd_ois"),
        issued_at_ms: 1752412811000,
        body: CommandBody::PermissionDecision {
            session_id: SessionId::new("sess_fix_login"),
            prompt_id: PromptId::new("prompt_ms1"),
            choice: None,
            option_index: None,
            option_indices: Some(vec![0, 2]),
            free_text: None,
            answers: None,
        },
    };
    let v = serde_json::to_value(&cmd).unwrap();
    assert_eq!(v["type"], "permission_decision");
    assert_eq!(v["option_indices"][0], 0);
    assert_eq!(v["option_indices"][1], 2);
    assert!(v.get("choice").is_none());
    assert!(v.get("option_index").is_none());
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
            option_indices: None,
            free_text: Some("Use CockroachDB instead.".into()),
            answers: None,
        },
    };
    let v = serde_json::to_value(&cmd).unwrap();
    assert_eq!(v["type"], "permission_decision");
    assert_eq!(v["free_text"], "Use CockroachDB instead.");
    assert!(v.get("choice").is_none());
    assert!(v.get("option_index").is_none());
    assert_eq!(serde_json::from_value::<PhoneCommand>(v).unwrap(), cmd);
}

/// A multi-question `PermissionPrompt` (a Claude `AskUserQuestion` carrying
/// several question tabs) round-trips, and an older payload without the
/// `questions` list still parses (empty list → the flat single-question fields
/// apply).
#[test]
fn multi_question_prompt_round_trips() {
    let item = TranscriptItem::PermissionPrompt {
        item_id: ItemId::new("item_mq1"),
        prompt_id: PromptId::new("prompt_mq1"),
        kind: PromptKind::Question,
        // Flat fields mirror the first question, for a pre-v4 consumer.
        command: "Which database?".into(),
        options: vec![PermissionOption {
            index: 0,
            choice: None,
            label: "Postgres".into(),
            description: None,
        }],
        allow_free_text: true,
        multi_select: false,
        questions: vec![
            PromptQuestion {
                header: Some("Database".into()),
                question: "Which database?".into(),
                options: vec![
                    PermissionOption {
                        index: 0,
                        choice: None,
                        label: "Postgres".into(),
                        description: None,
                    },
                    PermissionOption {
                        index: 1,
                        choice: None,
                        label: "SQLite".into(),
                        description: None,
                    },
                ],
                multi_select: false,
            },
            PromptQuestion {
                header: Some("Checks".into()),
                question: "Which checks run before merge?".into(),
                options: vec![
                    PermissionOption {
                        index: 0,
                        choice: None,
                        label: "Tests".into(),
                        description: None,
                    },
                    PermissionOption {
                        index: 1,
                        choice: None,
                        label: "Clippy".into(),
                        description: None,
                    },
                ],
                multi_select: true,
            },
        ],
        at_ms: 1752412740000,
    };
    let v = serde_json::to_value(&item).unwrap();
    assert_eq!(v["questions"][0]["header"], "Database");
    assert_eq!(v["questions"][1]["multi_select"], true);
    assert_eq!(serde_json::from_value::<TranscriptItem>(v).unwrap(), item);

    // A payload without `questions` parses with an empty questions list.
    let legacy = serde_json::json!({
        "type": "permission_prompt",
        "item_id": "item_legacy",
        "prompt_id": "prompt_legacy",
        "kind": "question",
        "command": "Pick one.",
        "options": [{ "index": 0, "label": "A" }],
        "at_ms": 1752412740000i64,
    });
    match serde_json::from_value::<TranscriptItem>(legacy).unwrap() {
        TranscriptItem::PermissionPrompt { questions, .. } => assert!(questions.is_empty()),
        other => panic!("expected PermissionPrompt, got {other:?}"),
    }
}

/// A multi-question `PermissionDecision` (`answers`) round-trips, and a v3
/// payload without `answers` parses with `answers = None`.
#[test]
fn answers_decision_round_trips() {
    let cmd = PhoneCommand {
        command_id: CommandId::new("cmd_ans"),
        issued_at_ms: 1752412811000,
        body: CommandBody::PermissionDecision {
            session_id: SessionId::new("sess_fix_login"),
            prompt_id: PromptId::new("prompt_mq1"),
            choice: None,
            option_index: None,
            option_indices: None,
            free_text: None,
            answers: Some(vec![
                QuestionAnswer {
                    option_indices: vec![0],
                },
                QuestionAnswer {
                    option_indices: vec![0, 1],
                },
            ]),
        },
    };
    let v = serde_json::to_value(&cmd).unwrap();
    assert_eq!(v["type"], "permission_decision");
    assert_eq!(v["answers"][0]["option_indices"][0], 0);
    assert_eq!(v["answers"][1]["option_indices"][1], 1);
    assert!(v.get("option_index").is_none());
    assert_eq!(serde_json::from_value::<PhoneCommand>(v).unwrap(), cmd);

    // A payload without `answers` parses as None.
    let legacy = serde_json::json!({
        "type": "permission_decision",
        "session_id": "sess_x",
        "prompt_id": "prompt_x",
        "option_index": 1,
    });
    match serde_json::from_value::<CommandBody>(legacy).unwrap() {
        CommandBody::PermissionDecision { answers, .. } => assert!(answers.is_none()),
        other => panic!("expected PermissionDecision, got {other:?}"),
    }
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
