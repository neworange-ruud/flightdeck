//! A worker updating its own FlightDeck at a Desktop's request
//! (`specs/WORKER_PRD.md` §9, W20; protocol v14's
//! [`worker_command::UPDATE_WORKER`](crate::web::protocol::worker_command::UPDATE_WORKER)).
//!
//! ```text
//! update_worker ──► UpdateJobs::start      a thread runs WorkerUpdater::update
//!                        │                 (the network, maybe a minute)
//!   host tick ◄── poll ──┘
//!     Updated   ──► "restarting" message, shutdown intent = restarting,
//!                   WorkerUpdater::restart (a process that outlives this one
//!                   stops this worker and starts the new binary; agents resume)
//!     UpToDate  ──► "already the newest" message
//!     Err       ──► the reason, as an error message
//! ```
//!
//! Both side effects — replacing the binary and restarting — sit behind
//! [`WorkerUpdater`], so the host's half is tested with a fake; the real one is
//! [`crate::worker::self_update::RealWorkerUpdater`].

use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

/// What an update did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// A newer FlightDeck, `version`, replaced this binary; a restart runs it.
    Updated { version: String },
    /// This binary is already the newest release.
    UpToDate,
}

/// Replacing this worker's FlightDeck, and restarting into it.
pub trait WorkerUpdater: Send + Sync {
    /// Update FlightDeck in place the way it was installed. Blocking (the
    /// network, a package manager): called on a thread of its own. The error
    /// is written for the user.
    fn update(&self) -> Result<UpdateOutcome, String>;

    /// Hand `flightdeck worker restart` to a process that outlives this
    /// worker. Returns once it is started, not once it is done.
    fn restart(&self) -> Result<(), String>;
}

/// An updater for builds and tests where a worker must not touch its binary.
pub struct NoUpdater;

impl WorkerUpdater for NoUpdater {
    fn update(&self) -> Result<UpdateOutcome, String> {
        Err("this FlightDeck cannot update itself".to_string())
    }

    fn restart(&self) -> Result<(), String> {
        Err("this FlightDeck cannot restart itself".to_string())
    }
}

/// The one update that may run at a time.
#[derive(Default)]
pub struct UpdateJob {
    running: Option<Receiver<Result<UpdateOutcome, String>>>,
}

impl UpdateJob {
    /// Whether an update is running now.
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Start `updater.update()` on a thread. `false` when one is already
    /// running (nothing is started).
    pub fn start(&mut self, updater: &Arc<dyn WorkerUpdater>) -> bool {
        if self.running.is_some() {
            return false;
        }
        let (tx, rx) = channel();
        let updater = Arc::clone(updater);
        std::thread::Builder::new()
            .name("flightdeck-worker-update".to_string())
            .spawn(move || {
                let _ = tx.send(updater.update());
            })
            .map(|_| self.running = Some(rx))
            .is_ok()
    }

    /// The outcome, once the update has finished (then the job is free).
    pub fn poll(&mut self) -> Option<Result<UpdateOutcome, String>> {
        let rx = self.running.as_ref()?;
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("the update stopped unexpectedly".to_string()),
        };
        self.running = None;
        Some(outcome)
    }
}

/// The host message for a finished update, and whether to restart now.
/// Pure, so the words are tested without a worker.
pub fn report(outcome: &Result<UpdateOutcome, String>) -> (bool, String) {
    match outcome {
        Ok(UpdateOutcome::Updated { version }) => (
            true,
            format!(
                "FlightDeck {version} is installed on this worker. It restarts now; \
                 agents resume when it is back."
            ),
        ),
        Ok(UpdateOutcome::UpToDate) => (
            false,
            format!(
                "This worker already runs the newest FlightDeck ({}).",
                crate::VERSION
            ),
        ),
        Err(reason) => (
            false,
            format!("Error: could not update this worker: {reason}."),
        ),
    }
}

/// The host message when the update is in but the restart could not start.
pub fn restart_failed(version: &str, reason: &str) -> String {
    format!(
        "Error: FlightDeck {version} is installed, but the worker could not restart \
         itself ({reason}). Run `flightdeck worker restart` in the worker shell."
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::*;

    struct Scripted(Mutex<Option<Result<UpdateOutcome, String>>>);

    impl WorkerUpdater for Scripted {
        fn update(&self) -> Result<UpdateOutcome, String> {
            self.0.lock().unwrap().take().unwrap()
        }
        fn restart(&self) -> Result<(), String> {
            Ok(())
        }
    }

    fn wait(job: &mut UpdateJob) -> Result<UpdateOutcome, String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(outcome) = job.poll() {
                return outcome;
            }
            assert!(Instant::now() < deadline, "the update never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn one_update_runs_at_a_time_and_its_outcome_frees_the_job() {
        let updater: Arc<dyn WorkerUpdater> =
            Arc::new(Scripted(Mutex::new(Some(Ok(UpdateOutcome::Updated {
                version: "9.9.9".into(),
            })))));
        let mut job = UpdateJob::default();
        assert!(job.poll().is_none(), "nothing running");
        assert!(job.start(&updater));
        assert!(job.is_running());
        assert!(!job.start(&updater), "a second is refused while one runs");
        assert_eq!(
            wait(&mut job),
            Ok(UpdateOutcome::Updated {
                version: "9.9.9".into()
            })
        );
        assert!(!job.is_running());
    }

    #[test]
    fn the_report_restarts_only_after_an_update() {
        let (restart, text) = report(&Ok(UpdateOutcome::Updated {
            version: "9.9.9".into(),
        }));
        assert!(restart);
        assert!(
            text.contains("9.9.9") && text.contains("restarts"),
            "{text}"
        );

        let (restart, text) = report(&Ok(UpdateOutcome::UpToDate));
        assert!(!restart);
        assert!(text.contains("already"), "{text}");

        let (restart, text) = report(&Err("no network".into()));
        assert!(!restart);
        assert!(
            text.starts_with("Error:") && text.contains("no network"),
            "{text}"
        );
    }
}
