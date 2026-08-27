//! Unit tests for the ordering/dedup watermark and the cursor-resume decision
//! (`specs/WEB_INTERFACE.md` §5.1, Q3), plus the R2 git mapping.
//!
//! These are the two pieces of logic in this module that have to be right
//! independently of a socket: what happens to an out-of-order or replayed
//! keystroke, and which of Q3's three answers a byte cursor earns. The socket
//! itself is exercised in `tests/web_server.rs`.

use super::*;
use crate::web::protocol::AckOutcome;

// ---------------------------------------------------------------------------
// A recording host
// ---------------------------------------------------------------------------

/// A [`TerminalHost`] that records what it was asked to write and answers per a
/// scripted per-terminal outcome.
///
/// Note what it cannot record: a resize. [`TerminalHost`] has one method. The
/// integration test proves the same thing against a real PTY seam that *does*
/// count resizes.
#[derive(Default)]
struct RecordingHost {
    writes: Vec<(TerminalId, Vec<u8>)>,
    answers: HashMap<TerminalId, Written>,
}

impl RecordingHost {
    fn with(terminal: &str, answer: Written) -> Self {
        let mut host = RecordingHost::default();
        host.answers.insert(TerminalId::new(terminal), answer);
        host
    }

    fn written_to(&self, terminal: &str) -> Vec<u8> {
        self.writes
            .iter()
            .filter(|(id, _)| id.as_str() == terminal)
            .flat_map(|(_, bytes)| bytes.clone())
            .collect()
    }
}

impl TerminalHost for RecordingHost {
    fn write_terminal_input(&mut self, terminal_id: &TerminalId, bytes: &[u8]) -> Written {
        let answer = self
            .answers
            .get(terminal_id)
            .cloned()
            .unwrap_or(Written::NoSuchTerminal);
        if answer == Written::Ok {
            self.writes.push((terminal_id.clone(), bytes.to_vec()));
        }
        answer
    }
}

fn input(seq: u64, terminal: &str, data: &[u8]) -> Input {
    Input {
        seq,
        terminal_id: TerminalId::new(terminal),
        data: data.to_vec(),
    }
}

fn viewer(id: &str) -> ViewerId {
    ViewerId::new(id)
}

// ===========================================================================
// Terminal identity
// ===========================================================================

/// The ids are the spelling `TerminalId`'s own doc promises, and a child's id is
/// keyed by its mint rather than its index — which is the whole reason the mint
/// exists.
#[test]
fn terminal_ids_are_stable_and_never_positional() {
    assert_eq!(primary_terminal_id("tab-1").as_str(), "tab-1:primary");
    assert_eq!(child_terminal_id("tab-1", 3).as_str(), "tab-1:child:3");
    assert_ne!(child_terminal_id("tab-1", 1), child_terminal_id("tab-1", 2));
    assert_ne!(primary_terminal_id("tab-1"), primary_terminal_id("tab-2"));
}

// ===========================================================================
// Bytes out
// ===========================================================================

/// D2/Q3: every frame carries the offset of its own first byte, and those
/// offsets are contiguous across chunks.
#[test]
fn live_frames_carry_contiguous_monotonic_offsets() {
    let mut streams = TerminalStreams::new(1024);
    let id = primary_terminal_id("tab");

    let first = streams.pty_output(&id, b"hello ").expect("a frame");
    let second = streams.pty_output(&id, b"world").expect("a frame");

    assert_eq!(first.offset, 0);
    assert_eq!(first.next_offset(), 6);
    assert_eq!(second.offset, 6, "the second chunk continues the first");
    assert_eq!(second.next_offset(), 11);
    assert!(
        !first.truncated && !second.truncated,
        "live is never truncated"
    );
    assert_eq!(streams.byte_len(&id), 11);
}

/// An empty read is not a stream event: it must not produce a frame, and must
/// not move the offset. `try_read_output` returns empty vectors constantly.
#[test]
fn an_empty_chunk_produces_no_frame() {
    let mut streams = TerminalStreams::new(1024);
    let id = primary_terminal_id("tab");
    assert!(streams.pty_output(&id, b"").is_none());
    assert_eq!(streams.byte_len(&id), 0);
    assert!(
        !streams.knows(&id),
        "an empty read must not even conjure a ring buffer"
    );
}

/// The tee is the authority on which terminals exist: bytes from a terminal
/// nobody registered still stream rather than being dropped on the floor.
#[test]
fn output_from_an_unregistered_terminal_still_streams() {
    let mut streams = TerminalStreams::new(1024);
    let id = child_terminal_id("tab", 7);
    let frame = streams.pty_output(&id, b"surprise").expect("a frame");
    assert_eq!(frame.terminal_id, id);
    assert!(streams.knows(&id));
}

/// A closed terminal keeps its ring: a viewer that reconnects after the process
/// died still wants to read what it said before it died.
#[test]
fn a_closed_terminal_keeps_its_replay_but_reports_the_exit() {
    let mut streams = TerminalStreams::new(1024);
    let id = primary_terminal_id("tab");
    streams.pty_output(&id, b"goodbye");
    streams.closed(&id, Some(1));

    assert!(!streams.alive(&id));
    assert_eq!(streams.exit_code(&id), Some(1));
    assert_eq!(streams.byte_len(&id), 7);
    let frame = streams.resume_frame(&id, 0).expect("history survives");
    assert_eq!(frame.data, b"goodbye");
}

/// A tab the desktop closed takes its ring with it, or the registry would grow
/// by `replay_bytes` for every session ever opened.
#[test]
fn retain_drops_the_rings_of_terminals_the_host_no_longer_has() {
    let mut streams = TerminalStreams::new(1024);
    let kept = primary_terminal_id("kept");
    let gone = primary_terminal_id("gone");
    streams.pty_output(&kept, b"a");
    streams.pty_output(&gone, b"b");

    let live: HashSet<TerminalId> = [kept.clone()].into_iter().collect();
    streams.retain(&live);

    assert!(streams.knows(&kept));
    assert!(!streams.knows(&gone));
    assert_eq!(streams.len(), 1);
}

// ===========================================================================
// The cursor-resume decision (Q3)
// ===========================================================================

/// A viewer that is already current gets no frame at all — not a zero-length
/// one, which would make the browser repaint for nothing.
#[test]
fn a_current_cursor_earns_no_frame() {
    let mut streams = TerminalStreams::new(1024);
    let id = primary_terminal_id("tab");
    streams.pty_output(&id, b"abcdef");
    assert!(streams.resume_frame(&id, 6).is_none());
}

/// The Tail case: an exact continuation, starting where the viewer said it was,
/// with nothing re-sent and nothing skipped.
#[test]
fn a_cursor_inside_the_ring_resumes_exactly_where_it_left_off() {
    let mut streams = TerminalStreams::new(1024);
    let id = primary_terminal_id("tab");
    streams.pty_output(&id, b"abcdef");

    let frame = streams.resume_frame(&id, 2).expect("a tail");
    assert_eq!(frame.offset, 2);
    assert_eq!(frame.data, b"cdef");
    assert!(!frame.truncated, "nothing was lost, so nothing is claimed");
    assert_eq!(frame.next_offset(), 6);
}

/// The Truncated case: the cursor aged out of the ring, so the frame starts
/// ahead of what was asked for and says so. That inequality *is* the gap.
#[test]
fn a_cursor_that_aged_out_is_answered_truncated_from_the_oldest_byte() {
    let mut streams = TerminalStreams::new(4);
    let id = primary_terminal_id("tab");
    streams.pty_output(&id, b"abcdefgh"); // ring now holds "efgh", offsets 4..8

    let frame = streams.resume_frame(&id, 1).expect("a truncated replay");
    assert!(frame.truncated, "the viewer must be told it missed output");
    assert_eq!(
        frame.offset, 4,
        "a truncated replay starts at the oldest retained byte, not at the cursor"
    );
    assert!(
        frame.offset > 1,
        "the gap is exactly offset > requested, and truncated names it"
    );
    assert_eq!(frame.data, b"efgh");
}

/// A cursor from the future (a client bug, or a host restart that zeroed the
/// counters) is not skipped ahead to: it gets the same honest truncated answer.
#[test]
fn a_cursor_from_the_future_is_answered_truncated_not_trusted() {
    let mut streams = TerminalStreams::new(1024);
    let id = primary_terminal_id("tab");
    streams.pty_output(&id, b"abc");

    let frame = streams
        .resume_frame(&id, 9_000)
        .expect("a truncated replay");
    assert!(frame.truncated);
    assert_eq!(frame.data, b"abc");
    assert_eq!(frame.offset, 0);
}

/// A first attach carries no cursor, and is deliberately the *same* code path
/// as a resume from zero — the `TermCursor` type's own documented meaning of
/// zero. So a fresh viewer onto a wrapped ring is flagged truncated too, which
/// is honest: output was discarded before it could receive it.
#[test]
fn a_first_attach_is_a_resume_from_zero_and_is_honest_about_a_wrapped_ring() {
    let mut fresh = TerminalStreams::new(1024);
    let id = primary_terminal_id("tab");
    fresh.pty_output(&id, b"abc");
    let frames = fresh.attach_frames(&[]);
    assert_eq!(frames.len(), 1);
    assert!(!frames[0].truncated, "nothing was discarded");
    assert_eq!(frames[0].data, b"abc");

    let mut wrapped = TerminalStreams::new(4);
    wrapped.pty_output(&id, b"abcdefgh");
    let frames = wrapped.attach_frames(&[]);
    assert_eq!(frames.len(), 1);
    assert!(
        frames[0].truncated,
        "a first attach onto a wrapped ring did miss output and must say so"
    );
    assert_eq!(frames[0].offset, 4);
}

/// A terminal that has produced nothing yet earns no frame, so a snapshot is
/// not followed by a flurry of empty repaints.
#[test]
fn a_silent_terminal_earns_no_attach_frame() {
    let mut streams = TerminalStreams::new(1024);
    streams.open(primary_terminal_id("tab"));
    assert!(streams.attach_frames(&[]).is_empty());
}

/// A cursor for a terminal this host does not have is not answered with an
/// empty byte frame: the snapshot the viewer is about to receive is the
/// authoritative place to learn the terminal is gone.
#[test]
fn a_cursor_for_an_unknown_terminal_earns_no_frame() {
    let streams = TerminalStreams::new(1024);
    let cursors = vec![TermCursor {
        terminal_id: primary_terminal_id("vanished"),
        next_offset: 12,
    }];
    assert!(streams.attach_frames(&cursors).is_empty());
    assert!(streams
        .resume_frame(&primary_terminal_id("vanished"), 12)
        .is_none());
}

/// Two terminals, two cursors, each answered from its own ring — and in a
/// stable order.
#[test]
fn each_terminal_is_resumed_from_its_own_cursor() {
    let mut streams = TerminalStreams::new(1024);
    let primary = primary_terminal_id("tab");
    let child = child_terminal_id("tab", 1);
    streams.pty_output(&primary, b"PRIMARY");
    streams.pty_output(&child, b"CHILD");

    let frames = streams.attach_frames(&[
        TermCursor {
            terminal_id: primary.clone(),
            next_offset: 4,
        },
        TermCursor {
            terminal_id: child.clone(),
            next_offset: 0,
        },
    ]);

    assert_eq!(frames.len(), 2);
    let by_id = |id: &TerminalId| {
        frames
            .iter()
            .find(|f| &f.terminal_id == id)
            .expect("a frame for that terminal")
    };
    assert_eq!(by_id(&primary).data, b"ARY");
    assert_eq!(by_id(&primary).offset, 4);
    assert_eq!(by_id(&child).data, b"CHILD");
    assert_eq!(by_id(&child).offset, 0);
}

/// A zero-capacity ring retains nothing, so there is no payload to attach a
/// truncation claim to. It reports no frame rather than an empty one.
#[test]
fn a_zero_capacity_ring_answers_with_no_frame_at_all() {
    let mut streams = TerminalStreams::new(0);
    let id = primary_terminal_id("tab");
    streams.pty_output(&id, b"discarded");
    assert_eq!(
        streams.byte_len(&id),
        9,
        "the offset counter still advances"
    );
    assert!(streams.resume_frame(&id, 0).is_none());
}

// ===========================================================================
// The input watermark (§5.1)
// ===========================================================================

/// The happy path: in-order input is written, once each, and the watermark
/// tracks the highest seq that landed.
#[test]
fn in_order_input_is_written_once_and_advances_the_watermark() {
    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::with("tab:primary", Written::Ok);
    let v = viewer("v1");

    for (seq, byte) in [(1u64, b'a'), (2, b'b'), (3, b'c')] {
        let verdict = streams.apply_input(&v, &input(seq, "tab:primary", &[byte]), &mut host);
        assert_eq!(verdict, InputVerdict::Applied, "seq {seq}");
        assert_eq!(verdict.ack(seq).outcome, AckOutcome::Applied);
    }
    assert_eq!(host.written_to("tab:primary"), b"abc");
    assert_eq!(streams.watermark(&v), 3);
}

/// The dedup rule: a browser replaying its held queue after a reconnect
/// re-sends what it was never told about. Anything at or below the watermark is
/// **not** written — that would type it twice — and is acked `Ignored` with the
/// reason, never silently dropped (§5.1).
#[test]
fn a_replayed_keystroke_is_ignored_rather_than_typed_twice() {
    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::with("tab:primary", Written::Ok);
    let v = viewer("v1");

    streams.apply_input(&v, &input(1, "tab:primary", b"a"), &mut host);
    streams.apply_input(&v, &input(2, "tab:primary", b"b"), &mut host);

    let verdict = streams.apply_input(&v, &input(2, "tab:primary", b"b"), &mut host);
    assert_eq!(verdict, InputVerdict::AlreadyApplied { watermark: 2 });
    let ack = verdict.ack(2);
    assert_eq!(ack.outcome, AckOutcome::Ignored);
    assert!(
        ack.detail.as_deref().unwrap_or_default().contains("seq 2"),
        "the ack names the watermark that refused it: {:?}",
        ack.detail
    );
    assert_eq!(
        host.written_to("tab:primary"),
        b"ab",
        "the duplicate must not reach the PTY"
    );
    assert_eq!(streams.watermark(&v), 2);
}

/// A frame that arrives *after* a higher seq already landed cannot be written:
/// that would be out of order, which §5.1 forbids as squarely as it forbids
/// dropping. Ignored, with the reason.
#[test]
fn a_late_low_seq_is_ignored_rather_than_typed_out_of_order() {
    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::with("tab:primary", Written::Ok);
    let v = viewer("v1");

    streams.apply_input(&v, &input(7, "tab:primary", b"g"), &mut host);
    let verdict = streams.apply_input(&v, &input(4, "tab:primary", b"d"), &mut host);

    assert_eq!(verdict, InputVerdict::AlreadyApplied { watermark: 7 });
    assert_eq!(host.written_to("tab:primary"), b"g");
}

/// A gap is applied. One socket cannot reorder itself, so a gap means the
/// browser has nothing to fill it with, and stalling a live terminal waiting for
/// keystrokes that do not exist would be the worse failure.
#[test]
fn a_gap_in_the_sequence_does_not_stall_the_terminal() {
    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::with("tab:primary", Written::Ok);
    let v = viewer("v1");

    assert_eq!(
        streams.apply_input(&v, &input(1, "tab:primary", b"a"), &mut host),
        InputVerdict::Applied
    );
    assert_eq!(
        streams.apply_input(&v, &input(9, "tab:primary", b"i"), &mut host),
        InputVerdict::Applied
    );
    assert_eq!(host.written_to("tab:primary"), b"ai");
    assert_eq!(streams.watermark(&v), 9);
}

/// Watermarks are per viewer: an observer promoted to controller, or a second
/// browser, starts from its own zero rather than inheriting someone else's.
#[test]
fn watermarks_do_not_leak_between_viewers() {
    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::with("tab:primary", Written::Ok);

    streams.apply_input(&viewer("v1"), &input(5, "tab:primary", b"a"), &mut host);
    let verdict = streams.apply_input(&viewer("v2"), &input(1, "tab:primary", b"b"), &mut host);

    assert_eq!(
        verdict,
        InputVerdict::Applied,
        "v2's seq 1 is its own first frame, not a replay of v1's"
    );
    assert_eq!(streams.watermark(&viewer("v1")), 5);
    assert_eq!(streams.watermark(&viewer("v2")), 1);
}

/// A reconnect gets a new `ViewerId`, so the watermark has to be carried onto
/// it or the whole queue would be re-typed.
#[test]
fn a_reconnect_adopts_the_previous_connections_watermark() {
    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::with("tab:primary", Written::Ok);
    let old = viewer("v1");
    let new = viewer("v2");

    streams.apply_input(&old, &input(3, "tab:primary", b"abc"), &mut host);
    streams.adopt_watermark(&old, &new);

    assert_eq!(streams.watermark(&new), 3);
    assert_eq!(
        streams.apply_input(&new, &input(3, "tab:primary", b"abc"), &mut host),
        InputVerdict::AlreadyApplied { watermark: 3 },
        "the replayed tail of the queue must not be typed twice"
    );
    assert_eq!(
        streams.apply_input(&new, &input(4, "tab:primary", b"d"), &mut host),
        InputVerdict::Applied,
        "and the genuinely new keystroke still lands"
    );
    assert_eq!(host.written_to("tab:primary"), b"abcd");
}

// ---------------------------------------------------------------------------
// Refusal paths (SPECS §26: failure paths need tests, not only success paths)
// ---------------------------------------------------------------------------

/// A keystroke typed against a terminal the desktop has since closed is
/// **rejected with a sentence**, never silently discarded — and the watermark
/// does not move, so the browser may legitimately retry the same seq.
#[test]
fn input_for_a_terminal_that_is_gone_is_rejected_and_the_watermark_holds() {
    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::default();
    let v = viewer("v1");

    let verdict = streams.apply_input(&v, &input(1, "tab:stale", b"x"), &mut host);
    assert_eq!(verdict, InputVerdict::UnknownTerminal);

    let ack = verdict.ack(1);
    assert_eq!(ack.outcome, AckOutcome::Rejected);
    assert!(
        ack.detail.is_some(),
        "a rejection without a reason is a silent drop with extra steps"
    );
    assert_eq!(
        streams.watermark(&v),
        0,
        "nothing was applied, so nothing may be claimed as applied"
    );
}

/// A terminal that exists but has exited is a *different* refusal from one that
/// is gone: the browser can still see it, and deserves the accurate reason.
#[test]
fn input_for_an_exited_terminal_says_the_process_has_gone() {
    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::with("tab:primary", Written::NotRunning);
    let verdict = streams.apply_input(&viewer("v1"), &input(1, "tab:primary", b"x"), &mut host);

    assert_eq!(verdict, InputVerdict::TerminalClosed);
    let detail = verdict.ack(1).detail.unwrap_or_default();
    assert!(detail.contains("exited"), "unexpected detail: {detail}");
}

/// A PTY that refuses the write is reported with the OS's own reason rather
/// than a generic failure.
#[test]
fn a_pty_that_refuses_the_write_is_reported_verbatim() {
    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::with(
        "tab:primary",
        Written::Failed("broken pipe (os error 32)".to_string()),
    );
    let v = viewer("v1");
    let verdict = streams.apply_input(&v, &input(1, "tab:primary", b"x"), &mut host);

    assert_eq!(
        verdict,
        InputVerdict::WriteFailed("broken pipe (os error 32)".to_string())
    );
    let detail = verdict.ack(1).detail.unwrap_or_default();
    assert!(
        detail.contains("broken pipe"),
        "unexpected detail: {detail}"
    );
    assert_eq!(
        streams.watermark(&v),
        0,
        "a failed write is retryable at the same seq"
    );
}

/// Every verdict has an ack. §5.1 forbids a keystroke disappearing without a
/// trace, so there must be no verdict for which the host says nothing.
#[test]
fn every_verdict_produces_an_ack_for_its_own_seq() {
    let verdicts = [
        InputVerdict::Applied,
        InputVerdict::AlreadyApplied { watermark: 4 },
        InputVerdict::UnknownTerminal,
        InputVerdict::TerminalClosed,
        InputVerdict::WriteFailed("nope".to_string()),
    ];
    for verdict in verdicts {
        let ack = verdict.ack(11);
        assert_eq!(ack.seq, 11, "{verdict:?} acked the wrong frame");
        if !verdict.applied() {
            assert!(
                ack.detail.is_some(),
                "{verdict:?} must say why it did not apply"
            );
        }
    }
}

/// The watermark map is bounded, and eviction takes the oldest — never the
/// viewer that just typed.
#[test]
fn the_watermark_map_is_bounded_and_evicts_the_oldest() {
    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::with("tab:primary", Written::Ok);
    for i in 0..(REMEMBERED_WATERMARKS + 5) {
        streams.apply_input(
            &viewer(&format!("v{i}")),
            &input(1, "tab:primary", b"x"),
            &mut host,
        );
    }
    assert_eq!(streams.watermarks.len(), REMEMBERED_WATERMARKS);
    assert_eq!(
        streams.watermark(&viewer("v0")),
        0,
        "the oldest was evicted"
    );
    assert_eq!(
        streams.watermark(&viewer(&format!("v{}", REMEMBERED_WATERMARKS + 4))),
        1,
        "the newest is retained"
    );
}

// ===========================================================================
// The inbound drain, and the D4 guarantee
// ===========================================================================

/// A `Resize` produces **no outbound frame and no host call**. The integration
/// test proves the PTY-level half against a resize-counting fake; this asserts
/// the shape of the decision here.
#[test]
fn a_resize_records_a_viewport_and_touches_nothing_else() {
    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::with("tab:primary", Written::Ok);
    let v = viewer("v1");
    let viewport = Viewport {
        cols: 200,
        rows: 60,
    };

    let out = streams.apply_inbound(
        &WebInbound::Resize {
            viewer_id: v.clone(),
            viewport,
        },
        &mut host,
    );

    assert!(out.is_empty(), "a resize is not answered on the wire");
    assert!(
        host.writes.is_empty(),
        "a resize must not touch a terminal at all"
    );
    assert_eq!(streams.viewport(&v), Some(viewport));
}

/// A detached viewer's viewport is forgotten; its watermark is not, because a
/// reconnect still needs it.
#[test]
fn detaching_forgets_the_viewport_but_keeps_the_watermark() {
    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::with("tab:primary", Written::Ok);
    let v = viewer("v1");
    streams.apply_input(&v, &input(2, "tab:primary", b"hi"), &mut host);
    streams.apply_inbound(
        &WebInbound::Resize {
            viewer_id: v.clone(),
            viewport: Viewport { cols: 80, rows: 24 },
        },
        &mut host,
    );

    streams.apply_inbound(
        &WebInbound::ViewerDetached {
            viewer_id: v.clone(),
        },
        &mut host,
    );

    assert_eq!(streams.viewport(&v), None);
    assert_eq!(streams.watermark(&v), 2);
}

/// An attach is answered with one targeted `TermBytes` per terminal that has
/// something to say — addressed to that viewer, not broadcast at everyone.
#[test]
fn an_attach_is_answered_with_targeted_resume_frames() {
    use crate::web::protocol::{Seat, ServerMsg};

    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::default();
    let id = primary_terminal_id("tab");
    streams.pty_output(&id, b"history");

    let out = streams.apply_inbound(
        &WebInbound::ViewerAttached {
            viewer_id: viewer("v1"),
            address: std::net::IpAddr::from([127, 0, 0, 1]),
            label: "test".to_string(),
            seat: Seat::Controlling,
            cursors: Vec::new(),
            resume_viewer: None,
        },
        &mut host,
    );

    assert_eq!(out.len(), 1);
    match &out[0] {
        WebOutbound::Viewer { viewer_id, msg } => {
            assert_eq!(viewer_id, &viewer("v1"));
            match msg {
                ServerMsg::TermBytes(frame) => {
                    assert_eq!(frame.data, b"history");
                    assert_eq!(frame.terminal_id, id);
                }
                other => panic!("expected TermBytes, got {other:?}"),
            }
        }
        other => panic!("a resume must be targeted, not broadcast: {other:?}"),
    }
}

/// Input arriving through the drain is acked to the viewer that sent it.
#[test]
fn input_through_the_drain_is_acked_to_its_sender() {
    use crate::web::protocol::ServerMsg;

    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::with("tab:primary", Written::Ok);
    let out = streams.apply_inbound(
        &WebInbound::Input {
            viewer_id: viewer("v1"),
            input: input(1, "tab:primary", b"x"),
        },
        &mut host,
    );

    assert_eq!(out.len(), 1);
    match &out[0] {
        WebOutbound::Viewer { viewer_id, msg } => {
            assert_eq!(viewer_id, &viewer("v1"));
            assert!(matches!(
                msg,
                ServerMsg::Ack(Ack {
                    outcome: AckOutcome::Applied,
                    ..
                })
            ));
        }
        other => panic!("an ack must be targeted: {other:?}"),
    }
    assert_eq!(host.written_to("tab:primary"), b"x");
}

// ===========================================================================
// R2 — the git mapping
// ===========================================================================

mod r2 {
    use super::*;
    use crate::git::status::{WorktreeChanges, WorktreeStatus};
    use std::path::PathBuf;

    fn status(upstream: Option<&str>) -> WorktreeStatus {
        WorktreeStatus {
            branch: "flightdeck/fix-login".to_string(),
            base_branch: "main".to_string(),
            dirty: true,
            changes: WorktreeChanges {
                added: 3,
                modified: 2,
                deleted: 1,
            },
            ahead: 0,
            behind: 0,
            upstream: upstream.map(|s| s.to_string()),
            base_drift: 4,
            worktree_path: PathBuf::from("/tmp/wt"),
        }
    }

    /// The `unknown` arm. `collected: false` is sent deliberately, and is the
    /// only thing that renders `git: ?` — the browser never has to infer it
    /// from zeroed counts.
    #[test]
    fn an_uncollected_worktree_is_reported_uncollected_not_clean() {
        let git = git_bar(GitFacts {
            status: None,
            fallback_branch: "flightdeck/fix-login",
        });
        assert!(!git.collected, "the browser renders `git: ?` from this");
        assert!(is_git_unknown(&git));
        assert!(
            git.is_clean(),
            "the counts are zero, which is exactly why `collected` has to exist"
        );
        assert_eq!(git.branch.as_deref(), Some("flightdeck/fix-login"));
    }

    /// The `no_upstream` arm — a load-bearing fact (2g, §5.1), sent as data.
    #[test]
    fn a_collected_worktree_with_no_upstream_says_so() {
        let git = git_bar(GitFacts {
            status: Some(&status(None)),
            fallback_branch: "",
        });
        assert!(git.collected);
        assert!(!git.has_upstream, "the browser renders `no-upstream`");
        assert!(!is_git_unknown(&git));
    }

    /// The `known` arm, with the file count the web git bar needs.
    #[test]
    fn a_collected_worktree_with_an_upstream_carries_the_counts() {
        let git = git_bar(GitFacts {
            status: Some(&status(Some("origin/flightdeck/fix-login"))),
            fallback_branch: "",
        });
        assert!(git.collected && git.has_upstream);
        assert_eq!((git.added, git.modified, git.removed), (3, 2, 1));
        assert_eq!(git.files_changed, 6, "the `(6 files)` in the git bar");
        assert_eq!(git.drift, 4);
    }

    /// The impossible fourth state. R2's objection to the two-bool encoding is
    /// that it *admits* `collected: false, has_upstream: true`; this asserts the
    /// host never emits it, which is what makes the pair a faithful three-way
    /// union. This is the test a later widening of the wire would rewrite.
    #[test]
    fn git_bar_never_claims_an_upstream_it_has_not_looked_for() {
        for fallback in ["", "some/branch"] {
            let git = git_bar(GitFacts {
                status: None,
                fallback_branch: fallback,
            });
            // The impossible fourth state, spelled as the implication it is:
            // claiming an upstream requires having looked.
            assert!(
                !git.has_upstream || git.collected,
                "the impossible fourth state escaped the adapter"
            );
            assert!(!git.has_upstream);
        }
    }

    /// The lifecycle half of R2: it is a fact on the wire, derived from the
    /// same function that decides whether to attach an integration at launch,
    /// so the flag cannot drift from the behaviour it describes.
    #[test]
    fn lifecycle_reporting_is_a_fact_not_an_inference() {
        use crate::contracts::domain::AgentDef;

        let def = |command: &str| AgentDef {
            key: "k".to_string(),
            display_name: "Some Agent".to_string(),
            command: command.to_string(),
            args: Vec::new(),
            status_patterns: Default::default(),
        };

        assert!(lifecycle_reporting(Some(&def("claude"))));
        assert!(
            !lifecycle_reporting(Some(&def("my-custom-wrapper"))),
            "an agent FlightDeck cannot wire hooks into reports no lifecycle"
        );
        assert!(
            !lifecycle_reporting(None),
            "a tab whose agent left the config reports no lifecycle either"
        );
    }
}

/// A reconnect that names the connection it is resuming inherits that
/// connection's watermark through the drain, with no help from the caller —
/// which is what makes §5.1's "never doubled" true in production and not only
/// in a test that remembered to call `adopt_watermark` by hand.
#[test]
fn the_drain_carries_the_watermark_across_a_reconnect() {
    use crate::web::protocol::Seat;

    let mut streams = TerminalStreams::new(1024);
    let mut host = RecordingHost::with("tab:primary", Written::Ok);
    let old = viewer("v1");
    let new = viewer("v2");

    streams.apply_input(&old, &input(2, "tab:primary", b"ab"), &mut host);
    streams.apply_inbound(
        &WebInbound::ViewerAttached {
            viewer_id: new.clone(),
            address: std::net::IpAddr::from([127, 0, 0, 1]),
            label: "test".to_string(),
            seat: Seat::Controlling,
            cursors: Vec::new(),
            resume_viewer: Some(old.clone()),
        },
        &mut host,
    );

    assert_eq!(streams.watermark(&new), 2);
    assert_eq!(
        streams.apply_input(&new, &input(2, "tab:primary", b"ab"), &mut host),
        InputVerdict::AlreadyApplied { watermark: 2 }
    );
    assert_eq!(host.written_to("tab:primary"), b"ab");
}

// ===========================================================================
// The publish-time delta diff
// ===========================================================================

mod delta {
    use super::*;
    use crate::agents::status::DisplayStatus;
    use crate::contracts::{InterpretedStatus, ProcessState};
    use crate::web::protocol::{
        Delta, Geometry, ProjectId, SessionPhase, TerminalRole, TerminalView,
    };
    use crate::web::server::HostState;

    fn terminal(id: &str) -> TerminalFacts {
        TerminalFacts {
            terminal_id: TerminalId::new(id),
            role: TerminalRole::Primary,
            title: "agent".to_string(),
            geometry: Geometry {
                cols: 120,
                rows: 34,
            },
            alive: true,
            exit_code: None,
        }
    }

    fn state(interpreted: InterpretedStatus, terminals: Vec<TerminalFacts>) -> HostState {
        let project_id = ProjectId::new("proj");
        let streams = TerminalStreams::new(1024);
        let session = session_view(
            &SessionFacts {
                project_id: &project_id,
                tab_id: "tab-1",
                name: "fix-login",
                agent: "claude",
                agent_def: None,
                phase: SessionPhase::Ready,
                display: DisplayStatus {
                    process: ProcessState::Running,
                    interpreted,
                    manual: None,
                },
                running_time_secs: 0,
                git: GitFacts {
                    status: None,
                    fallback_branch: "flightdeck/fix-login",
                },
                recovered: false,
                attached_existing_branch: false,
                terminals,
            },
            &streams,
        );
        HostState {
            projects: vec![project_view(
                &project_id,
                "flightdeck",
                "/repo",
                "main",
                vec![session],
            )],
            ..HostState::default()
        }
    }

    /// The first publish tells the browser about everything, because nobody has
    /// been told about any of it.
    #[test]
    fn a_first_publish_upserts_every_project() {
        let next = state(InterpretedStatus::Working, vec![terminal("tab-1:primary")]);
        let out = deltas(&HostState::default(), &next);
        assert!(
            matches!(out.as_slice(), [Delta::ProjectUpsert(_)]),
            "{out:?}"
        );
    }

    /// An unchanged tick is silent. This is the one that matters most: the TUI
    /// calls this every frame.
    #[test]
    fn an_unchanged_state_produces_no_deltas() {
        let before = state(InterpretedStatus::Working, vec![terminal("tab-1:primary")]);
        let after = state(InterpretedStatus::Working, vec![terminal("tab-1:primary")]);
        assert!(deltas(&before, &after).is_empty());
    }

    /// The cheap, frequent one: a status transition is a `Status` delta, and the
    /// project dot that follows from it comes with it.
    #[test]
    fn a_status_transition_is_a_status_delta_plus_the_dot() {
        let before = state(InterpretedStatus::Working, vec![terminal("tab-1:primary")]);
        let after = state(
            InterpretedStatus::WaitingForInput,
            vec![terminal("tab-1:primary")],
        );
        let out = deltas(&before, &after);
        assert!(
            out.iter().any(|d| matches!(d, Delta::Status { .. })),
            "{out:?}"
        );
        assert!(
            out.iter().any(|d| matches!(d, Delta::ProjectDot { .. })),
            "the rolled-up dot changed too: {out:?}"
        );
        assert!(
            !out.iter().any(|d| matches!(d, Delta::SessionUpsert(_))),
            "a status change must not resend the whole row: {out:?}"
        );
    }

    /// Byte counters move on every tick a terminal prints. They must not produce
    /// a state-change frame riding alongside the byte frame that already says it.
    #[test]
    fn a_growing_byte_length_produces_no_delta() {
        let mut streams = TerminalStreams::new(1024);
        let id = TerminalId::new("tab-1:primary");
        let facts = vec![terminal("tab-1:primary")];

        let before = state(InterpretedStatus::Working, facts.clone());
        streams.pty_output(&id, b"lots of output");
        // Rebuild with the same facts but a registry that has since advanced.
        let mut after = state(InterpretedStatus::Working, facts);
        after.projects[0].sessions[0].terminals[0].byte_len = 14;
        after.projects[0].sessions[0].terminals[0].replay_from = 0;

        assert!(
            deltas(&before, &after).is_empty(),
            "byte_len is carried by TermBytes, not by a TerminalUpsert"
        );
    }

    /// A terminal that exits is `TerminalClosed`, with its code — not an upsert
    /// that happens to have `alive: false`.
    #[test]
    fn an_exited_terminal_is_reported_closed_with_its_code() {
        let before = state(InterpretedStatus::Working, vec![terminal("tab-1:primary")]);
        let mut after = before.clone();
        let view: &mut TerminalView = &mut after.projects[0].sessions[0].terminals[0];
        view.alive = false;
        view.exit_code = Some(2);

        let out = deltas(&before, &after);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(
            matches!(
                &out[0],
                Delta::TerminalClosed {
                    exit_code: Some(2),
                    ..
                }
            ),
            "{out:?}"
        );
    }

    /// A host-side resize is a per-terminal `Geometry` delta naming the terminal
    /// — the only geometry frame, so the browser is never told twice.
    #[test]
    fn a_host_resize_is_one_geometry_delta_naming_its_terminal() {
        let before = state(InterpretedStatus::Working, vec![terminal("tab-1:primary")]);
        let mut after = before.clone();
        after.projects[0].sessions[0].terminals[0].geometry = Geometry {
            cols: 100,
            rows: 40,
        };
        after.geometry = Geometry {
            cols: 100,
            rows: 40,
        };

        let out = deltas(&before, &after);
        assert_eq!(out.len(), 1, "exactly one geometry frame: {out:?}");
        match &out[0] {
            Delta::Geometry {
                terminal_id,
                geometry,
            } => {
                assert_eq!(terminal_id.as_str(), "tab-1:primary");
                assert_eq!(geometry.cols, 100);
            }
            other => panic!("{other:?}"),
        }
    }

    /// A new terminal is an upsert; a vanished one is closed.
    #[test]
    fn terminals_appearing_and_vanishing_are_both_reported() {
        let before = state(InterpretedStatus::Working, vec![terminal("tab-1:primary")]);
        let with_child = state(
            InterpretedStatus::Working,
            vec![terminal("tab-1:primary"), terminal("tab-1:child:1")],
        );

        let out = deltas(&before, &with_child);
        assert!(
            matches!(out.as_slice(), [Delta::TerminalUpsert(t)] if t.terminal_id.as_str() == "tab-1:child:1"),
            "{out:?}"
        );

        let out = deltas(&with_child, &before);
        assert!(
            matches!(out.as_slice(), [Delta::TerminalClosed { terminal_id, .. }] if terminal_id.as_str() == "tab-1:child:1"),
            "{out:?}"
        );
    }

    /// A closed project is `ProjectRemoved`, not silence.
    #[test]
    fn a_closed_project_is_reported_removed() {
        let before = state(InterpretedStatus::Working, vec![terminal("tab-1:primary")]);
        let out = deltas(&before, &HostState::default());
        assert!(
            matches!(out.as_slice(), [Delta::ProjectRemoved { .. }]),
            "{out:?}"
        );
    }

    /// Git refreshing from `unknown` to collected is a `Git` delta — the R2
    /// transition the browser renders as `git: ?` becoming real facts.
    #[test]
    fn git_becoming_collected_is_a_git_delta() {
        use crate::git::status::{WorktreeChanges, WorktreeStatus};

        let before = state(InterpretedStatus::Working, vec![terminal("tab-1:primary")]);
        let mut after = before.clone();
        after.projects[0].sessions[0].git = git_bar(GitFacts {
            status: Some(&WorktreeStatus {
                branch: "flightdeck/fix-login".to_string(),
                base_branch: "main".to_string(),
                dirty: false,
                changes: WorktreeChanges::default(),
                ahead: 0,
                behind: 0,
                upstream: None,
                base_drift: 0,
                worktree_path: std::path::PathBuf::from("/tmp/wt"),
            }),
            fallback_branch: "",
        });

        let out = deltas(&before, &after);
        assert_eq!(out.len(), 1, "{out:?}");
        match &out[0] {
            Delta::Git { git, .. } => {
                assert!(git.collected, "the browser stops rendering `git: ?`");
                assert!(!git.has_upstream, "and starts rendering `no-upstream`");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The selection is shared (D3), so a move on either surface is one frame.
    #[test]
    fn a_selection_move_is_one_selection_delta() {
        let before = state(InterpretedStatus::Working, vec![terminal("tab-1:primary")]);
        let mut after = before.clone();
        after.selection.session_id = Some(crate::contracts::TabId("tab-1".to_string()));

        let out = deltas(&before, &after);
        assert!(matches!(out.as_slice(), [Delta::Selection(_)]), "{out:?}");
    }
}
