//! The worker's lock files, and the local-host lock they give the TUI and the
//! Desktop (SPECS §33, W22).
//!
//! A supervisor holds `supervisor.lock` and a `worker run` holds `run.lock`
//! for their whole lives. The OS releases an advisory lock when its holder
//! exits however it exits, so "is a worker running here?" never trusts a pid
//! file a crash left behind. While either is held, a local TUI or Desktop
//! window refuses to open: two hosts must not drive the same projects.

use std::path::Path;
use std::time::Duration;

use crate::contracts::{FileLock, FileSystem, Result};
use crate::worker::paths::WorkerPaths;

/// Why a local TUI or Desktop window will not open while a worker runs on
/// this machine (W22, `specs/WORKER_PRD.md` §4.2).
pub const LOCAL_HOST_REFUSAL: &str = "A FlightDeck worker is running on this machine and \
serving its projects. Control it from FlightDeck Desktop, or run `flightdeck worker stop`.";

/// Whether a worker (a foreground `worker run`, or a supervisor) is running
/// for the account `paths` belongs to: `run.lock` or `supervisor.lock` is held.
///
/// Probes by taking each lock and dropping it at once. A lock file that does
/// not exist cannot be held, and is not created by asking: a machine that has
/// never run a worker gets no `~/.flightdeck/worker/` from a TUI launch. An
/// error while probing counts as "not running": refusing every launch because
/// a lock file is unreadable would be worse than the race it guards.
pub fn worker_running(fs: &dyn FileSystem, paths: &WorkerPaths) -> bool {
    [&paths.run_lock, &paths.supervisor_lock]
        .into_iter()
        .any(|lock| fs.exists(lock) && matches!(fs.try_lock_exclusive(lock), Ok(None)))
}

/// Refuse to open a local host (the TUI; the Desktop calls
/// [`worker_running`] itself) while a worker runs for this account:
/// `Err(Other(LOCAL_HOST_REFUSAL))`. Always `Ok` off unix, where no worker
/// can run.
pub fn refuse_local_host() -> Result<()> {
    #[cfg(unix)]
    {
        if let Some(paths) = WorkerPaths::current() {
            if worker_running(&crate::contracts::RealFs, &paths) {
                return Err(crate::contracts::FlightDeckError::Other(
                    LOCAL_HOST_REFUSAL.to_string(),
                ));
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Ok(())
    }
}

/// Take `lock` for as long as the returned guard lives. `Ok(None)` means
/// another process holds it (a second supervisor, a second `worker run`).
pub fn acquire(fs: &dyn FileSystem, lock: &Path) -> Result<Option<FileLock>> {
    fs.try_lock_exclusive(lock)
}

/// How many times [`acquire_patiently`] tries before concluding the lock is
/// held for good.
pub const ACQUIRE_ATTEMPTS: u32 = 20;

/// The pause between two of [`acquire_patiently`]'s attempts: with
/// [`ACQUIRE_ATTEMPTS`], about half a second in all.
pub const ACQUIRE_PAUSE: Duration = Duration::from_millis(25);

/// [`acquire`] for a process starting up (`worker run`, the supervisor),
/// which must not mistake a probe for another worker.
///
/// [`worker_running`] and `worker status` find out whether a lock is held by
/// taking it for a moment. A worker starting in that moment would find its
/// lock taken and exit as "already running" (73) — a `worker status` run by a
/// script, or a TUI launched on the box, would stop it starting. A probe lets
/// go at once, so `pause` is called between up to [`ACQUIRE_ATTEMPTS`] tries;
/// `Ok(None)` only once every try found it held.
pub fn acquire_patiently(
    fs: &dyn FileSystem,
    lock: &Path,
    pause: &mut dyn FnMut(),
) -> Result<Option<FileLock>> {
    for _ in 1..ACQUIRE_ATTEMPTS {
        if let Some(guard) = acquire(fs, lock)? {
            return Ok(Some(guard));
        }
        pause();
    }
    acquire(fs, lock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::FlightDeckError;
    use crate::testing::FakeFs;

    fn paths() -> WorkerPaths {
        WorkerPaths::from_home(Path::new("/home/u"))
    }

    #[test]
    fn no_lock_files_means_not_running_and_creates_nothing() {
        let fs = FakeFs::new();
        assert!(!worker_running(&fs, &paths()));
        assert!(fs.writes().is_empty(), "probing wrote {:?}", fs.writes());
        assert!(!fs.exists(&paths().run_lock));
    }

    #[test]
    fn free_lock_files_mean_not_running() {
        let fs = FakeFs::new();
        let p = paths();
        // Files left behind by a worker that has exited: present, unheld.
        drop(acquire(&fs, &p.run_lock).unwrap());
        drop(acquire(&fs, &p.supervisor_lock).unwrap());
        assert!(!worker_running(&fs, &p));
        // The probe released what it took.
        assert!(acquire(&fs, &p.run_lock).unwrap().is_some());
    }

    #[test]
    fn a_held_run_lock_means_running() {
        let fs = FakeFs::new();
        let p = paths();
        let _held = acquire(&fs, &p.run_lock).unwrap().expect("free");
        assert!(worker_running(&fs, &p));
    }

    #[test]
    fn a_held_supervisor_lock_means_running() {
        let fs = FakeFs::new();
        let p = paths();
        let _held = acquire(&fs, &p.supervisor_lock).unwrap().expect("free");
        assert!(worker_running(&fs, &p));
    }

    #[test]
    fn acquire_refuses_a_second_holder() {
        let fs = FakeFs::new();
        let p = paths();
        let first = acquire(&fs, &p.run_lock).unwrap();
        assert!(first.is_some());
        assert!(acquire(&fs, &p.run_lock).unwrap().is_none());
        drop(first);
        assert!(acquire(&fs, &p.run_lock).unwrap().is_some());
    }

    #[test]
    fn a_patient_acquire_outwaits_a_probe() {
        let fs = FakeFs::new();
        let p = paths();
        // A probe holds the lock when the worker first tries, and lets go
        // during the pause.
        let mut probe = acquire(&fs, &p.run_lock).unwrap();
        assert!(probe.is_some());
        let mut pauses = 0;
        let guard = acquire_patiently(&fs, &p.run_lock, &mut || {
            pauses += 1;
            probe = None;
        })
        .unwrap();
        assert!(guard.is_some(), "taken once the probe let go");
        assert_eq!(pauses, 1);
        assert!(probe.is_none());
    }

    #[test]
    fn a_patient_acquire_gives_up_on_a_real_holder() {
        let fs = FakeFs::new();
        let p = paths();
        let _held = acquire(&fs, &p.run_lock).unwrap().expect("free");
        let mut pauses = 0;
        let guard = acquire_patiently(&fs, &p.run_lock, &mut || pauses += 1).unwrap();
        assert!(guard.is_none(), "another worker holds it");
        assert_eq!(pauses, ACQUIRE_ATTEMPTS - 1, "every attempt was made");
    }

    #[test]
    fn local_host_refusal_text_is_pinned() {
        assert_eq!(
            LOCAL_HOST_REFUSAL,
            "A FlightDeck worker is running on this machine and serving its projects. \
             Control it from FlightDeck Desktop, or run `flightdeck worker stop`."
        );
        assert_eq!(
            FlightDeckError::Other(LOCAL_HOST_REFUSAL.to_string()).to_string(),
            LOCAL_HOST_REFUSAL,
            "shown verbatim, with no error-kind prefix"
        );
    }
}
