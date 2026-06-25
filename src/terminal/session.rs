//! The per-tab terminal session: one primary agent terminal plus N child shell
//! terminals in the same worktree (SPECS §19, §25).
//!
//! Children may outlive the primary and are not persisted (SPECS §19).

use crate::contracts::{FlightDeckError, ProcessState, PtyBackend, PtySession, PtySize, Result};
use crate::tui::selection::{screen_row_to_rfb, Point, Selection};
use std::path::Path;

/// Scrollback lines kept by each terminal's VT parser.
const SCROLLBACK: usize = 2000;

/// Whether a terminal hosts the primary agent or a child shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalKind {
    Primary,
    Child,
}

/// A single terminal (primary or child): its live PTY session plus a VT100
/// parser that turns the raw PTY byte stream into a renderable screen grid.
pub struct Terminal {
    pub kind: TerminalKind,
    pub title: String,
    session: Box<dyn PtySession>,
    parser: vt100::Parser,
    /// The active mouse text selection, if any (SPECS §20).
    selection: Option<Selection>,
}

impl Terminal {
    /// Construct a terminal wrapping a spawned session, with a VT parser sized
    /// to the terminal's viewport.
    fn new(kind: TerminalKind, title: String, session: Box<dyn PtySession>, size: PtySize) -> Self {
        Terminal {
            kind,
            title,
            session,
            parser: vt100::Parser::new(size.rows.max(1), size.cols.max(1), SCROLLBACK),
            selection: None,
        }
    }

    /// The terminal's process state.
    pub fn process_state(&self) -> ProcessState {
        self.session.process_state()
    }

    /// Mutable access to the underlying session.
    pub fn session_mut(&mut self) -> &mut dyn PtySession {
        self.session.as_mut()
    }

    /// Feed raw PTY output bytes into the VT parser (updates the screen grid).
    pub fn process_output(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    /// The current parsed screen, for rendering.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Whether the hosted application has enabled xterm mouse reporting. When
    /// true, mouse events (e.g. wheel scroll) should be forwarded to the PTY so
    /// the app's own scroll region / scrollbar responds, exactly as in a real
    /// terminal emulator (SPECS §20).
    pub fn wants_mouse(&self) -> bool {
        self.parser.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    /// The mouse-report encoding the hosted application expects.
    pub fn mouse_encoding(&self) -> vt100::MouseProtocolEncoding {
        self.parser.screen().mouse_protocol_encoding()
    }

    /// Whether the hosted application has enabled bracketed paste mode (DECSET
    /// 2004). When true, pasted text should be wrapped in the `ESC [200~` /
    /// `ESC [201~` guards so the app (e.g. Claude Code, OpenCode, a shell)
    /// treats a multi-line paste as one atomic insert instead of executing each
    /// line as it arrives.
    pub fn bracketed_paste(&self) -> bool {
        self.parser.screen().bracketed_paste()
    }

    /// Scroll the viewport `lines` rows up into the VT100 scrollback. Used for
    /// plain (non-mouse-aware) output; clamped to the available scrollback.
    pub fn scroll_up(&mut self, lines: usize) {
        let cur = self.parser.screen().scrollback();
        self.parser
            .screen_mut()
            .set_scrollback(cur.saturating_add(lines));
    }

    /// Scroll the viewport `lines` rows back down toward the live bottom.
    pub fn scroll_down(&mut self, lines: usize) {
        let cur = self.parser.screen().scrollback();
        self.parser
            .screen_mut()
            .set_scrollback(cur.saturating_sub(lines));
    }

    /// Snap the viewport back to the live bottom (scrollback offset 0).
    pub fn scroll_to_bottom(&mut self) {
        self.parser.screen_mut().set_scrollback(0);
    }

    // --- Mouse text selection (SPECS §20) ---------------------------------

    /// The active selection, if any (for rendering the highlight).
    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    /// Whether a non-empty selection exists (something is actually highlighted).
    pub fn has_selection(&self) -> bool {
        self.selection.map(|s| !s.is_empty()).unwrap_or(false)
    }

    /// Clear any active selection.
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Begin a selection at a visible screen cell (drag start).
    pub fn begin_selection(&mut self, screen_row: u16, col: u16) {
        let p = self.point_at(screen_row, col);
        self.selection = Some(Selection::new(p));
    }

    /// Move the selection head to a visible screen cell (drag move). No-op if no
    /// selection is in progress.
    pub fn update_selection(&mut self, screen_row: u16, col: u16) {
        let p = self.point_at(screen_row, col);
        if let Some(sel) = self.selection.as_mut() {
            sel.head = p;
        }
    }

    /// Map a visible screen cell to a scroll-stable [`Point`], clamping to the
    /// current screen bounds and offset.
    fn point_at(&self, screen_row: u16, col: u16) -> Point {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let offset = screen.scrollback();
        let row = screen_row.min(rows.saturating_sub(1));
        let col = col.min(cols.saturating_sub(1));
        Point {
            rows_from_bottom: screen_row_to_rfb(row, rows, offset).max(0),
            col,
        }
    }

    /// Extract the currently-selected text, reading from scrollback as needed.
    ///
    /// Lines are joined with `\n` and trailing whitespace is trimmed per line.
    /// Restores the viewport's scrollback offset before returning.
    pub fn selected_text(&mut self) -> Option<String> {
        let sel = self.selection?;
        if sel.is_empty() {
            return None;
        }
        let (rows, cols) = self.parser.screen().size();
        let saved = self.parser.screen().scrollback();
        let (first, last) = sel.first_last();

        let mut lines: Vec<String> = Vec::new();
        let mut rfb = first.rows_from_bottom;
        while rfb >= last.rows_from_bottom {
            if let Some((c0, c1)) = sel.col_range_for_rfb(rfb, cols) {
                // Bring this content line into view at the bottom-most row (the
                // offset clamps internally for very old lines).
                self.parser.screen_mut().set_scrollback(rfb.max(0) as usize);
                let actual = self.parser.screen().scrollback();
                let screen_row = (rows as i64 - 1) - rfb + actual as i64;
                if (0..rows as i64).contains(&screen_row) {
                    lines.push(self.read_row(
                        screen_row as u16,
                        c0,
                        c1.min(cols.saturating_sub(1)),
                    ));
                }
            }
            rfb -= 1;
        }

        self.parser.screen_mut().set_scrollback(saved);
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }

    /// Read the cell contents of a visible `screen_row` from column `c0` to `c1`
    /// inclusive, with trailing whitespace trimmed.
    fn read_row(&self, screen_row: u16, c0: u16, c1: u16) -> String {
        let screen = self.parser.screen();
        let mut s = String::new();
        for c in c0..=c1 {
            let contents = screen.cell(screen_row, c).map(|cell| cell.contents());
            match contents {
                Some(text) if !text.is_empty() => s.push_str(text),
                _ => s.push(' '),
            }
        }
        s.trim_end().to_string()
    }

    /// Resize both the VT screen grid and the underlying PTY (SPECS §23). The
    /// active selection is dropped, as content reflows under a new width.
    pub fn resize(&mut self, size: PtySize) -> Result<()> {
        self.selection = None;
        self.parser
            .screen_mut()
            .set_size(size.rows.max(1), size.cols.max(1));
        self.session.resize(size)
    }
}

/// A tab's terminal session: one optional primary + ordered child terminals,
/// tracking the currently selected child (SPECS §19).
#[derive(Default)]
pub struct Session {
    primary: Option<Terminal>,
    children: Vec<Terminal>,
    selected_child: Option<usize>,
}

impl Session {
    /// Create an empty session.
    pub fn new() -> Self {
        Session::default()
    }

    /// Spawn the primary agent terminal (SPECS §17).
    pub fn spawn_primary(
        &mut self,
        backend: &dyn PtyBackend,
        cmd: &str,
        args: &[String],
        cwd: &Path,
        size: PtySize,
    ) -> Result<()> {
        let session = backend.spawn(cmd, args, cwd, size)?;
        self.primary = Some(Terminal::new(
            TerminalKind::Primary,
            cmd.to_string(),
            session,
            size,
        ));
        Ok(())
    }

    /// Spawn a new child shell terminal in the worktree, returning its index
    /// (SPECS §19).
    pub fn spawn_child(
        &mut self,
        backend: &dyn PtyBackend,
        cmd: &str,
        args: &[String],
        cwd: &Path,
        size: PtySize,
    ) -> Result<usize> {
        let session = backend.spawn(cmd, args, cwd, size)?;
        self.children.push(Terminal::new(
            TerminalKind::Child,
            cmd.to_string(),
            session,
            size,
        ));
        let index = self.children.len() - 1;
        self.selected_child = Some(index);
        Ok(index)
    }

    /// Select a child terminal by index (SPECS §19).
    pub fn switch_child(&mut self, index: usize) -> Result<()> {
        if index >= self.children.len() {
            return Err(FlightDeckError::Other(format!(
                "no child terminal at index {index}"
            )));
        }
        self.selected_child = Some(index);
        Ok(())
    }

    /// Close a child terminal by index (SPECS §19).
    pub fn close_child(&mut self, index: usize) -> Result<()> {
        if index >= self.children.len() {
            return Err(FlightDeckError::Other(format!(
                "no child terminal at index {index}"
            )));
        }
        // Force-terminate the child's process tree before removing it.
        self.children[index].session_mut().terminate_tree()?;
        self.children.remove(index);

        // Fix up the selected child: clear if no children remain, otherwise
        // clamp to a still-valid index, shifting down when we removed an entry
        // at or before the current selection.
        self.selected_child = match self.selected_child {
            _ if self.children.is_empty() => None,
            Some(sel) if sel == index => Some(index.min(self.children.len() - 1)),
            Some(sel) if sel > index => Some(sel - 1),
            other => other,
        };
        Ok(())
    }

    /// The currently selected child index, if any.
    pub fn selected_child(&self) -> Option<usize> {
        self.selected_child
    }

    /// Focus the primary agent terminal (clear any child selection, SPECS §19).
    pub fn focus_primary(&mut self) {
        self.selected_child = None;
    }

    /// The currently active terminal: the selected child shell, or the primary
    /// agent when no child is selected (SPECS §19, §20).
    pub fn active(&self) -> Option<&Terminal> {
        match self.selected_child {
            Some(c) => self.children.get(c),
            None => self.primary.as_ref(),
        }
    }

    /// Mutable access to the currently active terminal (see [`Session::active`]).
    pub fn active_mut(&mut self) -> Option<&mut Terminal> {
        match self.selected_child {
            Some(c) => self.children.get_mut(c),
            None => self.primary.as_mut(),
        }
    }

    /// Number of child terminals.
    pub fn child_count(&self) -> usize {
        self.children.len()
    }

    /// The primary terminal, if spawned.
    pub fn primary(&self) -> Option<&Terminal> {
        self.primary.as_ref()
    }

    /// Mutable access to the primary terminal, if spawned.
    pub fn primary_mut(&mut self) -> Option<&mut Terminal> {
        self.primary.as_mut()
    }

    /// Process state of the primary, or [`ProcessState::NotStarted`].
    pub fn primary_state(&self) -> ProcessState {
        match &self.primary {
            Some(t) => t.process_state(),
            None => ProcessState::NotStarted,
        }
    }

    /// A child terminal by index.
    pub fn child(&self, index: usize) -> Option<&Terminal> {
        self.children.get(index)
    }

    /// Mutable access to a child terminal by index.
    pub fn child_mut(&mut self, index: usize) -> Option<&mut Terminal> {
        self.children.get_mut(index)
    }

    /// Send Ctrl-C to the primary agent (SPECS §25 default close action).
    pub fn ctrl_c_primary(&mut self) -> Result<()> {
        if let Some(primary) = self.primary.as_mut() {
            primary.session_mut().send_ctrl_c()?;
        }
        Ok(())
    }

    /// Send Ctrl-C to the primary and all child terminals (SPECS §25).
    pub fn ctrl_c_all(&mut self) -> Result<()> {
        if let Some(primary) = self.primary.as_mut() {
            primary.session_mut().send_ctrl_c()?;
        }
        for child in self.children.iter_mut() {
            child.session_mut().send_ctrl_c()?;
        }
        Ok(())
    }

    /// Force-terminate every process in this session (SPECS §25 force path).
    pub fn terminate_all(&mut self) -> Result<()> {
        if let Some(primary) = self.primary.as_mut() {
            primary.session_mut().terminate_tree()?;
        }
        for child in self.children.iter_mut() {
            child.session_mut().terminate_tree()?;
        }
        Ok(())
    }

    /// Whether all terminals (primary + children) have stopped (SPECS §25
    /// "close only if all processes have stopped").
    pub fn all_stopped(&self) -> bool {
        let primary_stopped = self
            .primary
            .as_ref()
            .map(|t| t.process_state() != ProcessState::Running)
            .unwrap_or(true);
        primary_stopped
            && self
                .children
                .iter()
                .all(|c| c.process_state() != ProcessState::Running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakePty;

    const CWD: &str = "/wt";

    fn sz() -> PtySize {
        PtySize::default()
    }

    // §26: creates primary terminal.
    #[test]
    fn creates_primary_terminal() {
        let pty = FakePty::new();
        pty.queue_session();
        let mut session = Session::new();
        session
            .spawn_primary(&pty, "opencode", &[], Path::new(CWD), sz())
            .unwrap();

        assert!(session.primary().is_some());
        assert_eq!(session.primary().unwrap().kind, TerminalKind::Primary);
        assert_eq!(session.primary().unwrap().title, "opencode");
        assert_eq!(session.primary_state(), ProcessState::Running);
    }

    // §26: creates child terminal (index returned, selected, count increments).
    #[test]
    fn creates_child_terminal() {
        let pty = FakePty::new();
        pty.queue_session();
        let mut session = Session::new();

        let idx = session
            .spawn_child(&pty, "zsh", &[], Path::new(CWD), sz())
            .unwrap();

        assert_eq!(idx, 0);
        assert_eq!(session.child_count(), 1);
        assert_eq!(session.selected_child(), Some(0));
        assert_eq!(session.child(0).unwrap().kind, TerminalKind::Child);
    }

    // §26: switches child terminal.
    #[test]
    fn switches_child_terminal() {
        let pty = FakePty::new();
        pty.queue_session();
        pty.queue_session();
        let mut session = Session::new();

        session
            .spawn_child(&pty, "zsh", &[], Path::new(CWD), sz())
            .unwrap();
        session
            .spawn_child(&pty, "zsh", &[], Path::new(CWD), sz())
            .unwrap();
        assert_eq!(session.selected_child(), Some(1));

        session.switch_child(0).unwrap();
        assert_eq!(session.selected_child(), Some(0));

        // Out-of-range is refused.
        assert!(session.switch_child(9).is_err());
        assert_eq!(session.selected_child(), Some(0));
    }

    // §26: closes child terminal (removed; selected_child fixed up).
    #[test]
    fn closes_child_terminal() {
        let pty = FakePty::new();
        let h0 = pty.queue_session();
        pty.queue_session();
        pty.queue_session();
        let mut session = Session::new();

        for _ in 0..3 {
            session
                .spawn_child(&pty, "zsh", &[], Path::new(CWD), sz())
                .unwrap();
        }
        assert_eq!(session.child_count(), 3);
        assert_eq!(session.selected_child(), Some(2));

        // Close the first child: it is terminated and removed; selection (2)
        // shifts down to 1 because an earlier entry was removed.
        session.close_child(0).unwrap();
        assert!(h0.terminated());
        assert_eq!(session.child_count(), 2);
        assert_eq!(session.selected_child(), Some(1));

        // Closing the currently-selected last child clamps selection.
        session.switch_child(1).unwrap();
        session.close_child(1).unwrap();
        assert_eq!(session.child_count(), 1);
        assert_eq!(session.selected_child(), Some(0));

        // Closing the final child clears the selection.
        session.close_child(0).unwrap();
        assert_eq!(session.child_count(), 0);
        assert_eq!(session.selected_child(), None);

        // Out-of-range close is refused.
        assert!(session.close_child(0).is_err());
    }

    // §26: sends Ctrl-C (primary only, then all).
    #[test]
    fn sends_ctrl_c_to_primary_and_all() {
        let pty = FakePty::new();
        let primary = pty.queue_session();
        let child = pty.queue_session();
        let mut session = Session::new();

        session
            .spawn_primary(&pty, "opencode", &[], Path::new(CWD), sz())
            .unwrap();
        session
            .spawn_child(&pty, "zsh", &[], Path::new(CWD), sz())
            .unwrap();

        // Default close action: Ctrl-C the primary only.
        session.ctrl_c_primary().unwrap();
        assert_eq!(primary.ctrl_c_count(), 1);
        assert_eq!(child.ctrl_c_count(), 0);

        // Ctrl-C all hits primary and every child.
        session.ctrl_c_all().unwrap();
        assert_eq!(primary.ctrl_c_count(), 2);
        assert_eq!(child.ctrl_c_count(), 1);
    }

    // §26: terminate_all forces every process tree.
    #[test]
    fn terminate_all_forces_every_tree() {
        let pty = FakePty::new();
        let primary = pty.queue_session();
        let child = pty.queue_session();
        let mut session = Session::new();

        session
            .spawn_primary(&pty, "opencode", &[], Path::new(CWD), sz())
            .unwrap();
        session
            .spawn_child(&pty, "zsh", &[], Path::new(CWD), sz())
            .unwrap();

        session.terminate_all().unwrap();
        assert!(primary.terminated());
        assert!(child.terminated());
        assert!(session.all_stopped());
    }

    // §26: handles process exit (state reflected; all_stopped true).
    #[test]
    fn handles_process_exit() {
        let pty = FakePty::new();
        let primary = pty.queue_session();
        let mut session = Session::new();
        session
            .spawn_primary(&pty, "opencode", &[], Path::new(CWD), sz())
            .unwrap();

        assert_eq!(session.primary_state(), ProcessState::Running);
        assert!(!session.all_stopped());

        primary.set_state(ProcessState::Exited(0));
        assert_eq!(session.primary_state(), ProcessState::Exited(0));
        assert!(session.all_stopped());
    }

    // §26: all_stopped accounts for still-running children.
    #[test]
    fn all_stopped_requires_children_stopped() {
        let pty = FakePty::new();
        let primary = pty.queue_session();
        let child = pty.queue_session();
        let mut session = Session::new();
        session
            .spawn_primary(&pty, "opencode", &[], Path::new(CWD), sz())
            .unwrap();
        session
            .spawn_child(&pty, "zsh", &[], Path::new(CWD), sz())
            .unwrap();

        primary.set_state(ProcessState::Exited(0));
        // Child still running → not all stopped.
        assert!(!session.all_stopped());
        child.set_state(ProcessState::Stopped);
        assert!(session.all_stopped());
    }

    // §26: empty session reports all_stopped and no-op control signals.
    #[test]
    fn empty_session_is_stopped_and_noop() {
        let mut session = Session::new();
        assert_eq!(session.primary_state(), ProcessState::NotStarted);
        assert!(session.all_stopped());
        // No primary/children: control calls are no-ops, not errors.
        session.ctrl_c_primary().unwrap();
        session.ctrl_c_all().unwrap();
        session.terminate_all().unwrap();
    }

    // §20: `active` follows the child selection (primary when none selected).
    #[test]
    fn active_terminal_follows_selection() {
        let pty = FakePty::new();
        pty.queue_session();
        pty.queue_session();
        let mut session = Session::new();
        session
            .spawn_primary(&pty, "opencode", &[], Path::new(CWD), sz())
            .unwrap();
        session
            .spawn_child(&pty, "zsh", &[], Path::new(CWD), sz())
            .unwrap();

        // A child is selected after spawning it.
        assert_eq!(session.active().unwrap().kind, TerminalKind::Child);
        session.focus_primary();
        assert_eq!(session.active().unwrap().kind, TerminalKind::Primary);
        assert!(session.active_mut().is_some());
    }

    // §20: a TUI that enables xterm mouse reporting is detected, with encoding.
    #[test]
    fn detects_mouse_mode_and_encoding() {
        let pty = FakePty::new();
        pty.queue_session();
        let mut session = Session::new();
        session
            .spawn_primary(&pty, "opencode", &[], Path::new(CWD), sz())
            .unwrap();

        let term = session.active_mut().unwrap();
        assert!(!term.wants_mouse());
        // Enable mouse tracking (1000) with SGR encoding (1006), as a full-screen
        // TUI like opencode does.
        term.process_output(b"\x1b[?1000h\x1b[?1006h");
        assert!(term.wants_mouse());
        assert_eq!(term.mouse_encoding(), vt100::MouseProtocolEncoding::Sgr);
    }

    // §20: a drag selects text on the visible screen and extracts it.
    #[test]
    fn selects_and_extracts_visible_text() {
        let pty = FakePty::new();
        pty.queue_session();
        let mut session = Session::new();
        session
            .spawn_primary(&pty, "agent", &[], Path::new(CWD), sz())
            .unwrap();
        let term = session.active_mut().unwrap();
        term.process_output(b"hello world\r\n");

        // "hello world" is on the top screen row (row 0). Select cols 0..=4.
        term.begin_selection(0, 0);
        term.update_selection(0, 4);
        assert!(term.has_selection());
        assert_eq!(term.selected_text().as_deref(), Some("hello"));

        // Extend across the whole word range; trailing blanks are trimmed.
        term.update_selection(0, 40);
        assert_eq!(term.selected_text().as_deref(), Some("hello world"));

        term.clear_selection();
        assert!(!term.has_selection());
        assert_eq!(term.selected_text(), None);
    }

    // §20: a multi-line selection joins rows with newlines.
    #[test]
    fn selects_multiple_lines() {
        let pty = FakePty::new();
        pty.queue_session();
        let mut session = Session::new();
        session
            .spawn_primary(&pty, "agent", &[], Path::new(CWD), sz())
            .unwrap();
        let term = session.active_mut().unwrap();
        term.process_output(b"line one\r\nline two\r\n");

        // Rows 0 and 1 hold the two lines. Select from start of row 0 to end of
        // row 1.
        term.begin_selection(0, 0);
        term.update_selection(1, 40);
        assert_eq!(term.selected_text().as_deref(), Some("line one\nline two"));
    }

    // §20: a selection survives scrolling — it stays pinned to its content and
    // can still be extracted after scrolling into history.
    #[test]
    fn selection_pinned_across_scroll() {
        let pty = FakePty::new();
        pty.queue_session();
        let mut session = Session::new();
        session
            .spawn_primary(&pty, "agent", &[], Path::new(CWD), sz())
            .unwrap();
        let term = session.active_mut().unwrap();
        for i in 0..40 {
            term.process_output(format!("row {i:02}\r\n").as_bytes());
        }
        // Scroll up so older content is visible, select a line, then scroll
        // further: the extracted text must be the same line.
        term.scroll_up(5);
        let row = 0u16; // top visible row at this offset
        term.begin_selection(row, 0);
        term.update_selection(row, 5);
        let before = term.selected_text();
        assert!(before.is_some());
        term.scroll_up(3);
        assert_eq!(term.selected_text(), before, "selection must stay pinned");
        term.scroll_to_bottom();
        assert_eq!(term.selected_text(), before, "even back at the bottom");
    }

    // §20: local scrollback scrolls up into history and snaps back to the bottom.
    #[test]
    fn scrolls_local_scrollback() {
        let pty = FakePty::new();
        pty.queue_session();
        let mut session = Session::new();
        session
            .spawn_primary(&pty, "opencode", &[], Path::new(CWD), sz())
            .unwrap();

        let term = session.active_mut().unwrap();
        // Feed more lines than the 24-row viewport so scrollback accumulates.
        for i in 0..40 {
            term.process_output(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(term.screen().scrollback(), 0);

        term.scroll_up(3);
        assert_eq!(term.screen().scrollback(), 3);
        term.scroll_down(1);
        assert_eq!(term.screen().scrollback(), 2);
        term.scroll_to_bottom();
        assert_eq!(term.screen().scrollback(), 0);

        // Scrolling down at the bottom is a no-op (saturating).
        term.scroll_down(5);
        assert_eq!(term.screen().scrollback(), 0);
    }

    // §26: handles failed process start (spawn_primary surfaces Err).
    #[test]
    fn handles_failed_process_start() {
        let pty = FakePty::new();
        pty.fail_next_spawn();
        let mut session = Session::new();

        let res = session.spawn_primary(&pty, "missing", &[], Path::new(CWD), sz());
        assert!(res.is_err());
        assert!(session.primary().is_none());
        assert_eq!(session.primary_state(), ProcessState::NotStarted);
    }

    // §26: failed child spawn does not mutate session.
    #[test]
    fn handles_failed_child_start() {
        let pty = FakePty::new();
        pty.fail_next_spawn();
        let mut session = Session::new();

        assert!(session
            .spawn_child(&pty, "missing", &[], Path::new(CWD), sz())
            .is_err());
        assert_eq!(session.child_count(), 0);
        assert_eq!(session.selected_child(), None);
    }
}
