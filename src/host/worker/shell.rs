//! The worker shell (`specs/WORKER_PRD.md` §4.4, W15): a login shell in
//! `$HOME`, streamed to the attached window as terminal `worker:shell`.
//!
//! It runs as the worker's account (`$SHELL -l`, falling back to `/bin/sh`),
//! outlives a detach, and is restarted on demand after it exits. The stream id
//! is kept across restarts so a window's offsets continue, like a restarted
//! primary.

// stub: filled by D

use std::path::Path;

use crate::contracts::{FlightDeckError, PtyBackend, PtySize, Result};
use crate::web::protocol::Geometry;
use crate::web::stream::Written;

/// What the window's shell tab shows about the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellFacts {
    /// Always "Worker shell".
    pub title: String,
    pub geometry: Geometry,
    pub alive: bool,
    pub exit_code: Option<i32>,
}

/// The program and arguments of the login shell: `$SHELL` when it is set,
/// absolute and `exists`, else `/bin/sh`; always `-l`.
pub fn login_shell(
    _env_shell: Option<&str>,
    _exists: &dyn Fn(&Path) -> bool,
) -> (String, Vec<String>) {
    // stub: filled by D
    ("/bin/sh".to_string(), vec!["-l".to_string()])
}

/// The worker's one login shell.
pub struct WorkerShell {
    // stub: filled by D (terminal: Option<Terminal>, exited, starts)
    size: PtySize,
}

impl WorkerShell {
    /// A shell not started yet, to be started at `size`.
    pub fn new(size: PtySize) -> Self {
        WorkerShell { size }
    }

    /// Spawn the login shell in `home`.
    pub fn start(&mut self, _pty: &dyn PtyBackend, _home: &Path) -> Result<()> {
        // stub: filled by D
        Err(FlightDeckError::Other(
            "the worker shell is not implemented in this build yet".to_string(),
        ))
    }

    /// Terminate the shell if it is alive, then start a new one at the last
    /// size.
    pub fn restart(&mut self, pty: &dyn PtyBackend, home: &Path) -> Result<()> {
        // stub: filled by D
        self.start(pty, home)
    }

    /// Read whatever output is waiting, hand each chunk to `tee`; whether
    /// anything was read.
    pub fn drain(&mut self, _tee: &mut dyn FnMut(&[u8])) -> bool {
        // stub: filled by D
        false
    }

    /// `Some(exit code)` once, on the turn the shell is seen to have exited.
    pub fn poll_exit(&mut self) -> Option<Option<i32>> {
        // stub: filled by D
        None
    }

    /// Type `bytes` into the shell.
    pub fn write(&mut self, _bytes: &[u8]) -> Written {
        // stub: filled by D
        Written::NotRunning
    }

    /// Resize the shell's PTY; whether the size changed (equal is a no-op).
    pub fn resize(&mut self, size: PtySize) -> bool {
        // stub: filled by D (forward to the PTY)
        if self.size == size {
            return false;
        }
        self.size = size;
        true
    }

    /// The shell's facts, or `None` before it was ever started.
    pub fn facts(&self) -> Option<ShellFacts> {
        // stub: filled by D
        None
    }

    /// Stop the shell (worker stop).
    pub fn terminate(&mut self) {
        // stub: filled by D
    }
}
