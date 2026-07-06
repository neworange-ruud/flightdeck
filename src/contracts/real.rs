//! Real (production) implementations of the small, trivial seam traits:
//! [`RealFs`] over `std::fs` and [`RealClock`] over the system clock.
//!
//! The non-trivial real implementations live with their owning modules:
//! `GitExecutor` → [`crate::git`], `PtyBackend` → [`crate::terminal`].

use crate::contracts::error::{FlightDeckError, Result};
use crate::contracts::traits::{Clock, FileSystem};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// `std::fs`-backed [`FileSystem`].
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFs;

impl FileSystem for RealFs {
    fn exists(&self, p: &Path) -> bool {
        p.exists()
    }

    fn is_dir(&self, p: &Path) -> bool {
        p.is_dir()
    }

    fn create_dir_all(&self, p: &Path) -> Result<()> {
        fs::create_dir_all(p).map_err(|e| FlightDeckError::Io(format!("{}: {e}", p.display())))
    }

    fn read_to_string(&self, p: &Path) -> Result<String> {
        fs::read_to_string(p).map_err(|e| FlightDeckError::Io(format!("{}: {e}", p.display())))
    }

    fn write(&self, p: &Path, contents: &str) -> Result<()> {
        // Atomic full-file write: stage into a sibling temp file, then rename it
        // over the destination. `rename` is atomic on POSIX and Windows, so a
        // crash/kill during the write leaves the destination either fully intact
        // (old) or fully replaced (new) — never truncated. This protects
        // `state.json` from corruption on an abrupt shutdown mid-write.
        let tmp = atomic_temp_path(p);
        let staged = fs::write(&tmp, contents)
            .map_err(|e| FlightDeckError::Io(format!("{}: {e}", tmp.display())));
        if let Err(e) = staged {
            // Best-effort cleanup of a partial temp; leave the destination alone.
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        fs::rename(&tmp, p).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            FlightDeckError::Io(format!("{}: {e}", p.display()))
        })
    }

    fn symlink(&self, target: &Path, link: &Path) -> Result<()> {
        #[cfg(unix)]
        let r = std::os::unix::fs::symlink(target, link);
        #[cfg(windows)]
        let r = std::os::windows::fs::symlink_file(target, link);
        r.map_err(|e| FlightDeckError::Io(format!("{}: {e}", link.display())))
    }

    fn append_line(&self, p: &Path, line: &str) -> Result<()> {
        // If the file already has content that doesn't end in a newline, add
        // one first so the new line doesn't get glued onto the last existing
        // line (a real-world state for files saved without a final newline).
        let needs_leading_newline = match fs::read(p) {
            Ok(bytes) => !bytes.is_empty() && bytes.last() != Some(&b'\n'),
            Err(_) => false,
        };
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .map_err(|e| FlightDeckError::Io(format!("{}: {e}", p.display())))?;
        if needs_leading_newline {
            f.write_all(b"\n")
                .map_err(|e| FlightDeckError::Io(format!("{}: {e}", p.display())))?;
        }
        writeln!(f, "{line}").map_err(|e| FlightDeckError::Io(format!("{}: {e}", p.display())))
    }

    fn list_dir(&self, p: &Path) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        let rd =
            fs::read_dir(p).map_err(|e| FlightDeckError::Io(format!("{}: {e}", p.display())))?;
        for entry in rd {
            let entry = entry.map_err(|e| FlightDeckError::Io(e.to_string()))?;
            out.push(entry.path());
        }
        out.sort();
        Ok(out)
    }

    fn remove_dir_all(&self, p: &Path) -> Result<()> {
        remove_dir_all_resilient(p)
    }
}

/// Remove a directory tree. On Windows a directory cannot be deleted while any
/// file in it is still open, and a just-killed process releases its handles
/// asynchronously — so retry briefly to absorb that teardown window instead of
/// failing with a transient permission-denied error. On other platforms a
/// single attempt is sufficient.
#[cfg(windows)]
fn remove_dir_all_resilient(p: &Path) -> Result<()> {
    use std::io::ErrorKind;
    use std::thread::sleep;
    use std::time::Duration;

    let mut last: Option<std::io::Error> = None;
    for _ in 0..10 {
        match fs::remove_dir_all(p) {
            Ok(()) => return Ok(()),
            // Already gone (possibly removed by an earlier partial attempt).
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                last = Some(e);
                sleep(Duration::from_millis(100));
            }
        }
    }
    Err(FlightDeckError::Io(format!(
        "{}: {}",
        p.display(),
        last.map(|e| e.to_string()).unwrap_or_default()
    )))
}

#[cfg(not(windows))]
fn remove_dir_all_resilient(p: &Path) -> Result<()> {
    fs::remove_dir_all(p).map_err(|e| FlightDeckError::Io(format!("{}: {e}", p.display())))
}

/// The sibling temp path used for an atomic write of `p`: same directory (so the
/// final `rename` stays on one filesystem and is atomic), hidden, distinctive.
fn atomic_temp_path(p: &Path) -> PathBuf {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "tmp".to_string());
    p.with_file_name(format!(".{name}.fdtmp"))
}

/// System-clock-backed [`Clock`] producing UTC ISO-8601 timestamps.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealClock;

impl Clock for RealClock {
    fn now_iso8601(&self) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        format_iso8601_utc(now.as_secs())
    }

    fn now_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    fn now_unix_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

/// Format a Unix timestamp (seconds) as a UTC ISO-8601 string.
///
/// Uses Howard Hinnant's `civil_from_days` algorithm to avoid a date-library
/// dependency.
fn format_iso8601_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // civil_from_days (days since 1970-01-01)
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_is_atomic_and_preserves_destination_when_temp_step_fails() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("state.json");
        let fs = RealFs;
        fs.write(&dest, "OLD").unwrap();

        // Sabotage the temp step: occupy the temp path with a directory so the
        // temp write cannot succeed. An atomic write must fail without touching
        // the existing destination; a naive truncate-in-place write would lose
        // "OLD".
        std::fs::create_dir(atomic_temp_path(&dest)).unwrap();

        let result = fs.write(&dest, "NEW");
        assert!(
            result.is_err(),
            "write should fail when the temp step fails"
        );
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            "OLD",
            "destination must be preserved when the write fails"
        );
    }

    #[test]
    fn write_replaces_content_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("state.json");
        let fs = RealFs;
        fs.write(&dest, "first").unwrap();
        fs.write(&dest, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "second");
        assert!(
            !atomic_temp_path(&dest).exists(),
            "no temp file should remain after a successful write"
        );
    }

    #[test]
    fn formats_unix_epoch() {
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn formats_known_timestamp() {
        // 2021-01-01T00:00:00Z == 1_609_459_200
        assert_eq!(format_iso8601_utc(1_609_459_200), "2021-01-01T00:00:00Z");
        // 2023-06-15T13:45:30Z == 1_686_836_730
        assert_eq!(format_iso8601_utc(1_686_836_730), "2023-06-15T13:45:30Z");
    }
}
