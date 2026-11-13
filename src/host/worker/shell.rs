//! The worker shells (`specs/WORKER_PRD.md` §4.4, W15, W35): login shells in
//! `$HOME`, each streamed to the attached window as its own terminal —
//! `worker:shell` for the first, `worker:shell:<n>` for the rest.
//!
//! Each runs as the worker's account (`$SHELL -l`, falling back to
//! `/bin/sh`), outlives a detach, and is restarted on demand after it exits.
//! A shell's stream id is kept across restarts so a window's offsets
//! continue, like a restarted primary. [`WorkerShells`] holds them; a window
//! opens and closes them (W35).

use std::path::Path;

use crate::contracts::{ProcessState, PtyBackend, PtySize, Result};
use crate::terminal::grid::TUI_EMULATOR;
use crate::terminal::session::{Terminal, MIN_GRID_COLS, MIN_GRID_ROWS};
use crate::web::protocol::{worker_shell_terminal_id, Geometry, TerminalId};
use crate::web::stream::Written;

/// What the window's shell tab shows about the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellFacts {
    /// `Shell <n>`.
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

/// One of the worker's login shells.
pub struct WorkerShell {
    /// The tab title, `Shell <n>`.
    title: String,
    terminal: Option<Terminal>,
    /// The size the next start spawns at: the last one asked for.
    size: PtySize,
    /// How the shell ended, once [`WorkerShell::poll_exit`] has reported it.
    exited: Option<Option<i32>>,
    /// How many times the shell was spawned.
    starts: u32,
}

impl WorkerShell {
    /// A shell not started yet, to be started at `size`, titled `Shell 1`.
    pub fn new(size: PtySize) -> Self {
        Self::numbered(1, size)
    }

    /// Shell number `n`, not started yet, to be started at `size`.
    pub fn numbered(n: u32, size: PtySize) -> Self {
        WorkerShell {
            title: format!("Shell {n}"),
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
        terminal.title = self.title.clone();
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
            title: self.title.clone(),
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

/// The most shells a worker keeps at once (W35): enough for any workflow, and
/// a bound on what a runaway client can make the worker spawn.
pub const MAX_WORKER_SHELLS: usize = 16;

/// Why a shell could not be opened, closed or restarted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellRefusal {
    /// [`MAX_WORKER_SHELLS`] are open already.
    TooMany,
    /// No shell has that id.
    NoSuchShell,
    /// The PTY did not spawn.
    Spawn(String),
}

impl std::fmt::Display for ShellRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShellRefusal::TooMany => write!(
                f,
                "The worker already has {MAX_WORKER_SHELLS} shells open. Close one first."
            ),
            ShellRefusal::NoSuchShell => f.write_str("That worker shell is already closed."),
            ShellRefusal::Spawn(e) => write!(f, "The worker shell did not start: {e}"),
        }
    }
}

/// Every worker shell, in the order they were opened (W35).
///
/// Numbers are minted from a counter and never reused while the worker runs,
/// so a stream id names one shell for the worker's whole life.
pub struct WorkerShells {
    shells: Vec<(TerminalId, WorkerShell)>,
    /// The number the next shell gets.
    next: u32,
    /// The size a new shell spawns at: the last one any shell was given.
    size: PtySize,
}

impl WorkerShells {
    /// No shells yet; the first opened is `Shell 1` at `size`.
    pub fn new(size: PtySize) -> Self {
        WorkerShells {
            shells: Vec::new(),
            next: 1,
            size,
        }
    }

    /// Open the next shell in `home` and return its id.
    pub fn open(
        &mut self,
        pty: &dyn PtyBackend,
        home: &Path,
    ) -> std::result::Result<TerminalId, ShellRefusal> {
        let env_shell = std::env::var("SHELL").ok();
        self.open_with(pty, home, env_shell.as_deref(), &|p| p.exists())
    }

    fn open_with(
        &mut self,
        pty: &dyn PtyBackend,
        home: &Path,
        env_shell: Option<&str>,
        exists: &dyn Fn(&Path) -> bool,
    ) -> std::result::Result<TerminalId, ShellRefusal> {
        if self.shells.len() >= MAX_WORKER_SHELLS {
            return Err(ShellRefusal::TooMany);
        }
        let n = self.next;
        let mut shell = WorkerShell::numbered(n, self.size);
        shell
            .start_with(pty, home, env_shell, exists)
            .map_err(|e| ShellRefusal::Spawn(e.to_string()))?;
        // Only a shell that started uses up its number.
        self.next += 1;
        let id = worker_shell_terminal_id(n);
        self.shells.push((id.clone(), shell));
        Ok(id)
    }

    /// End shell `id`'s process and forget it.
    pub fn close(&mut self, id: &TerminalId) -> std::result::Result<(), ShellRefusal> {
        let at = self
            .shells
            .iter()
            .position(|(have, _)| have == id)
            .ok_or(ShellRefusal::NoSuchShell)?;
        let (_, mut shell) = self.shells.remove(at);
        shell.terminate();
        Ok(())
    }

    /// Start shell `id` again in `home` (after it exited, or terminating it).
    pub fn restart(
        &mut self,
        id: &TerminalId,
        pty: &dyn PtyBackend,
        home: &Path,
    ) -> std::result::Result<(), ShellRefusal> {
        let shell = self.get_mut(id).ok_or(ShellRefusal::NoSuchShell)?;
        shell
            .restart(pty, home)
            .map_err(|e| ShellRefusal::Spawn(e.to_string()))
    }

    /// The first shell's id, while any shell is open.
    pub fn first(&self) -> Option<&TerminalId> {
        self.shells.first().map(|(id, _)| id)
    }

    /// Whether `id` is an open shell.
    pub fn contains(&self, id: &TerminalId) -> bool {
        self.shells.iter().any(|(have, _)| have == id)
    }

    /// How many shells are open.
    pub fn len(&self) -> usize {
        self.shells.len()
    }

    /// Whether no shell is open.
    pub fn is_empty(&self) -> bool {
        self.shells.is_empty()
    }

    /// Shell `id`, mutably.
    pub fn get_mut(&mut self, id: &TerminalId) -> Option<&mut WorkerShell> {
        self.shells
            .iter_mut()
            .find(|(have, _)| have == id)
            .map(|(_, shell)| shell)
    }

    /// Read every shell's waiting output, handing each chunk to `tee` with
    /// its shell's id; whether anything was read.
    pub fn drain(&mut self, tee: &mut dyn FnMut(&TerminalId, &[u8])) -> bool {
        let mut read = false;
        for (id, shell) in self.shells.iter_mut() {
            read |= shell.drain(&mut |bytes| tee(id, bytes));
        }
        read
    }

    /// Every shell seen to have exited on this turn, with its code.
    pub fn poll_exits(&mut self) -> Vec<(TerminalId, Option<i32>)> {
        self.shells
            .iter_mut()
            .filter_map(|(id, shell)| shell.poll_exit().map(|code| (id.clone(), code)))
            .collect()
    }

    /// Type `bytes` into shell `id`; `None` when there is no such shell.
    pub fn write(&mut self, id: &TerminalId, bytes: &[u8]) -> Option<Written> {
        self.get_mut(id).map(|shell| shell.write(bytes))
    }

    /// Resize shell `id`, and spawn later shells at `size`.
    pub fn resize(&mut self, id: &TerminalId, size: PtySize) {
        self.size = size;
        if let Some(shell) = self.get_mut(id) {
            shell.resize(size);
        }
    }

    /// Every started shell's facts, in order.
    pub fn facts(&self) -> Vec<(TerminalId, ShellFacts)> {
        self.shells
            .iter()
            .filter_map(|(id, shell)| shell.facts().map(|facts| (id.clone(), facts)))
            .collect()
    }

    /// Stop every shell (worker stop).
    pub fn terminate(&mut self) {
        for (_, shell) in self.shells.iter_mut() {
            shell.terminate();
        }
    }
}

// The worker is Linux and macOS only (W2); its tests use unix paths.
#[cfg(all(test, unix))]
mod tests;
