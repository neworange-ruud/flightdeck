//! Opt-in, file-based diagnostic log for the remote link.
//!
//! FlightDeck is a full-screen TUI, so stdout/stderr are the UI — there is
//! nowhere for ad-hoc `tracing` to land. This module appends one timestamped
//! line per event to the file named by the `FLIGHTDECK_REMOTE_LOG` environment
//! variable, and is a no-op when that variable is unset (every normal run). It
//! exists to diagnose the desktop↔phone delivery path (remote-control-bbf /
//! the desktop→phone routing investigation): who sent what seq, which acks and
//! presence frames came back, and any relay errors — never any ciphertext or
//! plaintext, only routing metadata.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

/// The configured log path, resolved once. `None` disables logging entirely.
fn log_path() -> Option<&'static PathBuf> {
    static PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
    PATH.get_or_init(|| std::env::var_os("FLIGHTDECK_REMOTE_LOG").map(PathBuf::from))
        .as_ref()
}

/// Append a diagnostic line (prefixed with a unix-millis timestamp) when
/// `FLIGHTDECK_REMOTE_LOG` is set; otherwise do nothing. Best-effort: any I/O
/// error is swallowed so diagnostics never affect the app.
pub fn log(line: &str) {
    let Some(path) = log_path() else { return };
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{ms} {line}");
    }
}
