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
fn resolve_source_normalizes_a_base_branch_trailing_dot_cwd() {
    // A base-branch agent's worktree is `repo_root.join(".")` → `…/wt/.`. Claude
    // records its session under the CLEAN `…/wt`, so resolve_source must find it
    // despite the trailing `.`, or the transcript stays permanently empty
    // (remote-control-ou3). The mangled dir below matches the clean path only.
    let home = tempfile::tempdir().unwrap();
    let claude_dir = home.path().join(".claude/projects/-home-u-wt");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("11111111-1111-1111-1111-111111111111.jsonl"),
        "{}\n",
    )
    .unwrap();

    let dotted = std::path::Path::new("/home/u/wt/.");
    match resolve_source("claude", dotted, home.path()) {
        Some(TranscriptSource::Jsonl { path, format }) => {
            assert_eq!(format, SessionFormat::Claude);
            assert!(path.ends_with("11111111-1111-1111-1111-111111111111.jsonl"));
        }
        other => panic!("expected a Claude Jsonl source for a trailing-dot cwd, got {other:?}"),
    }
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

// --- Claude AskUserQuestion (structured prompt) -----------------------------

/// A Claude assistant record whose single content block is an `AskUserQuestion`
/// tool_use with one question and three described options.
const ASK_USER_QUESTION: &str = r#"{"type":"assistant","uuid":"aq1","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[{"question":"Which database should we use?","header":"Database","multiSelect":false,"options":[{"label":"Postgres","description":"Relational, ACID"},{"label":"SQLite","description":"Embedded, zero-config"},{"label":"Redis","description":"In-memory KV"}]}]}}]}}"#;

#[test]
fn ask_user_question_becomes_a_structured_question_prompt() {
    let f = NamedTempFile::new().unwrap();
    append(&f, &[ASK_USER_QUESTION]);

    let mut b = builder();
    b.sync_jsonl(f.path(), SessionFormat::Claude, 0);

    // The AskUserQuestion is surfaced as a Question PermissionPrompt IMMEDIATELY
    // at ingest — not held for a later needs-input edge (remote-control-z30). No
    // Activity pill is emitted for it.
    assert_eq!(b.total(), 1, "AskUserQuestion emits the prompt at ingest");

    // A subsequent needs-input edge for the same (still-unanswered) question does
    // NOT stack a second, binary prompt — it just reports the preview.
    let preview = b.on_needs_input(0);
    assert_eq!(preview.as_deref(), Some("Which database should we use?"));
    assert_eq!(
        b.total(),
        1,
        "needs-input does not duplicate the open question"
    );

    match b.load(None).items.last() {
        Some(TranscriptItem::PermissionPrompt {
            kind,
            command,
            options,
            allow_free_text,
            ..
        }) => {
            assert_eq!(*kind, PromptKind::Question);
            assert_eq!(command, "Which database should we use?");
            assert!(*allow_free_text, "AskUserQuestion always allows free text");
            assert_eq!(options.len(), 3);
            let expect = [
                (0u32, "Postgres", "Relational, ACID"),
                (1, "SQLite", "Embedded, zero-config"),
                (2, "Redis", "In-memory KV"),
            ];
            for (opt, (idx, label, desc)) in options.iter().zip(expect) {
                assert_eq!(opt.index, idx);
                assert_eq!(opt.label, label);
                assert_eq!(opt.description.as_deref(), Some(desc));
                assert!(
                    opt.choice.is_none(),
                    "Question options carry no binary choice"
                );
            }
        }
        other => panic!("expected a Question PermissionPrompt, got {other:?}"),
    }
}

#[test]
fn answering_a_question_lets_a_later_needs_input_edge_prompt_again() {
    // A question surfaced at ingest opens a prompt; a following user turn answers
    // it (clearing the open-prompt guard) so a subsequent needs-input edge for a
    // *new* wait synthesizes its binary prompt as usual (remote-control-z30).
    let f = NamedTempFile::new().unwrap();
    append(&f, &[ASK_USER_QUESTION, USER]);

    let mut b = builder();
    b.sync_jsonl(f.path(), SessionFormat::Claude, 0);
    // Prompt (from ingest) + the user's answer.
    assert_eq!(b.total(), 2);

    // The next needs-input edge is a fresh wait: it emits the binary fallback.
    b.on_needs_input(0);
    assert_eq!(
        b.total(),
        3,
        "a new wait after an answer emits its own prompt"
    );
    match b.load(None).items.last() {
        Some(TranscriptItem::PermissionPrompt { kind, .. }) => {
            assert_eq!(*kind, PromptKind::Permission);
        }
        other => panic!("expected a binary Permission prompt, got {other:?}"),
    }
}

#[test]
fn a_normal_tool_use_still_yields_an_activity_pill() {
    let f = NamedTempFile::new().unwrap();
    append(&f, &[ASSISTANT_TOOL]);

    let mut b = builder();
    b.sync_jsonl(f.path(), SessionFormat::Claude, 0);
    assert_eq!(labels(&b.load(None)), vec!["act[Command]:Run the tests"]);
}

#[test]
fn without_a_structured_prompt_needs_input_is_binary_allow_deny() {
    let f = NamedTempFile::new().unwrap();
    append(&f, &[ASSISTANT_TEXT]);

    let mut b = builder();
    b.sync_jsonl(f.path(), SessionFormat::Claude, 0);
    b.on_needs_input(0);

    match b.load(None).items.last() {
        Some(TranscriptItem::PermissionPrompt {
            kind,
            options,
            allow_free_text,
            ..
        }) => {
            assert_eq!(*kind, PromptKind::Permission);
            assert!(!*allow_free_text);
            assert_eq!(options.len(), 2);
            assert_eq!(options[0].choice, Some(PermissionChoice::AllowOnce));
            assert_eq!(options[1].choice, Some(PermissionChoice::Deny));
        }
        other => panic!("expected a binary Permission prompt, got {other:?}"),
    }
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

// --- OpenCode (SQLite-backed) format ----------------------------------------
//
// OpenCode's conversation lives in a live SQLite DB, joined into `Part` rows
// (id, role, at_ms, data JSON). The DB/SQL layer is tested in
// `remote::opencode::tests`; here we test the translation + streaming gating of
// `ingest_opencode` directly with hand-built parts, and one end-to-end
// `sync_opencode` against a temp DB.

use crate::remote::opencode::Part;

/// A `Part` with a fixed timestamp, `data` parsed from `json`.
fn oc(id: &str, role: &str, json: &str) -> Part {
    Part {
        id: id.to_string(),
        role: role.to_string(),
        at_ms: 1000,
        data: serde_json::from_str(json).unwrap(),
    }
}

#[test]
fn opencode_user_prose_and_final_assistant_prose() {
    let mut b = builder();
    b.ingest_opencode(
        &oc(
            "p1",
            "user",
            r#"{"type":"text","text":"Explain this repo"}"#,
        ),
        0,
    );
    b.ingest_opencode(
        &oc(
            "p2",
            "assistant",
            r#"{"type":"text","text":"Sure — here goes.","time":{"start":1,"end":2}}"#,
        ),
        0,
    );
    assert_eq!(
        labels(&b.load(None)),
        vec!["user:Explain this repo", "agent:Sure — here goes."]
    );
}

#[test]
fn opencode_defers_streaming_assistant_text_until_final() {
    let mut b = builder();
    let streaming = oc(
        "p1",
        "assistant",
        r#"{"type":"text","text":"Read","time":{"start":1}}"#,
    );
    b.ingest_opencode(&streaming, 0);
    assert_eq!(b.total(), 0, "no time.end yet → not emitted, left unseen");

    // The same part, now finalized, is emitted on the next poll.
    let done = oc(
        "p1",
        "assistant",
        r#"{"type":"text","text":"Reading the code.","time":{"start":1,"end":9}}"#,
    );
    b.ingest_opencode(&done, 0);
    assert_eq!(labels(&b.load(None)), vec!["agent:Reading the code."]);
}

#[test]
fn opencode_tool_parts_map_to_pills() {
    let mut b = builder();
    let tool = |tool: &str, input: &str| {
        format!(
            r#"{{"type":"tool","tool":"{tool}","state":{{"status":"completed","input":{input}}}}}"#
        )
    };
    let parts = [
        (
            "t1",
            tool(
                "bash",
                r#"{"command":"cargo test","description":"Run the tests"}"#,
            ),
        ),
        ("t2", tool("edit", r#"{"filePath":"/repo/src/main.rs"}"#)),
        ("t3", tool("read", r#"{"filePath":"/repo/README.md"}"#)),
        ("t4", tool("grep", r#"{"pattern":"TODO"}"#)),
        ("t5", tool("webfetch", r#"{"url":"https://example.com"}"#)),
        ("t6", tool("todowrite", r#"{"todos":[]}"#)),
        ("t7", tool("skill", r#"{"name":"graphify"}"#)),
        // An MCP tool (`server_tool`) has no special case → shows its name.
        ("t8", tool("linear_get_issue", r#"{"id":"ENG-1"}"#)),
    ];
    for (id, data) in &parts {
        b.ingest_opencode(&oc(id, "assistant", data), 0);
    }
    assert_eq!(
        labels(&b.load(None)),
        vec![
            "act[Command]:Run the tests",
            "act[Edit]:Edited main.rs",
            "act[Search]:Read README.md",
            "act[Search]:Searched TODO",
            "act[Search]:Fetched https://example.com",
            "act[Other]:Updated the task list",
            "act[Other]:Skill: graphify",
            "act[Other]:linear_get_issue",
        ]
    );
}

#[test]
fn opencode_pending_tool_is_deferred_until_settled() {
    let mut b = builder();
    let running = oc(
        "t1",
        "assistant",
        r#"{"type":"tool","tool":"bash","state":{"status":"running","input":{"command":"sleep 1"}}}"#,
    );
    b.ingest_opencode(&running, 0);
    assert_eq!(b.total(), 0, "a running tool call is not shown yet");

    let done = oc(
        "t1",
        "assistant",
        r#"{"type":"tool","tool":"bash","state":{"status":"completed","input":{"command":"sleep 1"}}}"#,
    );
    b.ingest_opencode(&done, 0);
    assert_eq!(labels(&b.load(None)), vec!["act[Command]:Ran sleep 1"]);
}

#[test]
fn opencode_skips_reasoning_steps_and_patches() {
    let mut b = builder();
    for json in [
        r#"{"type":"reasoning","text":"thinking"}"#,
        r#"{"type":"step-start"}"#,
        r#"{"type":"step-finish"}"#,
        r#"{"type":"patch","hash":"abc"}"#,
        r#"{"type":"file","filename":"img.png"}"#,
    ] {
        b.ingest_opencode(&oc("x", "assistant", json), 0);
    }
    assert_eq!(b.total(), 0, "none of these are conversation items");
}

#[test]
fn opencode_agent_prose_becomes_needs_input_preview() {
    let mut b = builder();
    b.ingest_opencode(
        &oc(
            "p1",
            "assistant",
            r#"{"type":"text","text":"Delete the file?","time":{"start":1,"end":2}}"#,
        ),
        0,
    );
    assert_eq!(b.on_needs_input(0).as_deref(), Some("Delete the file?"));
}

#[cfg(not(windows))]
#[test]
fn opencode_sync_reads_db_and_resets_on_session_switch() {
    use rusqlite::Connection;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("opencode.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, time_updated INTEGER);
         CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, data TEXT);
         CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT,
                            time_created INTEGER, data TEXT);
         INSERT INTO session VALUES ('ses_a','/repo/wt',100);
         INSERT INTO message VALUES ('ma','ses_a','{\"role\":\"user\"}');
         INSERT INTO part VALUES ('pa','ma','ses_a',10,'{\"type\":\"text\",\"text\":\"first\"}');",
    )
    .unwrap();

    let mut b = builder();
    b.sync_opencode(&db, "/repo/wt", 0);
    assert_eq!(labels(&b.load(None)), vec!["user:first"]);

    // A newer session in the same worktree replaces the transcript.
    conn.execute_batch(
        "INSERT INTO session VALUES ('ses_b','/repo/wt',200);
         INSERT INTO message VALUES ('mb','ses_b','{\"role\":\"user\"}');
         INSERT INTO part VALUES ('pb','mb','ses_b',10,'{\"type\":\"text\",\"text\":\"second\"}');",
    )
    .unwrap();
    b.sync_opencode(&db, "/repo/wt", 0);
    assert_eq!(
        labels(&b.load(None)),
        vec!["user:second"],
        "switching to the newer session id resets the transcript"
    );
}
