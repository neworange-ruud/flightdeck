//! Shutdown-signal handling.
//!
//! FlightDeck registers no signal handlers by default, so an external
//! `SIGTERM`/`SIGINT` (terminal closed, `kill <pid>`, service stop) would kill
//! the process bare — skipping the teardown that persists `state.json` and
//! terminates child agents. Here we register a handler that only flips a shared
//! flag; the event loop polls it and breaks, letting the normal clean-teardown
//! path run.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Install handlers so `SIGTERM`/`SIGINT` set the returned flag (best-effort).
/// The event loop checks this flag each iteration and exits cleanly when set.
#[cfg(unix)]
pub fn install_shutdown_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ] {
        // Best-effort: a failed registration just means no graceful shutdown for
        // that particular signal.
        let _ = signal_hook::flag::register(sig, Arc::clone(&flag));
    }
    flag
}

/// Non-Unix stub: no POSIX signals to trap; the flag is simply never set.
#[cfg(not(unix))]
pub fn install_shutdown_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn sigterm_sets_the_returned_shutdown_flag() {
        let flag = install_shutdown_flag();
        assert!(!flag.load(Ordering::Relaxed), "flag starts clear");

        // `raise` delivers synchronously on the calling thread (POSIX): the
        // handler has run by the time it returns. A handler is installed, so
        // this does not terminate the test process.
        signal_hook::low_level::raise(signal_hook::consts::SIGTERM).unwrap();

        assert!(
            flag.load(Ordering::Relaxed),
            "SIGTERM must set the returned shutdown flag"
        );
    }
}
