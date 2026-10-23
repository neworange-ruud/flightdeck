//! The worker log, `~/.flightdeck/logs/worker.log` (SPECS §33).
//!
//! The worker writes lines to its stderr; the supervisor appends them, and its
//! own, to a [`RotatingLog`] (10 MiB × 5 files). `worker logs` reads the file
//! directly, so it works while the worker is down.

// stub: filled by F

use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// Rotate `worker.log` once it reaches this size.
pub const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// How many log files are kept (`worker.log` plus `.1` … `.4`).
pub const LOG_KEEP: u32 = 5;

/// An append-only log file that rotates itself by size.
pub struct RotatingLog {
    // stub: filled by F (path, max_bytes, keep, size, file)
    _stub: (),
}

impl RotatingLog {
    /// Open (or create) the log at `path` for appending.
    pub fn open(_path: &Path, _max_bytes: u64, _keep: u32) -> std::io::Result<Self> {
        // stub: filled by F
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the worker log is not implemented yet",
        ))
    }

    /// Append one line, rotating first if it would pass the size limit.
    /// Best effort: a failed write is dropped, never a panic.
    pub fn write_line(&mut self, _line: &str) {
        // stub: filled by F
    }
}

/// Write one worker log line to stderr, best effort (never `eprintln!`, which
/// panics once the supervisor reading the pipe is gone).
pub fn log_line(level: &str, msg: &str) {
    // stub: filled by F (timestamp and pid prefix)
    let _ = writeln!(std::io::stderr(), "worker {level} {msg}");
}

/// The last `n` lines of the log at `path`, reaching into the previous
/// rotation when the current file is short.
pub fn tail(_path: &Path, _n: usize) -> std::io::Result<Vec<String>> {
    // stub: filled by F
    Ok(Vec::new())
}

/// Copy new lines of the log at `path` to `out` until `stop` is set,
/// following rotations.
pub fn follow(_path: &Path, _out: &mut dyn Write, _stop: &AtomicBool) -> std::io::Result<()> {
    // stub: filled by F
    Ok(())
}
