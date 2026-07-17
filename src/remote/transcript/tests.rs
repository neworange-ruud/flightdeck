use super::*;

fn builder() -> TranscriptBuilder {
    TranscriptBuilder::new(SessionId::new("sess-1"))
}

/// Collect the current full transcript items.
fn items(b: &TranscriptBuilder) -> Vec<TranscriptItem> {
    b.load(None).items
}

// --- ANSI stripping --------------------------------------------------------

#[test]
fn strips_ansi_color_codes() {
    let mut b = builder();
    // Red "Hello", reset, then " world", flushed by a blank line.
    b.push_bytes(b"\x1b[31mHello\x1b[0m world\n\n", 0);
    let it = items(&b);
    assert_eq!(it.len(), 1);
    match &it[0] {
        TranscriptItem::AgentMessage { text, .. } => assert_eq!(text, "Hello world"),
        other => panic!("expected prose, got {other:?}"),
    }
}

#[test]
fn strips_cursor_movement_and_osc() {
    let mut b = builder();
    // Cursor move (CSI), an OSC title set (BEL-terminated), then prose.
    b.push_bytes(
        b"\x1b[2J\x1b[H\x1b]0;window title\x07plain text here\n\n",
        0,
    );
    match &items(&b)[0] {
        TranscriptItem::AgentMessage { text, .. } => assert_eq!(text, "plain text here"),
        other => panic!("expected prose, got {other:?}"),
    }
}

// --- carriage-return redraw + dedupe ---------------------------------------

#[test]
fn carriage_return_keeps_only_final_paint() {
    let mut b = builder();
    // A spinner overwriting itself, ending on the final frame.
    b.push_bytes("Loading 10%\rLoading 50%\rLoading done\n\n".as_bytes(), 0);
    match &items(&b)[0] {
        TranscriptItem::AgentMessage { text, .. } => assert_eq!(text, "Loading done"),
        other => panic!("expected prose, got {other:?}"),
    }
}

#[test]
fn deduplicates_repeated_redraw_frames() {
    let mut b = builder();
    // A full-screen TUI repaints the same three lines several times.
    let frame = "The quick brown fox\njumps over the lazy dog\nand keeps on running\n";
    for _ in 0..5 {
        b.push_bytes(frame.as_bytes(), 0);
    }
    b.push_bytes(b"\n", 0); // flush
                            // Only one copy of the prose survives (three lines in one block).
    let it = items(&b);
    assert_eq!(it.len(), 1);
    match &it[0] {
        TranscriptItem::AgentMessage { text, .. } => {
            assert_eq!(text.matches("quick brown fox").count(), 1);
            assert_eq!(text.lines().count(), 3);
        }
        other => panic!("expected prose, got {other:?}"),
    }
}

// --- pill collapsing -------------------------------------------------------

#[test]
fn collapses_tool_call_into_activity_pill() {
    let mut b = builder();
    // Claude-Code-style: a ⏺ tool header and a ⎿ result continuation.
    b.push_bytes(
        "\u{23fa} Bash(npm test)\n\u{23bf} 42 passed\n\n".as_bytes(),
        0,
    );
    let it = items(&b);
    assert_eq!(it.len(), 1);
    match &it[0] {
        TranscriptItem::Activity {
            summary,
            body,
            kind,
            ..
        } => {
            assert_eq!(summary, "Bash(npm test)");
            assert_eq!(body.as_deref(), Some("42 passed"));
            assert_eq!(*kind, ActivityKind::Test);
        }
        other => panic!("expected activity, got {other:?}"),
    }
}

#[test]
fn ran_line_splits_summary_and_detail() {
    let mut b = builder();
    b.push_bytes(b"Ran npm test \xc2\xb7 42 passed\n\n", 0); // "·" is C2 B7
    match &items(&b)[0] {
        TranscriptItem::Activity {
            summary,
            detail,
            kind,
            ..
        } => {
            assert_eq!(summary, "Ran npm test");
            assert_eq!(detail.as_deref(), Some("42 passed"));
            assert_eq!(*kind, ActivityKind::Test);
        }
        other => panic!("expected activity, got {other:?}"),
    }
}

#[test]
fn edit_line_classified_as_edit() {
    let mut b = builder();
    b.push_bytes("Edited auth.ts +18 -4\n\n".as_bytes(), 0);
    match &items(&b)[0] {
        TranscriptItem::Activity { summary, kind, .. } => {
            assert_eq!(summary, "Edited auth.ts +18 -4");
            assert_eq!(*kind, ActivityKind::Edit);
        }
        other => panic!("expected activity, got {other:?}"),
    }
}

#[test]
fn shell_prompt_line_is_command_pill() {
    let mut b = builder();
    b.push_bytes(b"$ ls -la\n\n", 0);
    match &items(&b)[0] {
        TranscriptItem::Activity { summary, kind, .. } => {
            assert_eq!(summary, "$ ls -la");
            assert_eq!(*kind, ActivityKind::Command);
        }
        other => panic!("expected activity, got {other:?}"),
    }
}

#[test]
fn ambiguous_line_stays_prose() {
    let mut b = builder();
    // A dashed list item is NOT a strong tool marker: honest -> prose.
    b.push_bytes(b"- some bullet point that is really prose\n\n", 0);
    match &items(&b)[0] {
        TranscriptItem::AgentMessage { .. } => {}
        other => panic!("expected prose, got {other:?}"),
    }
}

// --- prose chunking --------------------------------------------------------

#[test]
fn prose_blocks_split_on_blank_lines() {
    let mut b = builder();
    b.push_bytes(
        b"First paragraph line one.\nFirst paragraph line two.\n\nSecond paragraph here.\n\n",
        0,
    );
    let it = items(&b);
    assert_eq!(it.len(), 2);
    match (&it[0], &it[1]) {
        (
            TranscriptItem::AgentMessage { text: a, .. },
            TranscriptItem::AgentMessage { text: c, .. },
        ) => {
            assert_eq!(a, "First paragraph line one.\nFirst paragraph line two.");
            assert_eq!(c, "Second paragraph here.");
        }
        _ => panic!("expected two prose blocks"),
    }
}

#[test]
fn prose_then_pill_flushes_prose_first() {
    let mut b = builder();
    b.push_bytes(
        "Let me check the tests.\n\u{23fa} Read(config.toml)\n\n".as_bytes(),
        0,
    );
    let it = items(&b);
    assert_eq!(it.len(), 2);
    assert!(matches!(it[0], TranscriptItem::AgentMessage { .. }));
    assert!(matches!(it[1], TranscriptItem::Activity { .. }));
}

// --- permission preview capture --------------------------------------------

#[test]
fn needs_input_captures_preview_and_emits_prompt() {
    let mut b = builder();
    b.push_bytes(
        b"I want to run a shell command to install packages.\nProceed with the installation?\n",
        1_000,
    );
    let preview = b.on_needs_input(2_000).expect("preview");
    assert!(preview.contains("Proceed with the installation?"));
    // The last item is the inline permission prompt with both options.
    let it = items(&b);
    match it.last().unwrap() {
        TranscriptItem::PermissionPrompt {
            prompt_id,
            command,
            options,
            ..
        } => {
            assert_eq!(prompt_id.as_str(), "sess-1:p1");
            assert!(command.contains("Proceed with the installation?"));
            assert_eq!(options.len(), 2);
            assert_eq!(options[0].choice, PermissionChoice::AllowOnce);
            assert_eq!(options[1].choice, PermissionChoice::Deny);
        }
        other => panic!("expected permission prompt, got {other:?}"),
    }
}

#[test]
fn needs_input_folds_trailing_partial_line() {
    let mut b = builder();
    // No trailing newline — the prompt is a partial line.
    b.push_bytes(b"Allow write to /etc/hosts?", 0);
    let preview = b.on_needs_input(0).expect("preview");
    assert!(preview.contains("Allow write to /etc/hosts?"));
}

// --- pagination + incremental appends --------------------------------------

#[test]
fn take_appended_is_incremental() {
    let mut b = builder();
    b.push_bytes(b"one\n\n", 0);
    let first = b.take_appended().expect("first batch");
    assert_eq!(first.from_index, 0);
    assert!(!first.replace);
    assert_eq!(first.items.len(), 1);
    // Nothing new yet.
    assert!(b.take_appended().is_none());
    // Add more.
    b.push_bytes(b"two\n\n", 0);
    let second = b.take_appended().expect("second batch");
    assert_eq!(second.from_index, 1);
    assert_eq!(second.items.len(), 1);
}

#[test]
fn load_honours_from_index() {
    let mut b = builder();
    for i in 0..5 {
        b.push_bytes(format!("line {i}\n\n").as_bytes(), 0);
    }
    assert_eq!(b.total(), 5);
    let feed = b.load(Some(2));
    assert_eq!(feed.from_index, 2);
    assert!(feed.replace);
    assert_eq!(feed.items.len(), 3);
}

#[test]
fn ring_buffer_caps_memory() {
    let mut b = builder();
    for i in 0..(MAX_ITEMS + 50) {
        b.push_bytes(format!("m{i}\n\n").as_bytes(), 0);
    }
    assert_eq!(b.total() as usize, MAX_ITEMS + 50);
    // Only the most recent MAX_ITEMS are retained.
    let feed = b.load(None);
    assert_eq!(feed.items.len(), MAX_ITEMS);
    assert_eq!(feed.from_index, 50);
}
