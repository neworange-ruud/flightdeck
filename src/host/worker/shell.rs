//! The worker shell (`specs/WORKER_PRD.md` §4.4, W15): a login shell in
//! `$HOME`, streamed to the attached window as terminal `worker:shell`.
//!
//! It runs as the worker's account (`$SHELL -l`, falling back to `/bin/sh`),
//! outlives a detach, and is restarted on demand after it exits. The stream id
//! is kept across restarts so a window's offsets continue, like a restarted
//! primary.

use std::path::Path;

use crate::contracts::{ProcessState, PtyBackend, PtySize, Result};
use crate::terminal::grid::TUI_EMULATOR;
use crate::terminal::session::{Terminal, MIN_GRID_COLS, MIN_GRID_ROWS};
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

/// The shell used when `$SHELL` cannot be.
const FALLBACK_SHELL: &str = "/bin/sh";

/// The most chunks one [`WorkerShell::drain`] reads, so a shell printing
/// without end cannot hold the worker's loop.
const MAX_CHUNKS_PER_DRAIN: usize = 64;

/// The program and arguments of the login shell: `$SHELL` when it is set,
/// absolute and `exists`, else `/bin/sh`; always `-l`.
pub fn login_shell(
    env_shell: Option<&str>,
    exists: &dyn Fn(&Path) -> bool,
) -> (String, Vec<String>) {
    let shell = env_shell
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| Path::new(s).is_absolute() && exists(Path::new(s)))
        .unwrap_or(FALLBACK_SHELL);
    (shell.to_string(), vec!["-l".to_string()])
}

/// The worker's one login shell.
pub struct WorkerShell {
    terminal: Option<Terminal>,
    /// The size the next start spawns at: the last one asked for.
    size: PtySize,
    /// How the shell ended, once [`WorkerShell::poll_exit`] has reported it.
    exited: Option<Option<i32>>,
    /// How many times the shell was spawned.
    starts: u32,
}

impl WorkerShell {
    /// A shell not started yet, to be started at `size`.
    pub fn new(size: PtySize) -> Self {
        WorkerShell {
            terminal: None,
            size,
            exited: None,
            starts: 0,
        }
    }

    /// Spawn the login shell in `home`.
    pub fn start(&mut self, pty: &dyn PtyBackend, home: &Path) -> Result<()> {
        let env_shell = std::env::var("SHELL").ok();
        self.start_with(pty, home, env_shell.as_deref(), &|p| p.exists())
    }

    fn start_with(
        &mut self,
        pty: &dyn PtyBackend,
        home: &Path,
        env_shell: Option<&str>,
        exists: &dyn Fn(&Path) -> bool,
    ) -> Result<()> {
        self.terminate();
        let (program, args) = login_shell(env_shell, exists);
        let env = [
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("COLORTERM".to_string(), "truecolor".to_string()),
        ];
        let mut terminal =
            Terminal::spawn(pty, TUI_EMULATOR, &program, &args, &env, home, self.size)?;
        terminal.title = "Worker shell".to_string();
        self.terminal = Some(terminal);
        self.exited = None;
        self.starts += 1;
        Ok(())
    }

    /// Terminate the shell if it is alive, then start a new one at the last
    /// size.
    pub fn restart(&mut self, pty: &dyn PtyBackend, home: &Path) -> Result<()> {
        self.terminate();
        self.start(pty, home)
    }

    /// Read whatever output is waiting, hand each chunk to `tee`; whether
    /// anything was read.
    pub fn drain(&mut self, tee: &mut dyn FnMut(&[u8])) -> bool {
        let Some(terminal) = self.terminal.as_mut() else {
            return false;
        };
        let mut read = false;
        for _ in 0..MAX_CHUNKS_PER_DRAIN {
            match terminal.session_mut().try_read_output() {
                Ok(bytes) if !bytes.is_empty() => {
                    read = true;
                    terminal.process_output(&bytes);
                    // Answer the shell's cursor-position probe, as an agent's
                    // terminal does: some prompts wait for it.
                    terminal.answer_cursor_position_query(&bytes);
                    tee(&bytes);
                }
                _ => break,
            }
        }
        terminal.tick();
        read
    }

    /// `Some(exit code)` once, on the turn the shell is seen to have exited.
    pub fn poll_exit(&mut self) -> Option<Option<i32>> {
        if self.exited.is_some() {
            return None;
        }
        let code = match self.terminal.as_ref()?.process_state() {
            ProcessState::Exited(code) => Some(code),
            // Stopped or failed: gone, without a code to report.
            ProcessState::Stopped | ProcessState::Failed | ProcessState::Lost => None,
            ProcessState::NotStarted | ProcessState::Starting | ProcessState::Running => {
                return None;
            }
        };
        self.exited = Some(code);
        Some(code)
    }

    /// Type `bytes` into the shell.
    pub fn write(&mut self, bytes: &[u8]) -> Written {
        let Some(terminal) = self.terminal.as_mut() else {
            return Written::NotRunning;
        };
        if !matches!(
            terminal.process_state(),
            ProcessState::Running | ProcessState::Starting
        ) {
            return Written::NotRunning;
        }
        terminal.clear_selection();
        terminal.scroll_to_bottom();
        match terminal.session_mut().write_input(bytes) {
            Ok(()) => Written::Ok,
            Err(e) => Written::Failed(e.to_string()),
        }
    }

    /// Resize the shell's PTY; whether the size changed (equal is a no-op).
    pub fn resize(&mut self, size: PtySize) -> bool {
        let Some(terminal) = self.terminal.as_mut() else {
            // Not running: the next start uses it.
            let changed = self.size != size;
            self.size = size;
            return changed;
        };
        let want = (size.rows.max(MIN_GRID_ROWS), size.cols.max(MIN_GRID_COLS));
        if terminal.screen().size() == want {
            self.size = size;
            return false;
        }
        self.size = size;
        terminal.resize(size).is_ok()
    }

    /// The shell's facts, or `None` before it was ever started.
    pub fn facts(&self) -> Option<ShellFacts> {
        let terminal = self.terminal.as_ref()?;
        let (rows, cols) = terminal.screen().size();
        let (alive, exit_code) = match terminal.process_state() {
            ProcessState::Exited(code) => (false, Some(code)),
            ProcessState::Running | ProcessState::Starting => (true, None),
            ProcessState::NotStarted
            | ProcessState::Stopped
            | ProcessState::Failed
            | ProcessState::Lost => (false, None),
        };
        Some(ShellFacts {
            title: "Worker shell".to_string(),
            geometry: Geometry { cols, rows },
            alive,
            exit_code,
        })
    }

    /// Stop the shell (worker stop).
    pub fn terminate(&mut self) {
        if let Some(terminal) = self.terminal.as_mut() {
            if matches!(
                terminal.process_state(),
                ProcessState::Running | ProcessState::Starting
            ) {
                let _ = terminal.session_mut().terminate_tree();
            }
        }
    }
}

// The worker is Linux and macOS only (W2); its tests use unix paths.
#[cfg(all(test, unix))]
mod tests;
