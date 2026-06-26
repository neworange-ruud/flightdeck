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

    fn create_dir_all(&self, p: &Path) -> Result<()> {
        fs::create_dir_all(p).map_err(|e| FlightDeckError::Io(format!("{}: {e}", p.display())))
    }

    fn read_to_string(&self, p: &Path) -> Result<String> {
        fs::read_to_string(p).map_err(|e| FlightDeckError::Io(format!("{}: {e}", p.display())))
    }

    fn write(&self, p: &Path, contents: &str) -> Result<()> {
        fs::write(p, contents).map_err(|e| FlightDeckError::Io(format!("{}: {e}", p.display())))
    }

    fn append_line(&self, p: &Path, line: &str) -> Result<()> {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .map_err(|e| FlightDeckError::Io(format!("{}: {e}", p.display())))?;
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
