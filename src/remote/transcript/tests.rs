//! Tests for the session-file (JSONL) transcript reconstruction.

use super::*;

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
    b.sync_jsonl(f.path(), 0);

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
    b.sync_jsonl(f.path(), 0);
    assert_eq!(b.total(), 0, "none of these are conversation prose");
}

#[test]
fn tails_newly_appended_records() {
    let f = NamedTempFile::new().unwrap();
    append(&f, &[USER]);

    let mut b = builder();
    b.sync_jsonl(f.path(), 0);
    let first = b.take_appended().expect("first item");
    assert_eq!(labels(&first), vec!["user:Fix the login bug"]);
    assert_eq!(first.from_index, 0);

    // The agent writes its reply; a later sync picks up only the new record.
    append(&f, &[ASSISTANT_TEXT]);
    b.sync_jsonl(f.path(), 0);
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
    b.sync_jsonl(f.path(), 0);
    assert_eq!(b.total(), 2);
    let _ = b.take_appended();

    // Re-syncing the unchanged file is a no-op (offset + uuid dedup).
    b.sync_jsonl(f.path(), 0);
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
    b.sync_jsonl(f.path(), 0);
    assert_eq!(b.total(), 1, "only the complete first line is consumed");

    // Once the record is completed, the next sync ingests it.
    {
        let mut h = std::fs::OpenOptions::new()
            .append(true)
            .open(f.path())
            .unwrap();
        writeln!(h, r#""text":"done"}}]}}}}"#).unwrap();
    }
    b.sync_jsonl(f.path(), 0);
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
    b.sync_jsonl(f.path(), 0);
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
    b.sync_jsonl(f1.path(), 0);
    assert_eq!(b.total(), 1);

    // A different path (a fresh session) resets and re-reads from scratch.
    let f2 = NamedTempFile::new().unwrap();
    append(&f2, &[ASSISTANT_TEXT]);
    b.sync_jsonl(f2.path(), 0);
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
    b.sync_jsonl(f.path(), 999); // fallback that must NOT be used
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
    b.sync_jsonl(std::path::Path::new("/no/such/session.jsonl"), 0);
    assert_eq!(b.total(), 0);
}
