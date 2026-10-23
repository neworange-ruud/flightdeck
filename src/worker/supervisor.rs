//! The supervisor: keeps one `worker run` alive (SPECS §33).
//!
//! Small on purpose: std threads only, no tokio, no `AppHost`. It restarts a
//! worker that exits abnormally with exponential backoff (1 s doubling to
//! 60 s), gives up after 5 crashes within 10 minutes, and records every
//! transition in `supervisor.json` for `worker status`. The worker's stdout
//! and stderr, and the supervisor's own lines, go to the rotating
//! `worker.log`.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::contracts::{Clock, RealClock, RealFs, Result};
use crate::worker::daemon;
use crate::worker::lock;
use crate::worker::log::{self, RotatingLog, LOG_KEEP, LOG_MAX_BYTES};
use crate::worker::paths::WorkerPaths;
use crate::worker::run::{EXIT_ALREADY_RUNNING, EXIT_CONFIG_INVALID};
use crate::worker::state::{
    self, ExitReport, SupervisorPhase, SupervisorState, SUPERVISOR_STATE_SCHEMA,
};

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
    /// When each recent crash happened (ms on the caller's clock), oldest
    /// first; only those inside the policy's window are kept.
    crashes: VecDeque<u64>,
    /// The backoff the next crash gets.
    next_backoff: Duration,
    /// The previous exit was "already running" (73) and was retried.
    retried_already_running: bool,
}

impl CrashTracker {
    pub fn new(policy: &RestartPolicy) -> Self {
        CrashTracker {
            crashes: VecDeque::new(),
            next_backoff: policy.initial,
            retried_already_running: false,
        }
    }

    /// Record an exit at `now_ms` after a run of `ran_for` and decide.
    /// `stopping` is set while `worker stop` is in progress.
    ///
    /// * Anything while stopping → [`Decision::Stop`].
    /// * [`ExitKind::Fatal`] → give up at once, except that a first "already
    ///   running" (73) is retried: a TUI probing `run.lock` at the instant the
    ///   worker starts can hold it for a moment.
    /// * Otherwise (a crash, or a clean exit nobody asked for) → restart after
    ///   the current backoff, which then doubles up to the policy's maximum; a
    ///   run of at least `stable_after` first resets it. The `crash_limit`-th
    ///   crash inside `window` gives up.
    pub fn on_exit(
        &mut self,
        policy: &RestartPolicy,
        now_ms: u64,
        ran_for: Duration,
        exit: &ExitKind,
        stopping: bool,
    ) -> Decision {
        if stopping {
            return Decision::Stop;
        }
        let retried = std::mem::take(&mut self.retried_already_running);
        if let ExitKind::Fatal { code, reason } = exit {
            if *code == EXIT_ALREADY_RUNNING && !retried {
                self.retried_already_running = true;
                return Decision::Restart {
                    after: policy.initial,
                };
            }
            return Decision::GiveUp {
                reason: reason.clone(),
            };
        }
        if ran_for >= policy.stable_after {
            self.next_backoff = policy.initial;
        }
        let window_ms = policy.window.as_millis() as u64;
        self.crashes.push_back(now_ms);
        while self
            .crashes
            .front()
            .is_some_and(|&at| now_ms.saturating_sub(at) >= window_ms)
        {
            self.crashes.pop_front();
        }
        if self.crashes.len() as u32 >= policy.crash_limit.max(1) {
            return Decision::GiveUp {
                reason: format!(
                    "{} crashes within {}",
                    policy.crash_limit,
                    describe_duration(policy.window)
                ),
            };
        }
        let after = self.next_backoff;
        self.next_backoff = (self.next_backoff * 2).min(policy.max);
        Decision::Restart { after }
    }
}

/// "10 minutes", "1 minute", "90 seconds".
fn describe_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit}")
        } else {
            format!("{n} {unit}s")
        }
    };
    if secs >= 60 && secs.is_multiple_of(60) {
        plural(secs / 60, "minute")
    } else {
        plural(secs, "second")
    }
}

/// Classify a worker's exit status.
pub fn classify(status: std::process::ExitStatus) -> ExitKind {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return ExitKind::Crashed {
                code: None,
                signal: Some(signal),
            };
        }
    }
    match status.code() {
        Some(0) => ExitKind::Clean,
        Some(EXIT_ALREADY_RUNNING) => ExitKind::Fatal {
            code: EXIT_ALREADY_RUNNING,
            reason: "another worker is already running for this account".to_string(),
        },
        Some(EXIT_CONFIG_INVALID) => ExitKind::Fatal {
            code: EXIT_CONFIG_INVALID,
            reason: "the global config is invalid (see `flightdeck worker logs`)".to_string(),
        },
        code => ExitKind::Crashed { code, signal: None },
    }
}

/// One line for a person: "exited with code 101 (panic)", "killed by
/// signal 9".
pub fn describe(exit: &ExitKind) -> String {
    match exit {
        ExitKind::Clean => "exited with code 0".to_string(),
        ExitKind::Fatal { code, .. } if *code == EXIT_ALREADY_RUNNING => {
            format!("exited with code {code} (another worker is running)")
        }
        ExitKind::Fatal { code, .. } if *code == EXIT_CONFIG_INVALID => {
            format!("exited with code {code} (invalid configuration)")
        }
        ExitKind::Fatal { code, .. } => format!("exited with code {code}"),
        ExitKind::Crashed {
            signal: Some(signal),
            ..
        } => format!("killed by signal {signal}"),
        ExitKind::Crashed {
            code: Some(101), ..
        } => "exited with code 101 (panic)".to_string(),
        ExitKind::Crashed {
            code: Some(code), ..
        } => format!("exited with code {code}"),
        ExitKind::Crashed { .. } => "exited without a status".to_string(),
    }
}

fn exit_report(exit: &ExitKind, at: u64, description: String) -> ExitReport {
    let (code, signal) = match exit {
        ExitKind::Clean => (Some(0), None),
        ExitKind::Fatal { code, .. } => (Some(*code), None),
        ExitKind::Crashed { code, signal } => (*code, *signal),
    };
    ExitReport {
        at,
        code,
        signal,
        description,
    }
}

/// The supervise loop's knobs; [`supervise`] uses the real ones, tests
/// shrink them.
struct Tuning {
    policy: RestartPolicy,
    /// How long a worker gets to exit after `SIGTERM` before `SIGKILL`.
    stop_grace: Duration,
    /// How often the loop looks at the child and the stop flag.
    poll: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            policy: RestartPolicy::default(),
            stop_grace: Duration::from_secs(30),
            poll: Duration::from_millis(100),
        }
    }
}

/// How often a starting worker is pinged on its control socket.
const PING_EVERY: Duration = Duration::from_millis(500);

/// Run the supervise loop (`flightdeck worker supervise`) until stopped;
/// returns the process exit code: 0 when stopped on request, 1 when another
/// supervisor holds the lock or this one gave up.
pub fn supervise(paths: &WorkerPaths) -> Result<i32> {
    paths.ensure_dirs()?;
    let stop = crate::signals::install_shutdown_flag();
    // Resolved once: after an in-place update Linux reports the running
    // binary as "… (deleted)", and the path is what the next worker runs.
    let exe = std::env::current_exe()?;
    supervise_with(paths, &exe, &["worker", "run"], &Tuning::default(), &stop)
}

/// [`supervise`] over an explicit program, knobs and stop flag.
fn supervise_with(
    paths: &WorkerPaths,
    exe: &Path,
    args: &[&str],
    tuning: &Tuning,
    stop: &AtomicBool,
) -> Result<i32> {
    let log = Arc::new(Mutex::new(RotatingLog::open(
        &paths.log_file,
        LOG_MAX_BYTES,
        LOG_KEEP,
    )?));
    let mut sup = Supervisor::new(paths, exe.to_path_buf(), log);
    // Patiently, as `worker run` takes its own: a probe of this lock (`worker
    // status`, a TUI launch) is not another supervisor.
    let Some(_lock) = lock::acquire_patiently(&RealFs, &paths.supervisor_lock, &mut || {
        std::thread::sleep(lock::ACQUIRE_PAUSE)
    })?
    else {
        sup.say("error", "another supervisor is already running; exiting");
        return Ok(1);
    };
    let _ = std::fs::write(
        &paths.supervisor_pid,
        format!("{}\n", sup.state.supervisor_pid),
    );
    sup.say("info", &format!("started (FlightDeck {})", crate::VERSION));
    let code = sup.run(args, tuning, stop);
    let _ = std::fs::remove_file(&paths.supervisor_pid);
    Ok(code)
}

/// The supervise loop's state.
struct Supervisor<'a> {
    paths: &'a WorkerPaths,
    exe: PathBuf,
    log: Arc<Mutex<RotatingLog>>,
    who: String,
    state: SupervisorState,
    /// The supervisor's monotonic clock, for crash timing.
    epoch: Instant,
}

impl<'a> Supervisor<'a> {
    fn new(paths: &'a WorkerPaths, exe: PathBuf, log: Arc<Mutex<RotatingLog>>) -> Self {
        let pid = std::process::id();
        Supervisor {
            paths,
            exe,
            log,
            who: format!("supervisor[{pid}]"),
            state: SupervisorState {
                schema: SUPERVISOR_STATE_SCHEMA,
                supervisor_pid: pid,
                started_at: RealClock.now_unix_secs(),
                phase: SupervisorPhase::Starting,
                worker_pid: None,
                worker_started_at: None,
                restarts: 0,
                last_exit: None,
                gave_up_reason: None,
                backoff_until: None,
            },
            epoch: Instant::now(),
        }
    }

    /// Append one of the supervisor's own lines to the log.
    fn say(&self, level: &str, msg: &str) {
        let line = log::format_line(&RealClock.now_iso8601(), &self.who, level, msg);
        lock_log(&self.log).write_line(&line);
    }

    /// Record the state for `worker status`; a failure is logged, not fatal.
    fn save(&self) {
        if let Err(e) = state::write_atomic(&self.paths.supervisor_state, &self.state) {
            self.say("warn", &format!("could not write supervisor.json: {e}"));
        }
    }

    fn set_phase(&mut self, phase: SupervisorPhase) {
        self.state.phase = phase;
        self.save();
    }

    /// Spawn, watch and respawn workers until stopped or given up; the exit
    /// code.
    fn run(&mut self, args: &[&str], tuning: &Tuning, stop: &AtomicBool) -> i32 {
        let mut tracker = CrashTracker::new(&tuning.policy);
        self.save();
        while !stop.load(Ordering::Relaxed) {
            let started = Instant::now();
            let (exit, description) = match self.spawn(args) {
                Ok(child) => {
                    let exit = self.watch(child, tuning, stop);
                    let description = describe(&exit);
                    (exit, description)
                }
                Err(e) => {
                    let exit = ExitKind::Crashed {
                        code: None,
                        signal: None,
                    };
                    (exit, format!("could not be started: {e}"))
                }
            };
            let stopping = stop.load(Ordering::Relaxed);
            self.say(
                if stopping { "info" } else { "warn" },
                &format!("worker {description}"),
            );
            self.state.worker_pid = None;
            self.state.last_exit = Some(exit_report(&exit, RealClock.now_unix_secs(), description));
            let now_ms = self.epoch.elapsed().as_millis() as u64;
            match tracker.on_exit(&tuning.policy, now_ms, started.elapsed(), &exit, stopping) {
                Decision::Stop => break,
                Decision::GiveUp { reason } => {
                    self.say("error", &format!("giving up: {reason}"));
                    self.state.gave_up_reason = Some(reason);
                    self.state.backoff_until = None;
                    self.set_phase(SupervisorPhase::GaveUp);
                    return 1;
                }
                Decision::Restart { after } => {
                    self.state.restarts += 1;
                    self.state.backoff_until =
                        Some(RealClock.now_unix_secs() + after.as_secs_f64().ceil() as u64);
                    self.say(
                        "info",
                        &format!("restarting the worker in {}", describe_duration(after)),
                    );
                    self.set_phase(SupervisorPhase::Backoff);
                    sleep_unless_stopped(after, tuning.poll, stop);
                    self.state.backoff_until = None;
                }
            }
        }
        self.state.worker_pid = None;
        self.state.backoff_until = None;
        self.set_phase(SupervisorPhase::Stopped);
        self.say("info", "stopped");
        0
    }

    /// Start a worker and its two log pumps.
    fn spawn(&mut self, args: &[&str]) -> std::io::Result<Child> {
        let mut child = daemon::spawn_supervised(&self.exe, args, &self.paths.home)?;
        if let Some(out) = child.stdout.take() {
            pump(out, Arc::clone(&self.log));
        }
        if let Some(err) = child.stderr.take() {
            pump(err, Arc::clone(&self.log));
        }
        self.state.worker_pid = Some(child.id());
        self.state.worker_started_at = Some(RealClock.now_unix_secs());
        self.say("info", &format!("spawned worker[{}]", child.id()));
        self.set_phase(SupervisorPhase::Starting);
        Ok(child)
    }

    /// Wait for the worker to exit, stopping it if `stop` is set; how it
    /// ended.
    fn watch(&mut self, mut child: Child, tuning: &Tuning, stop: &AtomicBool) -> ExitKind {
        let mut last_ping: Option<Instant> = None;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return classify(status),
                Ok(None) => {}
                Err(e) => {
                    self.say("error", &format!("lost track of the worker: {e}"));
                    return self.stop_child(&mut child, tuning);
                }
            }
            if stop.load(Ordering::Relaxed) {
                return self.stop_child(&mut child, tuning);
            }
            if self.state.phase == SupervisorPhase::Starting
                && last_ping.is_none_or(|at| at.elapsed() >= PING_EVERY)
            {
                last_ping = Some(Instant::now());
                if crate::worker::control::ping(self.paths).is_some() {
                    self.say("info", &format!("worker[{}] is up", child.id()));
                    self.set_phase(SupervisorPhase::Running);
                }
            }
            std::thread::sleep(tuning.poll);
        }
    }

    /// `SIGTERM` the worker, give it `stop_grace` to tear down (agents
    /// terminated, state persisted), then `SIGKILL` it.
    fn stop_child(&mut self, child: &mut Child, tuning: &Tuning) -> ExitKind {
        self.set_phase(SupervisorPhase::Stopping);
        let pid = child.id();
        self.say("info", &format!("stopping worker[{pid}]"));
        daemon::terminate(pid);
        let deadline = Instant::now() + tuning.stop_grace;
        while Instant::now() < deadline {
            if let Ok(Some(status)) = child.try_wait() {
                return classify(status);
            }
            std::thread::sleep(tuning.poll);
        }
        self.say(
            "warn",
            &format!(
                "worker[{pid}] did not exit within {}; killing it",
                describe_duration(tuning.stop_grace)
            ),
        );
        let _ = child.kill();
        match child.wait() {
            Ok(status) => classify(status),
            Err(_) => ExitKind::Crashed {
                code: None,
                signal: None,
            },
        }
    }
}

/// Sleep `total` in `step`s, returning early once `stop` is set.
fn sleep_unless_stopped(total: Duration, step: Duration, stop: &AtomicBool) {
    let deadline = Instant::now() + total;
    while !stop.load(Ordering::Relaxed) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(step));
    }
}

/// The log, even if a pump thread panicked while holding it.
fn lock_log(log: &Mutex<RotatingLog>) -> std::sync::MutexGuard<'_, RotatingLog> {
    log.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Copy `stream` into the log line by line, on its own thread, until the
/// stream closes. The lock is taken per line, never across a read.
fn pump(stream: impl Read + Send + 'static, log: Arc<Mutex<RotatingLog>>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    lock_log(&log).write_line(&line);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests;
