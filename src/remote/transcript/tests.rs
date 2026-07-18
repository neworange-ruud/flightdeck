//! Tests for the session-file (JSONL) transcript reconstruction.

use super::*;

use crate::agents::resume::SessionFormat;
use std::io::Write;
use tempfile::NamedTempFile;

fn builder() -> TranscriptBuilder {
    TranscriptBuilder::new(SessionId::new("s1"))
}

/// Append JSONL lines to `f`.
fn append(f: &NamedTempFile, lines: &[&str]) {
    let mut h = std::fs::OpenOptions::new()
        .append(true)
        .open(f.path())
        .unwrap();
    for l in lines {
        writeln!(h, "{l}").unwrap();
    }
}

/// A compact label per item, for order-sensitive assertions.
fn labels(feed: &TranscriptFeed) -> Vec<String> {
    feed.items
        .iter()
        .map(|i| match i {
            TranscriptItem::UserMessage { text, .. } => format!("user:{text}"),
            TranscriptItem::AgentMessage { text, .. } => format!("agent:{text}"),
            TranscriptItem::Activity { summary, kind, .. } => format!("act[{kind:?}]:{summary}"),
            TranscriptItem::PermissionPrompt { command, .. } => format!("prompt:{command}"),
        })
        .collect()
}

const USER: &str = r#"{"type":"user","uuid":"u1","timestamp":"2026-07-18T18:00:00.000Z","message":{"content":"Fix the login bug"}}"#;
const ASSISTANT_TEXT: &str = r#"{"type":"assistant","uuid":"a1","timestamp":"2026-07-18T18:00:01.000Z","message":{"content":[{"type":"text","text":"On it — reading the code."}]}}"#;
const ASSISTANT_TOOL: &str = r#"{"type":"assistant","uuid":"a2","timestamp":"2026-07-18T18:00:02.000Z","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test","description":"Run the tests"}}]}}"#;

#[test]
fn reconstructs_user_assistant_and_activity() {
    let f = NamedTempFile::new().unwrap();
    append(&f, &[USER, ASSISTANT_TEXT, ASSISTANT_TOOL]);

    let mut b = builder();
    b.sync_jsonl(f.path(), SessionFormat::Claude, 0);

    assert_eq!(b.total(), 3);
    assert_eq!(
        labels(&b.load(None)),
        vec![
            "user:Fix the login bug",
            "agent:On it — reading the code.",
            "act[Command]:Run the tests",
        ]
    );
}

#[test]
fn skips_tool_results_meta_and_sidechain() {
    let f = NamedTempFile::new().unwrap();
    append(
        &f,
        &[
            // a tool_result-only user record → not user prose
            r#"{"type":"user","uuid":"r1","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            // an injected meta record
            r#"{"type":"user","uuid":"m1","isMeta":true,"message":{"content":[{"type":"text","text":"<system>"}]}}"#,
            // a subagent (sidechain) turn
            r#"{"type":"assistant","uuid":"s1","isSidechain":true,"message":{"content":[{"type":"text","text":"subagent thinking"}]}}"#,
            // an unrelated record type
            r#"{"type":"file-history-snapshot","uuid":"x1"}"#,
        ],
    );

    let mut b = builder();
    b.sync_jsonl(f.path(), SessionFormat::Claude, 0);
    assert_eq!(b.total(), 0, "none of these are conversation prose");
}

#[test]
fn tails_newly_appended_records() {
    let f = NamedTempFile::new().unwrap();
    append(&f, &[USER]);

    let mut b = builder();
    b.sync_jsonl(f.path(), SessionFormat::Claude, 0);
    let first = b.take_appended().expect("first item");
    assert_eq!(labels(&first), vec!["user:Fix the login bug"]);
    assert_eq!(first.from_index, 0);

    // The agent writes its reply; a later sync picks up only the new record.
    append(&f, &[ASSISTANT_TEXT]);
    b.sync_jsonl(f.path(), SessionFormat::Claude, 0);
    let second = b.take_appended().expect("appended item");
    assert_eq!(labels(&second), vec!["agent:On it — reading the code."]);
    assert_eq!(
        second.from_index, 1,
        "append continues from the running ordinal"
    );
}

#[test]
fn resync_without_growth_adds_nothing() {
    let f = NamedTempFile::new().unwrap();
    append(&f, &[USER, ASSISTANT_TEXT]);

    let mut b = builder();
    b.sync_jsonl(f.path(), SessionFormat::Claude, 0);
    assert_eq!(b.total(), 2);
    let _ = b.take_appended();

    // Re-syncing the unchanged file is a no-op (offset + uuid dedup).
    b.sync_jsonl(f.path(), SessionFormat::Claude, 0);
    assert_eq!(b.total(), 2);
    assert!(b.take_appended().is_none());
}

#[test]
fn ignores_a_half_written_trailing_line() {
    let f = NamedTempFile::new().unwrap();
    append(&f, &[USER]);
    // A partial record with no trailing newline (mid-write).
    {
        let mut h = std::fs::OpenOptions::new()
            .append(true)
            .open(f.path())
            .unwrap();
        write!(
            h,
            r#"{{"type":"assistant","uuid":"a1","message":{{"content":[{{"type":"text","#
        )
        .unwrap();
    }

    let mut b = builder();
    b.sync_jsonl(f.path(), SessionFormat::Claude, 0);
    assert_eq!(b.total(), 1, "only the complete first line is consumed");

    // Once the record is completed, the next sync ingests it.
    {
        let mut h = std::fs::OpenOptions::new()
            .append(true)
            .open(f.path())
            .unwrap();
        writeln!(h, r#""text":"done"}}]}}}}"#).unwrap();
    }
    b.sync_jsonl(f.path(), SessionFormat::Claude, 0);
    assert_eq!(b.total(), 2);
    assert_eq!(labels(&b.load(None))[1], "agent:done");
}

#[test]
fn needs_input_preview_is_the_last_agent_prose() {
    let f = NamedTempFile::new().unwrap();
    append(
        &f,
        &[
            r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"text","text":"Should I delete the file?"}]}}"#,
        ],
    );

    let mut b = builder();
    b.sync_jsonl(f.path(), SessionFormat::Claude, 0);
    let preview = b.on_needs_input(0);
    assert_eq!(preview.as_deref(), Some("Should I delete the file?"));

    // The inline prompt item is appended and becomes the pending prompt id.
    let items = b.load(None);
    assert!(matches!(
        items.items.last(),
        Some(TranscriptItem::PermissionPrompt { .. })
    ));
    assert!(b.last_prompt_id().is_some());
}

#[test]
fn a_new_session_file_replaces_the_transcript() {
    let f1 = NamedTempFile::new().unwrap();
    append(&f1, &[USER]);
    let mut b = builder();
    b.sync_jsonl(f1.path(), SessionFormat::Claude, 0);
    assert_eq!(b.total(), 1);

    // A different path (a fresh session) resets and re-reads from scratch.
    let f2 = NamedTempFile::new().unwrap();
    append(&f2, &[ASSISTANT_TEXT]);
    b.sync_jsonl(f2.path(), SessionFormat::Claude, 0);
    assert_eq!(b.total(), 1);
    assert_eq!(
        labels(&b.load(None)),
        vec!["agent:On it — reading the code."]
    );
}

#[test]
fn iso8601_parses_to_unix_millis() {
    assert_eq!(parse_iso8601_ms("1970-01-01T00:00:00.000Z"), Some(0));
    assert_eq!(
        parse_iso8601_ms("1970-01-02T00:00:00.000Z"),
        Some(86_400_000)
    );
    // Fractional seconds are milliseconds (right-padded).
    assert_eq!(parse_iso8601_ms("1970-01-01T00:00:00.5Z"), Some(500));
    assert_eq!(parse_iso8601_ms("1970-01-01T00:00:01Z"), Some(1000));
    // A recent, plausible instant is far in the future of the epoch.
    assert!(parse_iso8601_ms("2026-07-18T18:00:00.000Z").unwrap() > 1_700_000_000_000);
    // Garbage is rejected so the caller falls back to wall-clock.
    assert_eq!(parse_iso8601_ms("not-a-timestamp"), None);
}

#[test]
fn timestamp_from_the_record_stamps_the_item() {
    let f = NamedTempFile::new().unwrap();
    append(&f, &[USER]);
    let mut b = builder();
    b.sync_jsonl(f.path(), SessionFormat::Claude, 999); // fallback that must NOT be used
    if let Some(TranscriptItem::UserMessage { at_ms, .. }) = b.load(None).items.first() {
        assert_eq!(
            *at_ms,
            parse_iso8601_ms("2026-07-18T18:00:00.000Z").unwrap()
        );
    } else {
        panic!("expected a user message");
    }
}

#[test]
fn missing_file_is_a_safe_noop() {
    let mut b = builder();
    b.sync_jsonl(
        std::path::Path::new("/no/such/session.jsonl"),
        SessionFormat::Claude,
        0,
    );
    assert_eq!(b.total(), 0);
}

// --- Codex rollout format ---------------------------------------------------
//
// Record shapes below mirror real `~/.codex/sessions/**/rollout-*.jsonl` files:
// prose lives in `event_msg` (`user_message` / `agent_message`), tool activity
// in `response_item` (`function_call` with a JSON-string `arguments`, or
// `custom_tool_call` with a raw `input`). See `ingest_codex`.

/// The leading session_meta line (carries cwd; must be skipped as non-prose).
const CX_META: &str = r#"{"timestamp":"2026-07-18T18:00:00.000Z","type":"session_meta","payload":{"session_id":"019f6f70-388c-7c33-98da-da1f5c43856d","cwd":"/repo/wt"}}"#;
const CX_USER: &str = r#"{"timestamp":"2026-07-18T18:00:01.000Z","type":"event_msg","payload":{"type":"user_message","message":"Explain this codebase"}}"#;
const CX_AGENT: &str = r#"{"timestamp":"2026-07-18T18:00:02.000Z","type":"event_msg","payload":{"type":"agent_message","message":"Here is the overview.","phase":"final_answer"}}"#;
const CX_SHELL: &str = r#"{"timestamp":"2026-07-18T18:00:03.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"cargo test\"],\"workdir\":\".\"}","call_id":"call_1"}}"#;

fn codex(f: &NamedTempFile) -> TranscriptBuilder {
    let mut b = builder();
    b.sync_jsonl(f.path(), SessionFormat::Codex, 0);
    b
}

#[test]
fn codex_reconstructs_prose_and_shell_activity() {
    let f = NamedTempFile::new().unwrap();
    append(&f, &[CX_META, CX_USER, CX_AGENT, CX_SHELL]);

    let b = codex(&f);
    assert_eq!(
        labels(&b.load(None)),
        vec![
            "user:Explain this codebase",
            "agent:Here is the overview.",
            // `["bash","-lc","cargo test"]` summarises as its script.
            "act[Command]:Ran cargo test",
        ],
        "session_meta is skipped; prose + tool pill reconstructed in order"
    );
}

#[test]
fn codex_shell_body_carries_the_command() {
    let f = NamedTempFile::new().unwrap();
    append(&f, &[CX_SHELL]);
    let b = codex(&f);
    match b.load(None).items.first() {
        Some(TranscriptItem::Activity { body, kind, .. }) => {
            assert_eq!(body.as_deref(), Some("cargo test"));
            assert_eq!(*kind, ActivityKind::Command);
        }
        other => panic!("expected a Command activity, got {other:?}"),
    }
}

#[test]
fn codex_apply_patch_names_the_edited_file() {
    let f = NamedTempFile::new().unwrap();
    // `arguments` is JSON-in-JSON: the patch's newlines are `\n`-escaped inside
    // the inner object, i.e. `\\n` in this outer string literal (as on disk).
    let rec = r#"{"type":"response_item","payload":{"type":"function_call","name":"apply_patch","arguments":"{\"input\":\"*** Begin Patch\\n*** Update File: src/main.rs\\n@@\\n-old\\n+new\\n*** End Patch\"}","call_id":"call_2"}}"#;
    append(&f, &[rec]);
    let b = codex(&f);
    assert_eq!(labels(&b.load(None)), vec!["act[Edit]:Edited main.rs"]);
}

#[test]
fn codex_update_plan_is_a_plain_pill() {
    let f = NamedTempFile::new().unwrap();
    let rec = r#"{"type":"response_item","payload":{"type":"function_call","name":"update_plan","arguments":"{\"plan\":[]}","call_id":"call_3"}}"#;
    append(&f, &[rec]);
    let b = codex(&f);
    assert_eq!(labels(&b.load(None)), vec!["act[Other]:Updated the plan"]);
}

#[test]
fn codex_custom_tool_call_uses_raw_input_as_body() {
    let f = NamedTempFile::new().unwrap();
    let rec = r#"{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"text(await tools.web__run({search_query:[{q:\"rust\"}]}))","call_id":"call_4"}}"#;
    append(&f, &[rec]);
    let b = codex(&f);
    match b.load(None).items.first() {
        Some(TranscriptItem::Activity {
            summary,
            body,
            kind,
            ..
        }) => {
            assert_eq!(summary, "Ran exec");
            assert!(body.as_deref().unwrap().contains("web__run"));
            assert_eq!(*kind, ActivityKind::Command);
        }
        other => panic!("expected a Command activity, got {other:?}"),
    }
}

#[test]
fn codex_skips_reasoning_output_and_duplicate_message_records() {
    let f = NamedTempFile::new().unwrap();
    append(
        &f,
        &[
            // encrypted reasoning — never shown
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":[],"encrypted_content":"…"}}"#,
            // the duplicate `message` mirror of agent prose — prose comes from event_msg
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Here is the overview."}]}}"#,
            // injected developer/context message — not user prose
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions>"}]}}"#,
            // tool output — not an activity
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"done"}}"#,
            // token accounting
            r#"{"type":"event_msg","payload":{"type":"token_count","total":42}}"#,
        ],
    );
    let b = codex(&f);
    assert_eq!(b.total(), 0, "none of these are conversation items");
}

#[test]
fn codex_agent_message_becomes_needs_input_preview() {
    let f = NamedTempFile::new().unwrap();
    let rec =
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"Shall I proceed?"}}"#;
    append(&f, &[rec]);
    let mut b = codex(&f);
    assert_eq!(b.on_needs_input(0).as_deref(), Some("Shall I proceed?"));
}

#[test]
fn codex_tails_new_records_and_switching_format_resets() {
    let f = NamedTempFile::new().unwrap();
    append(&f, &[CX_USER]);
    let mut b = codex(&f);
    assert_eq!(
        labels(&b.take_appended().unwrap()),
        vec!["user:Explain this codebase"]
    );

    append(&f, &[CX_AGENT]);
    b.sync_jsonl(f.path(), SessionFormat::Codex, 0);
    assert_eq!(
        labels(&b.take_appended().unwrap()),
        vec!["agent:Here is the overview."]
    );
}
