//! Starting processes that outlive their parent (SPECS §33).
//!
//! `worker start` spawns the supervisor, and the supervisor spawns `worker
//! run`, as **new processes**: `std::process::Command` with `setsid()` in
//! `pre_exec`, never a `fork()` of a process that already runs a runtime.

// stub: filled by F

use std::path::Path;

fn not_implemented() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "worker process spawning is not implemented yet",
    )
}

/// Spawn `exe args…` in `cwd` in a new session, detached: stdin and stdout
/// null, stderr appended to `stderr_to` (or null). Returns its pid.
pub fn spawn_detached(
    _exe: &Path,
    _args: &[&str],
    _cwd: &Path,
    _stderr_to: Option<&Path>,
) -> std::io::Result<u32> {
    // stub: filled by F
    Err(not_implemented())
}

/// Spawn `exe args…` in `cwd` in a new session with stdout and stderr piped
/// back to the caller (the supervisor logs them).
pub fn spawn_supervised(
    _exe: &Path,
    _args: &[&str],
    _cwd: &Path,
) -> std::io::Result<std::process::Child> {
    // stub: filled by F
    Err(not_implemented())
}

/// Send `sig` to `pid`; whether it was delivered.
pub fn signal(_pid: u32, _sig: i32) -> bool {
    // stub: filled by F
    false
}

/// Whether a process with `pid` exists (`kill(pid, 0)`). A hint only: pids
/// are recycled, the lock files are the truth.
pub fn alive(_pid: u32) -> bool {
    // stub: filled by F
    false
}
