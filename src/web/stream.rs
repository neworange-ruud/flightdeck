//! PTY bytes out, keystrokes in (`specs/WEB_INTERFACE.md` D2, D4, D8, Q3, §5.1).
//!
//! This is the host-side half of the terminal stream: the piece that sits
//! between FlightDeck's PTYs and [`super::server`]'s sockets. It is
//! deliberately **synchronous and runtime-free** — it is driven from the TUI's
//! render tick, on the same thread that owns `AppState`, because that is the
//! only thread allowed to touch a PTY.
//!
//! ## The three jobs
//!
//! 1. **Tee.** `src/lib.rs`'s `drain_pty_output` already reads every live PTY
//!    once per tick and feeds the desktop's own `vt100` parser.
//!    [`TerminalStreams::pty_output`] is called from the same place with the
//!    same bytes, *after* the parser has had them. That is the whole of D2: the
//!    browser gets the actual PTY byte stream and the desktop's parse is
//!    untouched — two readers of one buffer, not a second reader of the fd.
//! 2. **Input.** [`TerminalStreams::apply_input`] writes a controlling viewer's
//!    keystrokes to the named terminal's PTY and answers every single one with
//!    an [`Ack`]. The server does not ack: it has *forwarded* a frame, which is
//!    not the same claim as "the PTY took it".
//! 3. **Resume.** [`TerminalStreams::attach_frames`] answers a returning
//!    viewer's byte cursors from the ring buffers (Q3).
//!
//! ## What is structurally absent
//!
//! **There is no way to resize a PTY from this module.** D4 is unconditional:
//! the desktop owns PTY geometry. [`TerminalHost`] — the only seam through
//! which this module can touch a terminal at all — has exactly one method, and
//! it writes input. A [`WebInbound::Resize`] therefore cannot reach
//! `portable_pty` through here, not because a check declines it but because
//! there is no call to make. What [`TerminalStreams::apply_inbound`] does with
//! a `Resize` is record the reported viewport for the seat chip, and nothing
//! else.
//!
//! That is asserted, not merely documented: `tests/web_server.rs`'s
//! `a_resize_frame_never_resizes_a_pty` drives a real socket into real
//! [`crate::terminal::session::Terminal`]s backed by
//! [`crate::testing::FakePty`], whose sessions **count every `resize` call**,
//! and asserts the count is still zero — having first proved on the same fake
//! that the desktop's own resize path does increment it, so the counter is not
//! vacuous.
//!
//! ## Ordering, dedup, and the watermark (§5.1)
//!
//! [`Input::seq`] is monotonic per viewer **across reconnects**, and
//! [`Snapshot::last_input_seq`] tells a returning browser what the host already
//! took. The host side of that contract is one number per viewer: the highest
//! seq actually written to a PTY.
//!
//! * `seq > watermark` → written, watermark advanced, [`AckOutcome::Applied`].
//! * `seq <= watermark` → **not** written, [`AckOutcome::Ignored`]. This is the
//!   replay of a keystroke the browser had queued but had not yet been told
//!   landed. Writing it again would type it twice; writing it *after* a higher
//!   seq already landed would type it out of order. Both are worse than not
//!   writing it, and §5.1's "never silently dropped" is satisfied by the ack,
//!   which names the reason.
//!
//! A **gap** (seq jumps 3 → 7) is applied, and the watermark moves to 7. One
//! WebSocket cannot reorder frames within itself, so a gap means the browser
//! genuinely does not have 4–6 to send; refusing 7 until they arrive would
//! stall a live terminal forever waiting for keystrokes that do not exist. If
//! 4–6 do turn up later they are below the watermark and are ignored, which is
//! the same correct answer as any other late frame.
//!
//! ## A keystroke for a terminal that has gone
//!
//! Never silently discarded. A stale [`TerminalId`] — the browser typed into a
//! tab the desktop has since closed, or into a session that exited — is
//! answered [`AckOutcome::Rejected`] with a sentence, and the watermark does
//! **not** advance, because nothing was applied. A PTY that refuses the write
//! is answered the same way, with the OS's reason.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::web::protocol::{
    Ack, AckOutcome, Input, TermBytes, TermCursor, TerminalId, ViewerId, Viewport,
};
use crate::web::replay::{ByteOffset, ReplayBuffer, Resume};
use crate::web::server::{WebInbound, WebOutbound};

mod host_state;

pub use host_state::{
    deltas, geometry_of, git_bar, is_git_unknown, lifecycle_reporting, project_view, session_view,
    GitFacts, SessionFacts, TerminalFacts,
};

#[cfg(test)]
mod tests;

// ===========================================================================
// Terminal identity
// ===========================================================================

/// The id of a tab's primary agent terminal.
///
/// `<tab-id>:primary`, the spelling [`TerminalId`] documents. A restarted agent
/// keeps this id and continues its byte stream rather than starting a new one:
/// the offsets stay monotonic, a viewer's cursor stays valid across the
/// restart, and the agent repaints anyway. A fresh id would buy the browser a
/// discontinuity it has no use for.
pub fn primary_terminal_id(tab_id: &str) -> TerminalId {
    TerminalId::new(format!("{tab_id}:primary"))
}

/// The id of one child terminal, keyed by its session-minted
/// [`stream_id`](crate::terminal::session::Terminal::stream_id) rather than its
/// index in the tab.
pub fn child_terminal_id(tab_id: &str, stream_id: u64) -> TerminalId {
    TerminalId::new(format!("{tab_id}:child:{stream_id}"))
}

// ===========================================================================
// The seam onto the PTYs
// ===========================================================================

/// Where a keystroke goes.
///
/// One method, on purpose. The TUI implements it over `AppState`; the tests
/// implement it over a bare [`crate::terminal::session::Session`]. Neither
/// implementation can be asked to resize anything, because this trait cannot
/// express the request — see the module doc, "What is structurally absent".
pub trait TerminalHost {
    /// Write `bytes` to the PTY behind `terminal_id`.
    ///
    /// Returns [`Written::Ok`] only when the bytes really reached the PTY, since
    /// that is the claim the [`Ack`] will make on this method's behalf.
    fn write_terminal_input(&mut self, terminal_id: &TerminalId, bytes: &[u8]) -> Written;
}

/// The outcome of one [`TerminalHost::write_terminal_input`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Written {
    /// The bytes reached the PTY.
    Ok,
    /// No such terminal on this host: a stale id from before a reconnect, or a
    /// tab the desktop has closed.
    NoSuchTerminal,
    /// The terminal exists but its process is gone, so there is nothing to
    /// write to. Distinct from [`Written::NoSuchTerminal`] because the browser
    /// can still *see* the terminal and deserves to be told why its typing did
    /// nothing.
    NotRunning,
    /// The PTY refused the write, with the OS's reason.
    Failed(String),
}

/// What [`TerminalStreams::apply_input`] decided, before it becomes an [`Ack`].
///
/// A separate type from [`AckOutcome`] so the unit tests can assert about the
/// *decision* — which is where the ordering and dedup rules live — rather than
/// about a three-variant wire enum that collapses several of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputVerdict {
    /// Written to the PTY; the watermark advanced to this seq.
    Applied,
    /// At or below the watermark: already applied on an earlier connection, or
    /// arriving after a higher seq already landed. Not written.
    AlreadyApplied {
        /// The watermark that refused it.
        watermark: u64,
    },
    /// The named terminal is not on this host.
    UnknownTerminal,
    /// The terminal is there; its process is not.
    TerminalClosed,
    /// The PTY refused the write.
    WriteFailed(String),
}

impl InputVerdict {
    /// The wire answer for this verdict. Every verdict has one: §5.1 forbids a
    /// keystroke disappearing without a trace, so there is deliberately no
    /// "and this one we say nothing about" case.
    pub fn ack(&self, seq: u64) -> Ack {
        let (outcome, detail) = match self {
            InputVerdict::Applied => (AckOutcome::Applied, None),
            InputVerdict::AlreadyApplied { watermark } => (
                AckOutcome::Ignored,
                Some(format!(
                    "already applied — the host is at seq {watermark}, this frame is {seq}"
                )),
            ),
            InputVerdict::UnknownTerminal => (
                AckOutcome::Rejected,
                Some("that terminal is not open on the host any more".to_string()),
            ),
            InputVerdict::TerminalClosed => (
                AckOutcome::Rejected,
                Some("that terminal's process has exited".to_string()),
            ),
            InputVerdict::WriteFailed(reason) => (
                AckOutcome::Rejected,
                Some(format!("the terminal refused the write: {reason}")),
            ),
        };
        Ack {
            seq,
            outcome,
            detail,
        }
    }

    /// Whether the bytes reached a PTY.
    pub fn applied(&self) -> bool {
        matches!(self, InputVerdict::Applied)
    }
}

/// Resolve `terminal_id` inside one tab's [`Session`] and write to it.
///
/// Returns `None` when the id does not name a terminal of *this* session, so a
/// caller holding several tabs can try the next one; `Some` once the id has
/// been claimed, whether or not the write succeeded.
///
/// This is the single place that turns a wire id back into a PTY, shared by the
/// TUI's [`TerminalHost`] and by the integration tests, so the two cannot
/// diverge on what a stale id means. **It writes input and nothing else** —
/// there is no resize here, and D4 is why.
///
/// Writing snaps the pane back to the live bottom and drops any mouse
/// selection, exactly as typing on the desktop does (`write_active_pty`): a
/// keystroke that scrolls the desktop user's view differently depending on
/// which surface typed it would be a worse surprise than the scroll itself.
///
/// [`Session`]: crate::terminal::session::Session
pub fn write_into_session(
    session: &mut crate::terminal::session::Session,
    tab_id: &str,
    terminal_id: &TerminalId,
    bytes: &[u8],
) -> Option<Written> {
    let terminal = if terminal_id == &primary_terminal_id(tab_id) {
        session.primary_mut()?
    } else {
        let index = (0..session.child_count()).find(|&c| {
            session
                .child(c)
                .is_some_and(|child| terminal_id == &child_terminal_id(tab_id, child.stream_id()))
        })?;
        session.child_mut(index)?
    };

    if !matches!(
        terminal.process_state(),
        crate::contracts::domain::ProcessState::Running
            | crate::contracts::domain::ProcessState::Starting
    ) {
        return Some(Written::NotRunning);
    }
    terminal.clear_selection();
    terminal.scroll_to_bottom();
    Some(match terminal.session_mut().write_input(bytes) {
        Ok(()) => Written::Ok,
        Err(e) => Written::Failed(e.to_string()),
    })
}

// ===========================================================================
// One terminal's stream
// ===========================================================================

/// One live terminal's replay ring plus the liveness the browser renders.
#[derive(Debug)]
struct TerminalStream {
    replay: ReplayBuffer,
    alive: bool,
    exit_code: Option<i32>,
}

// ===========================================================================
// The registry
// ===========================================================================

/// Every browser-visible terminal's byte stream, plus the per-viewer input
/// watermark.
///
/// Owned by the TUI for the life of the process — **not** by the web server,
/// and not created and destroyed with it. A terminal's replay ring must survive
/// `Stop Web Interface` followed by `Start Web Interface`, or the first viewer
/// after a restart would paint an empty screen for a terminal that has been
/// running for an hour.
#[derive(Debug)]
pub struct TerminalStreams {
    capacity: usize,
    streams: HashMap<TerminalId, TerminalStream>,
    /// Highest [`Input::seq`] actually written to a PTY, per viewer. See the
    /// module doc.
    watermarks: HashMap<ViewerId, u64>,
    /// Insertion order of `watermarks`, so the map can be bounded without
    /// evicting a viewer that is still typing.
    watermark_order: Vec<ViewerId>,
    /// Last viewport each viewer reported (D4 — display only).
    viewports: HashMap<ViewerId, Viewport>,
}

/// How many viewers' input watermarks are retained.
///
/// Matches `server::REMEMBERED_INPUT_CURSORS`: the server remembers that many
/// viewers' forwarded cursors to answer [`Snapshot::last_input_seq`], and this
/// module remembering fewer would let a reconnect be promised a watermark this
/// side had already forgotten.
///
/// [`Snapshot::last_input_seq`]: crate::web::protocol::Snapshot::last_input_seq
const REMEMBERED_WATERMARKS: usize = 64;

impl TerminalStreams {
    /// A registry whose per-terminal rings hold `capacity_bytes`
    /// (`[web] replay_bytes`, Q2).
    pub fn new(capacity_bytes: usize) -> Self {
        TerminalStreams {
            capacity: capacity_bytes,
            streams: HashMap::new(),
            watermarks: HashMap::new(),
            watermark_order: Vec::new(),
            viewports: HashMap::new(),
        }
    }

    /// The configured ring capacity, for `Snapshot::replay_capacity_bytes`.
    pub fn capacity_bytes(&self) -> usize {
        self.capacity
    }

    /// How many terminals are being streamed.
    pub fn len(&self) -> usize {
        self.streams.len()
    }

    /// True when nothing is being streamed.
    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }

    /// Whether this terminal has a stream.
    pub fn knows(&self, terminal_id: &TerminalId) -> bool {
        self.streams.contains_key(terminal_id)
    }

    // -- bytes out ---------------------------------------------------------

    /// Tee one chunk of raw PTY output (D2).
    ///
    /// Returns the frame to fan out to every attached viewer, or `None` for an
    /// empty chunk. The ring is created on demand: the tee is the authority on
    /// which terminals exist, so a terminal nobody registered still streams
    /// rather than silently losing its first output.
    pub fn pty_output(&mut self, terminal_id: &TerminalId, bytes: &[u8]) -> Option<TermBytes> {
        if bytes.is_empty() {
            return None;
        }
        let capacity = self.capacity;
        let stream = self
            .streams
            .entry(terminal_id.clone())
            .or_insert_with(|| TerminalStream {
                replay: ReplayBuffer::new(capacity),
                alive: true,
                exit_code: None,
            });
        // The offset of the *first* byte of this chunk, which is the current
        // offset before the write. Read before, not after minus len, so a
        // capacity-clamping write cannot skew it.
        let offset = stream.replay.current_offset();
        stream.replay.write(bytes);
        Some(TermBytes::live(terminal_id.clone(), offset, bytes.to_vec()))
    }

    /// Register a terminal before it has produced any output, so its
    /// `TerminalView` exists in the very first snapshot. Idempotent.
    pub fn open(&mut self, terminal_id: TerminalId) {
        let capacity = self.capacity;
        self.streams
            .entry(terminal_id)
            .or_insert_with(|| TerminalStream {
                replay: ReplayBuffer::new(capacity),
                alive: true,
                exit_code: None,
            });
    }

    /// Mark a terminal's process gone. The ring is **kept**: a viewer that
    /// reconnects still wants to read what the process said before it died.
    pub fn closed(&mut self, terminal_id: &TerminalId, exit_code: Option<i32>) {
        if let Some(stream) = self.streams.get_mut(terminal_id) {
            stream.alive = false;
            stream.exit_code = exit_code;
        }
    }

    /// Drop the streams of terminals the host no longer has at all (their tab
    /// was closed), releasing their ring buffers. Terminals that merely exited
    /// stay — use [`TerminalStreams::closed`] for those.
    pub fn retain(&mut self, live: &HashSet<TerminalId>) {
        self.streams.retain(|id, _| live.contains(id));
    }

    /// Total bytes this terminal has ever written (`TerminalView::byte_len`).
    pub fn byte_len(&self, terminal_id: &TerminalId) -> ByteOffset {
        self.streams
            .get(terminal_id)
            .map(|s| s.replay.current_offset())
            .unwrap_or(0)
    }

    /// The oldest offset still retained (`TerminalView::replay_from`).
    pub fn replay_from(&self, terminal_id: &TerminalId) -> ByteOffset {
        self.streams
            .get(terminal_id)
            .map(|s| s.replay.oldest_offset())
            .unwrap_or(0)
    }

    /// Whether this terminal's process is still running.
    pub fn alive(&self, terminal_id: &TerminalId) -> bool {
        self.streams
            .get(terminal_id)
            .map(|s| s.alive)
            .unwrap_or(false)
    }

    /// This terminal's exit code, when it exited normally.
    pub fn exit_code(&self, terminal_id: &TerminalId) -> Option<i32> {
        self.streams.get(terminal_id).and_then(|s| s.exit_code)
    }

    // -- resume (Q3) -------------------------------------------------------

    /// The [`TermBytes`] to send one attaching viewer, given the cursors it
    /// presented (Q3).
    ///
    /// A viewer that names no cursor for a terminal is treated as having
    /// `next_offset: 0` — the [`TermCursor`] type's own documented meaning of
    /// zero ("everything you still have"). That is not a shortcut: it makes a
    /// first attach and a reconnect the *same* code path, so the truncation
    /// answer cannot drift between them. A first attach onto a terminal whose
    /// ring has already wrapped is therefore flagged `truncated` too, which is
    /// honest — output was discarded before this viewer could receive it, which
    /// is exactly what the flag means.
    ///
    /// A cursor naming a terminal this host does not have yields no frame: the
    /// snapshot the viewer is about to receive does not list that terminal, so
    /// the browser learns it is gone from the authoritative place rather than
    /// from an empty byte frame.
    ///
    /// Frames are returned sorted by terminal id, so the order is stable for
    /// tests and for a reader watching two terminals repaint.
    pub fn attach_frames(&self, cursors: &[TermCursor]) -> Vec<TermBytes> {
        let mut ids: Vec<&TerminalId> = self.streams.keys().collect();
        ids.sort();
        let mut out = Vec::new();
        for id in ids {
            let from = cursors
                .iter()
                .find(|c| &c.terminal_id == id)
                .map(|c| c.next_offset)
                .unwrap_or(0);
            if let Some(frame) = self.resume_frame(id, from) {
                out.push(frame);
            }
        }
        out
    }

    /// One terminal's resume answer, or `None` when there is nothing to send.
    ///
    /// Split out from [`TerminalStreams::attach_frames`] because this is the
    /// decision the unit tests are about.
    pub fn resume_frame(&self, terminal_id: &TerminalId, from: ByteOffset) -> Option<TermBytes> {
        let stream = self.streams.get(terminal_id)?;
        match stream.replay.resume(from) {
            Resume::UpToDate => None,
            Resume::Tail(slice) => {
                let data = slice.chunk.to_vec();
                if data.is_empty() {
                    return None;
                }
                Some(TermBytes {
                    terminal_id: terminal_id.clone(),
                    // A tail begins exactly where the viewer asked.
                    offset: from,
                    data,
                    truncated: false,
                })
            }
            Resume::Truncated(slice) => {
                let data = slice.chunk.to_vec();
                if data.is_empty() {
                    // The ring holds nothing (capacity 0, or everything the
                    // terminal wrote has been discarded). There are no bytes to
                    // hand over, and a zero-length frame flagged `truncated`
                    // would be a claim with no payload to attach it to. The
                    // viewer learns the gap from `TerminalView::replay_from`.
                    return None;
                }
                Some(TermBytes {
                    terminal_id: terminal_id.clone(),
                    // A truncated replay begins at the oldest retained byte,
                    // which is *ahead* of what the viewer asked for. That
                    // inequality is the gap, and `truncated` names it.
                    offset: stream.replay.oldest_offset(),
                    data,
                    truncated: true,
                })
            }
        }
    }

    // -- input in (§5.1) ---------------------------------------------------

    /// The highest seq written to a PTY for this viewer.
    pub fn watermark(&self, viewer_id: &ViewerId) -> u64 {
        self.watermarks.get(viewer_id).copied().unwrap_or(0)
    }

    /// Carry a dropped connection's watermark onto the [`ViewerId`] resuming
    /// it, so a reconnect's replayed queue is deduped against what the previous
    /// socket actually applied rather than against zero.
    pub fn adopt_watermark(&mut self, previous: &ViewerId, now: &ViewerId) {
        let seq = self.watermark(previous);
        if seq > 0 {
            self.set_watermark(now, seq);
        }
    }

    fn set_watermark(&mut self, viewer_id: &ViewerId, seq: u64) {
        if let Some(existing) = self.watermarks.get_mut(viewer_id) {
            *existing = (*existing).max(seq);
            return;
        }
        if self.watermark_order.len() >= REMEMBERED_WATERMARKS {
            let oldest = self.watermark_order.remove(0);
            self.watermarks.remove(&oldest);
        }
        self.watermark_order.push(viewer_id.clone());
        self.watermarks.insert(viewer_id.clone(), seq);
    }

    /// Apply one viewer's keystrokes, in order, exactly once (§5.1).
    ///
    /// See the module doc for the watermark rule and for what happens to input
    /// aimed at a terminal that has gone.
    pub fn apply_input(
        &mut self,
        viewer_id: &ViewerId,
        input: &Input,
        host: &mut dyn TerminalHost,
    ) -> InputVerdict {
        let watermark = self.watermark(viewer_id);
        if input.seq <= watermark {
            return InputVerdict::AlreadyApplied { watermark };
        }
        let verdict = match host.write_terminal_input(&input.terminal_id, &input.data) {
            Written::Ok => InputVerdict::Applied,
            Written::NoSuchTerminal => InputVerdict::UnknownTerminal,
            Written::NotRunning => InputVerdict::TerminalClosed,
            Written::Failed(reason) => InputVerdict::WriteFailed(reason),
        };
        if verdict.applied() {
            // Only a write that landed moves the watermark. A rejected frame
            // must stay re-sendable: the browser may legitimately retry the
            // same seq once the terminal is back.
            self.set_watermark(viewer_id, input.seq);
        }
        verdict
    }

    // -- viewports (D4, display only) --------------------------------------

    /// The last viewport this viewer reported, if any. Never an input to PTY
    /// sizing — see the module doc.
    pub fn viewport(&self, viewer_id: &ViewerId) -> Option<Viewport> {
        self.viewports.get(viewer_id).copied()
    }

    /// Forget everything remembered about a viewer whose socket closed, apart
    /// from its watermark, which a reconnect still needs.
    pub fn viewer_detached(&mut self, viewer_id: &ViewerId) {
        self.viewports.remove(viewer_id);
    }

    // -- the inbound drain -------------------------------------------------

    /// Handle one [`WebInbound`] frame, returning what to send back.
    ///
    /// This is the whole of the TUI's per-tick work for the terminal stream:
    /// drain the channel, call this, and push each returned [`WebOutbound`] at
    /// `WebServerHandle::send`. Frames this module does not own
    /// ([`WebInbound::Command`], the seat bookkeeping) come back as an empty
    /// vector and are the caller's business.
    ///
    /// **The `Resize` arm is the D4 guarantee in code.** It records the
    /// viewport and returns nothing. `host` is not even consulted, and could
    /// not be asked to resize anything if it were: [`TerminalHost`] has no such
    /// method.
    pub fn apply_inbound(
        &mut self,
        message: &WebInbound,
        host: &mut dyn TerminalHost,
    ) -> Vec<WebOutbound> {
        match message {
            WebInbound::ViewerAttached {
                viewer_id,
                cursors,
                resume_viewer,
                ..
            } => {
                // A reconnect gets a fresh id, so the watermark has to travel
                // onto it or the browser's replayed queue would be typed twice
                // (§5.1). This is the only place that can do it: the server
                // remembers what it forwarded, not what a PTY took.
                if let Some(previous) = resume_viewer {
                    self.adopt_watermark(previous, viewer_id);
                }
                self.attach_frames(cursors)
                    .into_iter()
                    .map(|frame| WebOutbound::Viewer {
                        viewer_id: viewer_id.clone(),
                        msg: crate::web::protocol::ServerMsg::TermBytes(frame),
                    })
                    .collect()
            }
            WebInbound::ViewerDetached { viewer_id } => {
                self.viewer_detached(viewer_id);
                Vec::new()
            }
            WebInbound::Input { viewer_id, input } => {
                let verdict = self.apply_input(viewer_id, input, host);
                vec![WebOutbound::Viewer {
                    viewer_id: viewer_id.clone(),
                    msg: crate::web::protocol::ServerMsg::Ack(verdict.ack(input.seq)),
                }]
            }
            WebInbound::Resize {
                viewer_id,
                viewport,
            } => {
                // D4: display only. There is no PTY call on this path, and
                // `TerminalHost` cannot express one.
                self.viewports.insert(viewer_id.clone(), *viewport);
                Vec::new()
            }
            // Seats and commands belong to the caller, not to the byte stream.
            WebInbound::SeatsChanged { .. } | WebInbound::Command { .. } => Vec::new(),
        }
    }
}
