//! The supervisor: keeps one `worker run` alive (SPECS §33).
//!
//! Small on purpose: std threads only, no tokio, no `AppHost`. It restarts a
//! worker that exits abnormally with exponential backoff (1 s doubling to
//! 60 s), gives up after 5 crashes within 10 minutes, and records every
//! transition in `supervisor.json` for `worker status`.

// stub: filled by F

use std::time::Duration;

use crate::contracts::{FlightDeckError, Result};
use crate::worker::paths::WorkerPaths;

/// When to restart and when to give up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestartPolicy {
    /// The first backoff.
    pub initial: Duration,
    /// The backoff never grows past this.
    pub max: Duration,
    /// Give up after this many crashes …
    pub crash_limit: u32,
    /// … within this window.
    pub window: Duration,
    /// A run at least this long resets the backoff to `initial`.
    pub stable_after: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        RestartPolicy {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(60),
            crash_limit: 5,
            window: Duration::from_secs(10 * 60),
            stable_after: Duration::from_secs(60),
        }
    }
}

/// How a worker process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitKind {
    /// Exit code 0.
    Clean,
    /// Any other exit code, or a signal.
    Crashed {
        code: Option<i32>,
        signal: Option<i32>,
    },
    /// An exit code that restarting cannot fix (73: already running; 78:
    /// invalid config).
    Fatal { code: i32, reason: String },
}

/// What the supervisor does next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Stop,
    Restart { after: Duration },
    GiveUp { reason: String },
}

/// Crash history, for backoff and giving up.
pub struct CrashTracker {
    // stub: filled by F (crashes: VecDeque<u64>, next_backoff: Duration)
    _stub: (),
}

impl CrashTracker {
    pub fn new(_policy: &RestartPolicy) -> Self {
        // stub: filled by F
        CrashTracker { _stub: () }
    }

    /// Record an exit at `now_ms` after a run of `ran_for` and decide.
    /// `stopping` is set while `worker stop` is in progress.
    pub fn on_exit(
        &mut self,
        _policy: &RestartPolicy,
        _now_ms: u64,
        _ran_for: Duration,
        _exit: &ExitKind,
        _stopping: bool,
    ) -> Decision {
        // stub: filled by F
        Decision::Stop
    }
}

/// Classify a worker's exit status.
pub fn classify(status: std::process::ExitStatus) -> ExitKind {
    // stub: filled by F (73/78 → Fatal, signals via ExitStatusExt)
    if status.success() {
        ExitKind::Clean
    } else {
        ExitKind::Crashed {
            code: status.code(),
            signal: None,
        }
    }
}

/// Run the supervise loop (`flightdeck worker supervise`) until stopped;
/// returns the process exit code.
pub fn supervise(_paths: &WorkerPaths) -> Result<i32> {
    // stub: filled by F
    Err(FlightDeckError::Other(
        "the worker supervisor is not implemented yet".to_string(),
    ))
}
