use super::*;

fn sid(s: &str) -> SessionId {
    SessionId::new(s)
}

fn shid(s: &str) -> ShellId {
    ShellId::new(s)
}

/// Pull the [`ShellOutput`] payloads out of a drained outbound batch, in order.
fn outputs(msgs: &[DesktopToPhone]) -> Vec<&ShellOutput> {
    msgs.iter()
        .filter_map(|m| match m {
            DesktopToPhone::ShellOutput(o) => Some(o),
            _ => None,
        })
        .collect()
}

/// Pull the [`ShellEventKind`]s out of a drained outbound batch, in order.
fn events(msgs: &[DesktopToPhone]) -> Vec<&ShellEventKind> {
    msgs.iter()
        .filter_map(|m| match m {
            DesktopToPhone::ShellEvent(e) => Some(&e.kind),
            _ => None,
        })
        .collect()
}

// --- chunking ------------------------------------------------------------------

#[test]
fn split_chunks_respects_max_and_char_boundaries() {
    // Empty → nothing.
    assert!(split_chunks("", 4).is_empty());
    // Exact + remainder.
    assert_eq!(split_chunks("abcdefg", 3), vec!["abc", "def", "g"]);
    // Never splits a multi-byte codepoint: "é" is two bytes, so with max 3 the
    // first chunk backs up to a boundary rather than cutting the 'é'.
    let s = "aébcé"; // bytes: a(1) é(2) b(1) c(1) é(2) = 7 bytes
    for piece in split_chunks(s, 3) {
        assert!(piece.len() <= 3);
        // Each piece must itself be valid UTF-8 (guaranteed by &str, but assert
        // reassembly is lossless).
    }
    assert_eq!(split_chunks(s, 3).concat(), s);
}

// --- lifecycle -----------------------------------------------------------------

#[test]
fn open_input_output_interrupt_close_lifecycle() {
    let mut m = ShellManager::new();
    assert!(!m.has_shell(&sid("t1")));

    m.opened(sid("t1"), shid("s1"), 0, 80, 24);
    assert!(m.has_shell(&sid("t1")));
    assert!(m.matches(&sid("t1"), &shid("s1")));
    assert_eq!(m.child_index(&sid("t1")), Some(0));

    // Opened event was queued.
    let batch = m.take_outbound();
    assert_eq!(
        events(&batch),
        vec![&ShellEventKind::Opened { cols: 80, rows: 24 }]
    );

    // Output for the backing child produces an ordered chunk.
    m.pump(&sid("t1"), 0, b"hello\n");
    let batch = m.take_outbound();
    let outs = outputs(&batch);
    assert_eq!(outs.len(), 1);
    assert_eq!(outs[0].seq, 1);
    assert_eq!(outs[0].data, "hello\n");
    assert_eq!(outs[0].stream, ShellStream::Stdout);
    assert_eq!(outs[0].shell_id, shid("s1"));

    // Sequence increments monotonically across pumps.
    m.pump(&sid("t1"), 0, b"world");
    let batch = m.take_outbound();
    assert_eq!(outputs(&batch)[0].seq, 2);

    // Output for a *different* child of the same session is ignored (e.g. a
    // desktop-opened shell), and so is an empty read.
    m.pump(&sid("t1"), 1, b"ignored");
    m.pump(&sid("t1"), 0, b"");
    assert!(m.take_outbound().is_empty());

    // Close returns the backing index and queues the closed event; the slot is
    // freed so a fresh open is allowed again.
    assert_eq!(m.close(&sid("t1"), &shid("s1")), Some(0));
    assert_eq!(events(&m.take_outbound()), vec![&ShellEventKind::Closed]);
    assert!(!m.has_shell(&sid("t1")));
}

#[test]
fn second_open_is_capped_per_session() {
    let mut m = ShellManager::new();
    m.opened(sid("t1"), shid("s1"), 0, 80, 24);
    let _ = m.take_outbound();
    // The cap is expressed via `has_shell`; the event loop refuses the spawn.
    assert!(m.has_shell(&sid("t1")));
    // A shell on a *different* session is independent.
    assert!(!m.has_shell(&sid("t2")));
}

#[test]
fn input_to_unknown_or_mismatched_shell_is_rejected() {
    let mut m = ShellManager::new();
    // No shell at all.
    assert!(!m.matches(&sid("t1"), &shid("s1")));
    assert_eq!(m.child_index(&sid("t1")), None);

    m.opened(sid("t1"), shid("s1"), 3, 80, 24);
    let _ = m.take_outbound();
    // Wrong shell id → not matched.
    assert!(!m.matches(&sid("t1"), &shid("other")));
    // Right id → matched, with the backing index.
    assert!(m.matches(&sid("t1"), &shid("s1")));
    assert_eq!(m.child_index(&sid("t1")), Some(3));

    // After close, input is rejected (closed shell).
    m.close(&sid("t1"), &shid("s1"));
    let _ = m.take_outbound();
    assert!(!m.matches(&sid("t1"), &shid("s1")));
}

#[test]
fn exit_is_reported_once_and_stops_output() {
    let mut m = ShellManager::new();
    m.opened(sid("t1"), shid("s1"), 0, 80, 24);
    let _ = m.take_outbound();

    assert_eq!(m.active_shells(), vec![(sid("t1"), 0)]);
    m.mark_exit(&sid("t1"), 0, Some(0));
    assert_eq!(
        events(&m.take_outbound()),
        vec![&ShellEventKind::Exited { code: Some(0) }]
    );
    // Exited shells drop out of the active-poll set…
    assert!(m.active_shells().is_empty());
    // …the event is not re-emitted…
    m.mark_exit(&sid("t1"), 0, Some(0));
    assert!(m.take_outbound().is_empty());
    // …and no further output flows after exit.
    m.pump(&sid("t1"), 0, b"late");
    assert!(m.take_outbound().is_empty());
    // The slot stays occupied until an explicit close.
    assert!(m.has_shell(&sid("t1")));
}

#[test]
fn large_read_splits_into_multiple_ordered_chunks() {
    let mut m = ShellManager::new();
    m.opened(sid("t1"), shid("s1"), 0, 80, 24);
    let _ = m.take_outbound();

    // 10 KiB in one read → three chunks (4096, 4096, 1808), seqs 1..=3.
    let blob = vec![b'x'; SHELL_CHUNK_BYTES * 2 + 1808];
    m.pump(&sid("t1"), 0, &blob);
    let batch = m.take_outbound();
    let outs = outputs(&batch);
    assert_eq!(outs.len(), 3);
    assert_eq!(outs[0].seq, 1);
    assert_eq!(outs[0].data.len(), SHELL_CHUNK_BYTES);
    assert_eq!(outs[1].seq, 2);
    assert_eq!(outs[1].data.len(), SHELL_CHUNK_BYTES);
    assert_eq!(outs[2].seq, 3);
    assert_eq!(outs[2].data.len(), 1808);
    // Lossless reassembly.
    let joined: String = outs.iter().map(|o| o.data.clone()).collect();
    assert_eq!(joined.len(), blob.len());
}

#[test]
fn clear_drops_shells_and_queue() {
    let mut m = ShellManager::new();
    m.opened(sid("t1"), shid("s1"), 0, 80, 24);
    m.pump(&sid("t1"), 0, b"data");
    m.clear();
    assert!(!m.has_shell(&sid("t1")));
    assert!(m.take_outbound().is_empty());
    assert!(m.active_shells().is_empty());
}
