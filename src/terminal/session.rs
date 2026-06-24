//! The per-tab terminal session: one primary agent terminal plus N child shell
//! terminals in the same worktree (SPECS §19, §25).
//!
//! Children may outlive the primary and are not persisted (SPECS §19).

use crate::contracts::{ProcessState, PtyBackend, PtySession, PtySize, Result};
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
        let _ = (backend, cmd, args, cwd, size);
        todo!("T6: spawn primary via backend, store as primary")
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
        let _ = (backend, cmd, args, cwd, size);
        todo!("T6: spawn child, push, select it, return index")
    }

    /// Select a child terminal by index (SPECS §19).
    pub fn switch_child(&mut self, index: usize) -> Result<()> {
        let _ = index;
        todo!("T6: bounds-check and set selected_child")
    }

    /// Close a child terminal by index (SPECS §19).
    pub fn close_child(&mut self, index: usize) -> Result<()> {
        let _ = index;
        todo!("T6: terminate + remove child, fix selected_child")
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
        todo!("T6: send_ctrl_c on primary if present")
    }

    /// Send Ctrl-C to the primary and all child terminals (SPECS §25).
    pub fn ctrl_c_all(&mut self) -> Result<()> {
        todo!("T6: send_ctrl_c on primary and every child")
    }

    /// Force-terminate every process in this session (SPECS §25 force path).
    pub fn terminate_all(&mut self) -> Result<()> {
        todo!("T6: terminate_tree on primary and every child")
    }

    /// Whether all terminals (primary + children) have stopped (SPECS §25
    /// "close only if all processes have stopped").
    pub fn all_stopped(&self) -> bool {
        todo!("T6: true iff primary and all children are not running")
    }
}
