//! Real [`PtyBackend`]/[`PtySession`] over `portable-pty` (SPECS §17, §25).
//!
//! A background reader thread drains PTY output into a shared buffer; the
//! session exposes a non-blocking read, input write, resize, Ctrl-C, and a
//! force terminate-tree path (SPECS §25).

use crate::contracts::{FlightDeckError, ProcessState, PtyBackend, PtySession, PtySize, Result};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize as PortPtySize};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// `portable-pty`-backed [`PtyBackend`].
#[derive(Debug, Default)]
pub struct PortablePtyBackend;

fn map_size(size: PtySize) -> PortPtySize {
    PortPtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Build the `portable-pty` command for an agent/shell command + args.
///
/// On Unix the command is spawned as-is. On Windows it must go through
/// [`resolve_windows_command`] first: `CreateProcess` can only launch PE
/// executables, but npm/pnpm/yarn install many CLIs (e.g. OpenCode) as a `.cmd`
/// batch wrapper on `PATH`. Spawning such a wrapper directly fails with
/// "%1 is not a valid Win32 application", so batch wrappers are run through the
/// command processor (`cmd.exe /d /c`).
fn build_command_builder(cmd: &str, args: &[String]) -> CommandBuilder {
    #[cfg(windows)]
    {
        match resolve_windows_command(cmd) {
            WindowsCommand::Batch(script) => {
                let comspec = std::env::var_os("ComSpec")
                    .unwrap_or_else(|| std::ffi::OsString::from("cmd.exe"));
                let mut builder = CommandBuilder::new(comspec);
                // /d skips any AutoRun registry command; /c runs then exits.
                builder.arg("/d");
                builder.arg("/c");
                builder.arg(script);
                builder.args(args);
                builder
            }
            WindowsCommand::Direct(target) => {
                let mut builder = CommandBuilder::new(target);
                builder.args(args);
                builder
            }
        }
    }
    #[cfg(not(windows))]
    {
        let mut builder = CommandBuilder::new(cmd);
        builder.args(args);
        builder
    }
}

/// How a command should be launched on Windows.
#[cfg(windows)]
#[derive(Debug, PartialEq, Eq)]
enum WindowsCommand {
    /// A `.cmd`/`.bat` wrapper to run via `cmd.exe /c <script>`.
    Batch(std::ffi::OsString),
    /// A directly-spawnable target (a PE executable, or an unresolved name left
    /// for `CreateProcess` to handle / fail on as before).
    Direct(std::ffi::OsString),
}

/// Resolve a command to either a batch wrapper (run via the command processor)
/// or a directly-spawnable target, mirroring how the OS would find it.
///
/// - A command that already carries an extension is classified by it
///   (`.cmd`/`.bat` → batch, anything else → direct).
/// - A bare name (or a path without an extension) is probed against the
///   `PATHEXT`-style set `.com/.exe/.bat/.cmd`: for a bare name across every
///   `PATH` entry, for a path-bearing name against that exact location. The
///   first existing file wins, classified by its extension.
/// - If nothing resolves, the original string is passed through as `Direct`.
#[cfg(windows)]
fn resolve_windows_command(cmd: &str) -> WindowsCommand {
    use std::ffi::OsString;
    use std::path::Path;

    let as_path = Path::new(cmd);
    if let Some(ext) = as_path.extension().and_then(|e| e.to_str()) {
        let ext = ext.to_ascii_lowercase();
        if ext == "cmd" || ext == "bat" {
            return WindowsCommand::Batch(OsString::from(cmd));
        }
        return WindowsCommand::Direct(OsString::from(cmd));
    }

    // No extension: probe candidate executable extensions, batch last so a real
    // .exe is preferred over a same-named wrapper.
    const EXTS: [&str; 4] = ["com", "exe", "bat", "cmd"];
    let classify = |base: &Path| -> Option<WindowsCommand> {
        for ext in EXTS {
            let candidate = base.with_extension(ext);
            if candidate.is_file() {
                return Some(if ext == "bat" || ext == "cmd" {
                    WindowsCommand::Batch(candidate.into_os_string())
                } else {
                    WindowsCommand::Direct(candidate.into_os_string())
                });
            }
        }
        None
    };

    let has_separator = cmd.contains('/') || cmd.contains('\\');
    if has_separator {
        if let Some(found) = classify(as_path) {
            return found;
        }
    } else if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if let Some(found) = classify(&dir.join(cmd)) {
                return found;
            }
        }
    }

    WindowsCommand::Direct(OsString::from(cmd))
}

impl PtyBackend for PortablePtyBackend {
    fn spawn(
        &self,
        cmd: &str,
        args: &[String],
        cwd: &Path,
        size: PtySize,
    ) -> Result<Box<dyn PtySession>> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(map_size(size))
            .map_err(|e| FlightDeckError::Io(format!("openpty failed: {e}")))?;

        let mut builder = build_command_builder(cmd, args);
        builder.cwd(cwd);

        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| FlightDeckError::Io(format!("failed to spawn {cmd}: {e}")))?;

        // The slave handle is no longer needed once the child is spawned; drop it
        // so EOF propagates correctly when the child exits.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| FlightDeckError::Io(format!("failed to clone pty reader: {e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| FlightDeckError::Io(format!("failed to take pty writer: {e}")))?;

        // Shared, non-blocking output buffer drained by `try_read_output`.
        let buffer: Arc<Mutex<VecDeque<u8>>> = Arc::new(Mutex::new(VecDeque::new()));
        let reader_buffer = Arc::clone(&buffer);

        // Reader thread owns only `Send` data (the boxed reader + the Arc).
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        if let Ok(mut buf) = reader_buffer.lock() {
                            buf.extend(&chunk[..n]);
                        } else {
                            break;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        let killer = child.clone_killer();

        Ok(Box::new(PortablePtySession {
            master: pair.master,
            writer,
            child: Mutex::new(child),
            killer,
            buffer,
            terminated: Mutex::new(false),
        }))
    }
}

/// A live `portable-pty` session implementing [`PtySession`].
///
/// All fields are `Send`, so the session itself is `Send` as the trait requires.
struct PortablePtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    // `try_wait` requires `&mut`, but `process_state` only has `&self`; the
    // `Mutex` gives us interior mutability so exits can be polled immutably.
    child: Mutex<Box<dyn Child + Send + Sync>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    buffer: Arc<Mutex<VecDeque<u8>>>,
    terminated: Mutex<bool>,
}

impl PtySession for PortablePtySession {
    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer
            .write_all(bytes)
            .map_err(|e| FlightDeckError::Io(format!("pty write failed: {e}")))?;
        self.writer
            .flush()
            .map_err(|e| FlightDeckError::Io(format!("pty flush failed: {e}")))?;
        Ok(())
    }

    fn resize(&mut self, size: PtySize) -> Result<()> {
        self.master
            .resize(map_size(size))
            .map_err(|e| FlightDeckError::Io(format!("pty resize failed: {e}")))
    }

    fn try_read_output(&mut self) -> Result<Vec<u8>> {
        let mut buf = self
            .buffer
            .lock()
            .map_err(|_| FlightDeckError::Io("pty output buffer poisoned".to_string()))?;
        if buf.is_empty() {
            return Ok(Vec::new());
        }
        Ok(buf.drain(..).collect())
    }

    fn send_ctrl_c(&mut self) -> Result<()> {
        self.write_input(&[0x03])
    }

    fn process_state(&self) -> ProcessState {
        if *self.terminated.lock().expect("terminated flag poisoned") {
            return ProcessState::Stopped;
        }
        let mut child = self.child.lock().expect("child poisoned");
        match child.try_wait() {
            Ok(None) => ProcessState::Running,
            Ok(Some(status)) => {
                if status.success() {
                    ProcessState::Exited(0)
                } else {
                    // Map any non-success (including signal-terminated) to the
                    // process's reported exit code.
                    ProcessState::Exited(status.exit_code() as i32)
                }
            }
            Err(_) => ProcessState::Failed,
        }
    }

    fn terminate_tree(&mut self) -> Result<()> {
        // On Windows the whole *tree* must die, not just the PTY's direct child.
        // An npm-installed agent CLI runs as `cmd.exe /d /c <wrapper>` (see
        // `build_command_builder`), which spawns the real agent (e.g. node.exe)
        // as a grandchild. Killing only the direct child (`cmd.exe`) leaves that
        // grandchild alive holding the worktree directory open (its cwd), so a
        // later `git worktree remove` / directory delete fails with a
        // permission-denied error. `taskkill /T /F` terminates the entire tree.
        #[cfg(windows)]
        {
            let pid = self.child.lock().expect("child poisoned").process_id();
            if let Some(pid) = pid {
                let _ = std::process::Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &pid.to_string()])
                    .output();
            }
            // Backstop, best-effort: the tree kill above may already have reaped
            // the direct child, in which case `kill()` would error.
            let _ = self.killer.kill();
        }
        #[cfg(not(windows))]
        {
            // Best-effort: kill the direct child of the PTY. On macOS/Linux this
            // does NOT recursively reap an arbitrary grandchild tree; children
            // that re-parent (daemonize / double-fork) may survive. Killing the
            // pty's direct child (the shell/agent) is acceptable, and dropping
            // the pty master delivers SIGHUP to the foreground group.
            let _ = self.killer.kill();
        }
        // Block until the direct child has actually exited so the OS releases the
        // handles it held — most importantly its working directory. On Windows
        // `TerminateProcess`/`taskkill` are asynchronous: they return before the
        // process is torn down, and a worktree directory cannot be deleted while
        // any process still has it open. Waiting here makes a subsequent
        // `git worktree remove` (abandon / merge cleanup) reliable. The wait is
        // bounded in practice because the tree has just been killed.
        let _ = self.child.lock().expect("child poisoned").wait();
        *self.terminated.lock().expect("terminated flag poisoned") = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real-PTY smoke test. Ignored by default (SPECS: real PTY must not run in
    // the standard test run). Spawns a trivial command and verifies the session
    // can be created and read without panicking.
    #[test]
    #[ignore]
    fn real_pty_echo_smoke() {
        let backend = PortablePtyBackend;
        let cwd = std::env::temp_dir();
        let mut session = backend
            .spawn(
                "echo",
                &["flightdeck".to_string()],
                &cwd,
                PtySize::default(),
            )
            .expect("spawn echo");

        // Give the reader thread a moment to drain output.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let out = session.try_read_output().expect("read output");
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("flightdeck"), "got: {text:?}");

        // Terminate is a no-op once the process has exited, but must not error.
        session.terminate_tree().expect("terminate");
    }

    // Windows command resolution: npm-installed CLIs (e.g. OpenCode) land on
    // PATH as a `.cmd` wrapper that CreateProcess can't launch directly; they
    // must run via `cmd.exe /c`. PE executables spawn directly.
    #[cfg(windows)]
    #[test]
    fn windows_classifies_batch_extensions() {
        assert_eq!(
            resolve_windows_command("opencode.cmd"),
            WindowsCommand::Batch("opencode.cmd".into())
        );
        assert_eq!(
            resolve_windows_command("setup.bat"),
            WindowsCommand::Batch("setup.bat".into())
        );
        // Case-insensitive extension match.
        assert_eq!(
            resolve_windows_command(r"C:\tools\opencode.CMD"),
            WindowsCommand::Batch(r"C:\tools\opencode.CMD".into())
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_classifies_exe_as_direct() {
        assert_eq!(
            resolve_windows_command("claude.exe"),
            WindowsCommand::Direct("claude.exe".into())
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_resolves_extensionless_path_to_cmd_wrapper() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("opencode.cmd");
        std::fs::write(&script, "@echo off\n").expect("write wrapper");
        // A path without an extension finds its `.cmd` sibling → batch.
        let base = dir.path().join("opencode");
        assert_eq!(
            resolve_windows_command(base.to_str().unwrap()),
            WindowsCommand::Batch(script.into_os_string())
        );
    }
}
