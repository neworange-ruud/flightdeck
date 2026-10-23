//! Starting processes that outlive their parent (SPECS §33).
//!
//! `worker start` spawns the supervisor, and the supervisor spawns `worker
//! run`, as **new processes**: `std::process::Command` with `setsid()` in
//! `pre_exec`, never a `fork()` of a process that already runs a runtime. The
//! new session detaches the child from the terminal, so closing it (SIGHUP)
//! or pressing Ctrl-C there reaches neither the supervisor nor the worker.
//!
//! Unix only; off unix every spawn fails with `Unsupported` and no signal is
//! ever delivered (the CLI refuses before getting here).

use std::path::Path;
use std::process::Child;

/// Spawn `exe args…` in `cwd` in a new session, detached: stdin and stdout
/// null, stderr appended to `stderr_to` (or null). Returns its pid.
///
/// The child is reaped by a background thread when it exits, so a caller
/// polling [`alive`] sees it go rather than linger as a zombie.
pub fn spawn_detached(
    exe: &Path,
    args: &[&str],
    cwd: &Path,
    stderr_to: Option<&Path>,
) -> std::io::Result<u32> {
    imp::spawn_detached(exe, args, cwd, stderr_to)
}

/// Spawn `exe args…` in `cwd` in a new session with stdout and stderr piped
/// back to the caller (the supervisor logs them), stdin null.
pub fn spawn_supervised(exe: &Path, args: &[&str], cwd: &Path) -> std::io::Result<Child> {
    imp::spawn_supervised(exe, args, cwd)
}

/// Send `sig` to `pid`; whether it was delivered. Pid 0 (which `kill(2)`
/// reads as "my whole process group") is refused.
pub fn signal(pid: u32, sig: i32) -> bool {
    imp::signal(pid, sig)
}

/// Ask `pid` to exit (`SIGTERM`).
pub fn terminate(pid: u32) -> bool {
    imp::terminate(pid)
}

/// Kill `pid` outright (`SIGKILL`).
pub fn kill(pid: u32) -> bool {
    imp::kill(pid)
}

/// Whether a process with `pid` exists (`kill(pid, 0)`). A hint only: pids
/// are recycled, the lock files are the truth.
pub fn alive(pid: u32) -> bool {
    imp::alive(pid)
}

#[cfg(unix)]
mod imp {
    use std::fs::OpenOptions;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};

    pub fn spawn_detached(
        exe: &Path,
        args: &[&str],
        cwd: &Path,
        stderr_to: Option<&Path>,
    ) -> std::io::Result<u32> {
        let stderr = match stderr_to {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Stdio::from(OpenOptions::new().create(true).append(true).open(path)?)
            }
            None => Stdio::null(),
        };
        let mut child = new_session(Command::new(exe))
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()?;
        let pid = child.id();
        // Reap it whenever it exits; if this process exits first, init does.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(pid)
    }

    pub fn spawn_supervised(exe: &Path, args: &[&str], cwd: &Path) -> std::io::Result<Child> {
        new_session(Command::new(exe))
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    }

    /// Run the child in a session of its own.
    fn new_session(mut cmd: Command) -> Command {
        // SAFETY: the closure runs in the child between fork and exec, where
        // only async-signal-safe calls are allowed. It calls `setsid()`
        // (async-signal-safe) and reads errno; it allocates nothing and takes
        // no locks.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        cmd
    }

    pub fn signal(pid: u32, sig: i32) -> bool {
        match i32::try_from(pid) {
            // SAFETY: kill(2) takes plain integers; a positive pid only ever
            // addresses that one process.
            Ok(pid) if pid > 0 => unsafe { libc::kill(pid, sig) == 0 },
            _ => false,
        }
    }

    pub fn terminate(pid: u32) -> bool {
        signal(pid, libc::SIGTERM)
    }

    pub fn kill(pid: u32) -> bool {
        signal(pid, libc::SIGKILL)
    }

    pub fn alive(pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        // SAFETY: signal 0 delivers nothing; it only checks the pid.
        let delivered = unsafe { libc::kill(pid, 0) } == 0;
        // EPERM: it exists but belongs to someone else.
        delivered || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(not(unix))]
mod imp {
    use std::path::Path;
    use std::process::Child;

    fn unsupported() -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            crate::worker::cli::NOT_SUPPORTED,
        )
    }

    pub fn spawn_detached(
        _exe: &Path,
        _args: &[&str],
        _cwd: &Path,
        _stderr_to: Option<&Path>,
    ) -> std::io::Result<u32> {
        Err(unsupported())
    }

    pub fn spawn_supervised(_exe: &Path, _args: &[&str], _cwd: &Path) -> std::io::Result<Child> {
        Err(unsupported())
    }

    pub fn signal(_pid: u32, _sig: i32) -> bool {
        false
    }

    pub fn terminate(_pid: u32) -> bool {
        false
    }

    pub fn kill(_pid: u32) -> bool {
        false
    }

    pub fn alive(_pid: u32) -> bool {
        false
    }
}

#[cfg(all(test, unix))]
mod tests;
