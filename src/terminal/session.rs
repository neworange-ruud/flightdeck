//! The per-tab terminal session: one primary agent terminal plus N child shell
//! terminals in the same worktree (SPECS §19, §25).
//!
//! Children may outlive the primary and are not persisted (SPECS §19).

use crate::contracts::{FlightDeckError, ProcessState, PtyBackend, PtySession, PtySize, Result};
use std::path::Path;

/// Whether a terminal hosts the primary agent or a child shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalKind {
    Primary,
    Child,
}

/// A single terminal (primary or child) and its live PTY session.
pub struct Terminal {
    pub kind: TerminalKind,
    pub title: String,
    session: Box<dyn PtySession>,
}

impl Terminal {
    /// The terminal's process state.
    pub fn process_state(&self) -> ProcessState {
        self.session.process_state()
    }

    /// Mutable access to the underlying session.
    pub fn session_mut(&mut self) -> &mut dyn PtySession {
        self.session.as_mut()
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
        self.primary = Some(Terminal {
            kind: TerminalKind::Primary,
            title: cmd.to_string(),
            session,
        });
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
        self.children.push(Terminal {
            kind: TerminalKind::Child,
            title: cmd.to_string(),
            session,
        });
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
