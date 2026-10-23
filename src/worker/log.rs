//! The worker log, `~/.flightdeck/logs/worker.log` (SPECS §33).
//!
//! The worker writes lines to its stderr; the supervisor appends them, and its
//! own, to a [`RotatingLog`] (10 MiB × 5 files). `worker logs` reads the file
//! directly, so it works while the worker is down.
//!
//! Every write here is best effort. Rust ignores `SIGPIPE`, so writing to a
//! pipe whose reader has gone returns an error rather than killing the
//! process; `eprintln!` would turn that error into a panic, so nothing here
//! uses it.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::contracts::{Clock, RealClock};

/// Rotate `worker.log` once it reaches this size.
pub const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
/// How many log files are kept (`worker.log` plus `.1` … `.4`).
pub const LOG_KEEP: u32 = 5;

/// How often [`follow`] looks for new lines.
const FOLLOW_POLL: Duration = Duration::from_millis(250);

/// An append-only log file that rotates itself by size.
pub struct RotatingLog {
    path: PathBuf,
    max_bytes: u64,
    /// Files kept, the live one included (at least 1).
    keep: u32,
    /// Bytes in the live file.
    size: u64,
    file: File,
}

impl RotatingLog {
    /// Open (or create) the log at `path` for appending, creating its
    /// directory if needed. A file that already exists is appended to, and
    /// counts toward `max_bytes`.
    pub fn open(path: &Path, max_bytes: u64, keep: u32) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = open_append(path)?;
        let size = file.metadata()?.len();
        Ok(RotatingLog {
            path: path.to_path_buf(),
            max_bytes,
            keep: keep.max(1),
            size,
            file,
        })
    }

    /// Append one line, rotating first if it would pass the size limit.
    /// Best effort: a failed write is dropped, never a panic. A line longer
    /// than the limit still goes in whole, alone in a fresh file.
    pub fn write_line(&mut self, line: &str) {
        let line = line.trim_end_matches(['\n', '\r']);
        let len = line.len() as u64 + 1;
        if self.size > 0 && self.size + len > self.max_bytes {
            self.rotate();
        }
        let mut bytes = Vec::with_capacity(line.len() + 1);
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
        if self.file.write_all(&bytes).is_ok() {
            self.size += len;
        }
    }

    /// Shift `log.N` to `log.N+1` (dropping the oldest), the live file to
    /// `log.1`, and start a new live file. When a step fails the log keeps
    /// appending to whatever file it has open: losing rotation beats losing
    /// lines.
    fn rotate(&mut self) {
        if self.keep > 1 {
            let _ = std::fs::remove_file(rotated_path(&self.path, self.keep - 1));
            for n in (1..self.keep - 1).rev() {
                let from = rotated_path(&self.path, n);
                if from.exists() {
                    let _ = std::fs::rename(&from, rotated_path(&self.path, n + 1));
                }
            }
            if std::fs::rename(&self.path, rotated_path(&self.path, 1)).is_err() {
                return;
            }
        } else if std::fs::remove_file(&self.path).is_err() {
            return;
        }
        if let Ok(file) = open_append(&self.path) {
            self.file = file;
            self.size = 0;
        }
    }
}

fn open_append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// `worker.log` → `worker.log.N`.
pub fn rotated_path(path: &Path, n: u32) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// One log line, without its newline: `<iso8601> <who> <level> <msg>`, where
/// `who` is `worker[pid]` or `supervisor[pid]`.
pub fn format_line(at: &str, who: &str, level: &str, msg: &str) -> String {
    format!("{at} {who} {level} {msg}")
}

/// Write one worker log line to stderr, best effort (never `eprintln!`, which
/// panics once the supervisor reading the pipe is gone).
pub fn log_line(level: &str, msg: &str) {
    let who = format!("worker[{}]", std::process::id());
    let mut line = format_line(&RealClock.now_iso8601(), &who, level, msg);
    line.push('\n');
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}

/// The last `n` lines of the log at `path`, reaching into the previous
/// rotation (`.1`) when the current file is short. `NotFound` when there is
/// no log at all.
pub fn tail(path: &Path, n: usize) -> std::io::Result<Vec<String>> {
    let current = match read_lines(path) {
        Ok(lines) => lines,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Between a rotation's rename and the new file's creation only
            // `.1` exists; with neither there is no log.
            if !rotated_path(path, 1).exists() {
                return Err(e);
            }
            Vec::new()
        }
        Err(e) => return Err(e),
    };
    if current.len() >= n {
        return Ok(current[current.len() - n..].to_vec());
    }
    let wanted = n - current.len();
    let mut lines = read_lines(&rotated_path(path, 1)).unwrap_or_default();
    let skip = lines.len().saturating_sub(wanted);
    lines.drain(..skip);
    lines.extend(current);
    Ok(lines)
}

/// Every line of a file, invalid UTF-8 replaced rather than refused.
fn read_lines(path: &Path) -> std::io::Result<Vec<String>> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(str::to_string)
        .collect())
}

/// Copy new lines of the log at `path` to `out` until `stop` is set,
/// following rotations. Starts at the end of the file; a log that does not
/// exist yet is waited for. Returns the first error writing to `out` (a
/// closed pipe ends the follow).
pub fn follow(path: &Path, out: &mut dyn Write, stop: &AtomicBool) -> std::io::Result<()> {
    follow_with(path, out, stop, FOLLOW_POLL, false)
}

/// [`follow`] with an explicit poll interval, optionally from the start of
/// the current file rather than its end.
pub(crate) fn follow_with(
    path: &Path,
    out: &mut dyn Write,
    stop: &AtomicBool,
    poll: Duration,
    from_start: bool,
) -> std::io::Result<()> {
    // (the open file, its identity, how far it has been copied)
    let mut current: Option<(File, Option<u64>, u64)> = None;
    let mut first_open = true;
    while !stop.load(Ordering::Relaxed) {
        if current.is_none() {
            if let Ok(file) = File::open(path) {
                let id = file_id(&file);
                let pos = if first_open && !from_start {
                    file.metadata().map(|m| m.len()).unwrap_or(0)
                } else {
                    0
                };
                current = Some((file, id, pos));
            }
            first_open = false;
        }
        if let Some((file, id, pos)) = current.as_mut() {
            *pos += copy_new(file, *pos, out)?;
            // Rotated: the path now names a different file, or the same one
            // cut short. What the old file still held was copied just above.
            let rotated = match std::fs::metadata(path) {
                Ok(meta) => meta.len() < *pos || (id.is_some() && meta_id(&meta) != *id),
                Err(_) => true,
            };
            if rotated {
                current = None;
                continue;
            }
        }
        std::thread::sleep(poll);
    }
    Ok(())
}

/// Copy whatever `file` holds past `pos` to `out`; the bytes copied.
fn copy_new(file: &mut File, pos: u64, out: &mut dyn Write) -> std::io::Result<u64> {
    if file.seek(SeekFrom::Start(pos)).is_err() {
        return Ok(0);
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() || buf.is_empty() {
        return Ok(0);
    }
    out.write_all(&buf)?;
    out.flush()?;
    Ok(buf.len() as u64)
}

#[cfg(unix)]
fn meta_id(meta: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.ino())
}

/// No inode to compare off unix: a rotation shows as the file getting
/// shorter.
#[cfg(not(unix))]
fn meta_id(_meta: &std::fs::Metadata) -> Option<u64> {
    None
}

fn file_id(file: &File) -> Option<u64> {
    file.metadata().ok().and_then(|m| meta_id(&m))
}

#[cfg(test)]
mod tests;
