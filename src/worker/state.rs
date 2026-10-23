//! The supervisor's last known state, `~/.flightdeck/worker/supervisor.json`
//! (SPECS §33).
//!
//! Written by the supervisor on every transition and read by `worker status`,
//! which has to explain a worker that is *not* running: why it last exited,
//! whether the supervisor gave up and why. Every timestamp is unix seconds.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The `schema` this build writes into `supervisor.json`.
pub const SUPERVISOR_STATE_SCHEMA: u32 = 1;

/// What the supervisor is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorPhase {
    /// A worker was spawned and has not answered on the control socket yet.
    Starting,
    /// The worker is up.
    Running,
    /// The worker crashed; the supervisor waits until `backoff_until`.
    Backoff,
    /// Too many crashes (or a fatal exit): the supervisor stopped restarting.
    GaveUp,
    /// `worker stop` is in progress.
    Stopping,
    /// Stopped on request.
    Stopped,
}

/// How a worker process ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitReport {
    /// When it exited (unix seconds).
    pub at: u64,
    /// Its exit code, when it exited rather than being killed.
    pub code: Option<i32>,
    /// The signal that killed it, when one did.
    pub signal: Option<i32>,
    /// One line for a person: "exited with code 78 (invalid configuration)".
    pub description: String,
}

/// The contents of `supervisor.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorState {
    /// [`SUPERVISOR_STATE_SCHEMA`] when written by this build.
    pub schema: u32,
    pub supervisor_pid: u32,
    /// When the supervisor started (unix seconds).
    pub started_at: u64,
    pub phase: SupervisorPhase,
    /// The current worker's pid, while one runs.
    pub worker_pid: Option<u32>,
    /// When the current worker was spawned (unix seconds).
    pub worker_started_at: Option<u64>,
    /// Crash restarts since the supervisor started.
    pub restarts: u32,
    pub last_exit: Option<ExitReport>,
    /// Why the supervisor gave up, in [`SupervisorPhase::GaveUp`].
    pub gave_up_reason: Option<String>,
    /// When the next restart is due, in [`SupervisorPhase::Backoff`] (unix
    /// seconds).
    pub backoff_until: Option<u64>,
}

/// Read `supervisor.json`. `None` when it is missing or unreadable: a state
/// file that cannot be parsed says nothing trustworthy.
pub fn read(path: &Path) -> Option<SupervisorState> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write `supervisor.json` atomically: to a sibling temp file, then renamed
/// over the original, so a concurrent `worker status` never reads half a file.
pub fn write_atomic(path: &Path, s: &SupervisorState) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(s).map_err(std::io::Error::other)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SupervisorState {
        SupervisorState {
            schema: SUPERVISOR_STATE_SCHEMA,
            supervisor_pid: 41,
            started_at: 1_700_000_000,
            phase: SupervisorPhase::Backoff,
            worker_pid: None,
            worker_started_at: Some(1_700_000_010),
            restarts: 2,
            last_exit: Some(ExitReport {
                at: 1_700_000_020,
                code: None,
                signal: Some(9),
                description: "killed by signal 9".to_string(),
            }),
            gave_up_reason: None,
            backoff_until: Some(1_700_000_024),
        }
    }

    #[test]
    fn write_atomic_round_trips_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("supervisor.json");
        write_atomic(&path, &sample()).unwrap();
        assert_eq!(read(&path), Some(sample()));
        // Overwriting replaces the whole file.
        let mut next = sample();
        next.phase = SupervisorPhase::GaveUp;
        next.gave_up_reason = Some("5 crashes within 10 minutes".to_string());
        write_atomic(&path, &next).unwrap();
        assert_eq!(read(&path), Some(next));
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("supervisor.json")]);
    }

    #[test]
    fn missing_or_garbled_state_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("supervisor.json");
        assert_eq!(read(&path), None);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(read(&path), None);
    }

    #[test]
    fn phases_serialize_snake_case() {
        let json = serde_json::to_string(&SupervisorPhase::GaveUp).unwrap();
        assert_eq!(json, "\"gave_up\"");
    }
}
