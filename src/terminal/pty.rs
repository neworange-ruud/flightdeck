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

/// Grace budget between the graceful SIGTERM and the SIGKILL fallback: up to
/// `STEPS * STEP` (~1s). Short enough to keep quit/teardown snappy, long enough
/// for an agent to flush and exit on its own.
#[cfg(unix)]
const TERMINATE_GRACE_STEPS: usize = 20;
#[cfg(unix)]
const TERMINATE_STEP: std::time::Duration = std::time::Duration::from_millis(50);

/// What graceful termination ended up doing (for tests / clarity). Unix-only:
/// Windows terminates via `taskkill /T /F` and never uses this policy.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TerminationOutcome {
    /// The process was already gone; nothing was signalled.
    AlreadyExited,
    /// It exited after the graceful stop, within the grace window.
    Graceful,
    /// It outlasted the grace window and had to be force-killed.
    Forced,
}

/// Escalation policy for terminating a process tree. If it is still alive, send
/// a graceful stop, then poll up to `grace_steps` times (sleeping via `step`
/// between checks) for it to exit, and force-kill if it never does. Pure control
/// flow with injected effects, so it is unit-testable without a real process.
/// Unix-only (see [`TerminationOutcome`]).
#[cfg(unix)]
pub(crate) fn escalate_terminate(
    is_alive: impl Fn() -> bool,
    graceful: impl FnOnce(),
    grace_steps: usize,
    step: impl Fn(),
    force: impl FnOnce(),
) -> TerminationOutcome {
    if !is_alive() {
        return TerminationOutcome::AlreadyExited;
    }
    graceful();
    for _ in 0..grace_steps {
        step();
        if !is_alive() {
            return TerminationOutcome::Graceful;
        }
    }
    force();
    TerminationOutcome::Forced
}

impl PtyBackend for PortablePtyBackend {
    fn spawn(
        &self,
        cmd: &str,
        args: &[String],
        env: &[(String, String)],
        cwd: &Path,
        size: PtySize,
    ) -> Result<Box<dyn PtySession>> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(map_size(size))
            .map_err(|e| FlightDeckError::Io(format!("openpty failed: {e}")))?;

        let mut builder = build_command_builder(cmd, args);
        builder.cwd(cwd);
        for (key, value) in env {
            builder.env(key, value);
        }

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
        #[cfg(unix)]
        {
            // Graceful, whole-group shutdown. The pty child is a session/process
            // -group leader (portable-pty calls `setsid`), so its PGID == PID and
            // signalling the *negative* PID reaches the agent AND its descendants
            // that stay in the group — not just the direct child. We send SIGTERM
            // first (let the agent flush/exit cleanly), wait a short grace window,
            // and only SIGKILL if it outlives it.
            let pid = self.child.lock().expect("child poisoned").process_id();
            if let Some(pid) = pid {
                let pgid = pid as i32;
                let signal_group = |sig: i32| {
                    // Ignore ESRCH etc.: a already-dead group is fine.
                    unsafe {
                        libc::kill(-pgid, sig);
                    }
                };
                let is_alive = || {
                    matches!(
                        self.child.lock().expect("child poisoned").try_wait(),
                        Ok(None)
                    )
                };
                escalate_terminate(
                    is_alive,
                    || signal_group(libc::SIGTERM),
                    TERMINATE_GRACE_STEPS,
                    || std::thread::sleep(TERMINATE_STEP),
                    || signal_group(libc::SIGKILL),
                );
            } else {
                // No pid to signal (already reaped): best-effort direct kill.
                let _ = self.killer.kill();
            }
        }
        #[cfg(not(any(windows, unix)))]
        {
            // Platforms without POSIX signals: best-effort direct child kill.
            self.killer
                .kill()
                .map_err(|e| FlightDeckError::Io(format!("failed to kill pty child: {e}")))?;
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
    #[cfg(unix)]
    use std::cell::Cell;

    #[cfg(unix)]
    #[test]
    fn escalate_skips_signals_when_already_exited() {
        let graceful = Cell::new(false);
        let forced = Cell::new(false);
        let out = escalate_terminate(
            || false,
            || graceful.set(true),
            5,
            || {},
            || forced.set(true),
        );
        assert_eq!(out, TerminationOutcome::AlreadyExited);
        assert!(!graceful.get(), "no graceful signal for a dead process");
        assert!(!forced.get(), "no force-kill for a dead process");
    }

    #[cfg(unix)]
    #[test]
    fn escalate_is_graceful_when_it_exits_within_grace() {
        let checks = Cell::new(0);
        let forced = Cell::new(false);
        // Alive for the first few liveness checks, then exits during the grace
        // window (before grace_steps is exhausted).
        let is_alive = || {
            let n = checks.get();
            checks.set(n + 1);
            n < 3
        };
        let out = escalate_terminate(is_alive, || {}, 10, || {}, || forced.set(true));
        assert_eq!(out, TerminationOutcome::Graceful);
        assert!(
            !forced.get(),
            "must not force-kill if it exits during grace"
        );
    }

    #[cfg(unix)]
    #[test]
    fn escalate_force_kills_when_still_alive_after_grace() {
        let graceful = Cell::new(false);
        let forced = Cell::new(false);
        let out = escalate_terminate(
            || true, // never exits
            || graceful.set(true),
            3,
            || {},
            || forced.set(true),
        );
        assert_eq!(out, TerminationOutcome::Forced);
        assert!(graceful.get(), "graceful stop is attempted first");
        assert!(forced.get(), "force-kill after the grace window");
    }

    // Real-process test (ignored: needs a real PTY + spawns processes). Verifies
    // that terminating a session reaps the WHOLE process group — including a
    // grandchild — rather than leaving it orphaned. Run with `--ignored`.
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn terminate_tree_reaps_process_group_including_grandchild() {
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().expect("tempdir");
        let pidfile = dir.path().join("grandchild.pid");
        // A shell that backgrounds a long `sleep` (a grandchild of flightdeck),
        // records its pid, and waits. Without group-killing, that sleep would
        // survive after the shell dies.
        let script = format!("sleep 300 & echo $! > {}; wait", pidfile.display());
        let backend = PortablePtyBackend;
        let mut session = backend
            .spawn(
                "sh",
                &["-c".to_string(), script],
                &[],
                dir.path(),
                PtySize::default(),
            )
            .expect("spawn sh");

        // Wait for the grandchild pid to be written.
        let mut pid: Option<i32> = None;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(p) = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
            {
                pid = Some(p);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let pid = pid.expect("grandchild pid recorded");
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "grandchild should be alive before terminate"
        );

        session.terminate_tree().expect("terminate");

        // The grandchild must be gone — the group was signalled, not just the
        // direct child.
        let mut gone = false;
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if unsafe { libc::kill(pid, 0) } == -1 {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(gone, "grandchild {pid} must be reaped with the group");
    }

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
                &[],
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
