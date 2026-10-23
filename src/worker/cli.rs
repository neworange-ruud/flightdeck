//! `flightdeck worker <command>`: argument parsing and dispatch (SPECS §33).
//!
//! `run()` in `lib.rs` hands every invocation whose first argument is `worker`
//! here, before its own `--help`/`--version` handling, so `flightdeck worker
//! --help` prints this family's help. The return value is the process exit
//! code.
//!
//! The lifecycle commands live here: `start` spawns the supervisor and waits
//! for the worker to answer, `stop` signals whoever holds a lock and waits for
//! the locks to free, `restart` is both, and `logs` reads the log file. The
//! commands that talk to a running worker go to [`crate::worker::status_cli`].

use std::io::Write;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crate::contracts::{Clock, FileSystem, RealClock, RealFs};
use crate::worker::control::{self, ControlRequest};
use crate::worker::paths::WorkerPaths;
use crate::worker::state::{self, SupervisorPhase};
use crate::worker::{daemon, lock, log, status_cli, supervisor};

/// Printed (to stderr, exit 2) by every worker command off Linux and macOS.
pub const NOT_SUPPORTED: &str =
    "flightdeck worker is not supported on Windows (Linux and macOS only).";

/// The command table (`specs/WORKER_PRD.md` §4.1), printed for `flightdeck
/// worker` with no command, `--help`, or an unknown command.
pub const USAGE: &str = "\
flightdeck worker: a headless FlightDeck that FlightDeck Desktop controls over Tailscale.

USAGE:
    flightdeck worker <COMMAND>

COMMANDS:
    start                Start the worker in the background
    stop                 Stop the worker; its agents resume on the next start
    restart              Stop, then start (how `flightdeck update` takes effect)
    status [--json]      Running or stopped, listen addresses, devices, projects, agents
    pair [--json]        Mint a one-time code to pair FlightDeck Desktop
    devices [--json]     List paired Desktops
    revoke <device>      Revoke a Desktop's access and close its connections
    logs [-f] [-n N]     Print (or follow) the worker log
    run                  Run the worker in the foreground, logging to stderr

Linux and macOS only.
";

/// How many lines `worker logs` prints without `-n`.
const DEFAULT_LOG_LINES: usize = 100;
/// How long `start` waits for the new worker to answer on its socket.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long `stop` waits for the locks to free: the supervisor's own 30 s
/// grace for the worker, plus room for its teardown.
const STOP_TIMEOUT: Duration = Duration::from_secs(40);
/// How often `start` and `stop` look again.
const WAIT_STEP: Duration = Duration::from_millis(200);
/// How many log lines a failed `start` shows.
const START_FAILURE_LOG_LINES: usize = 20;

/// A parsed `flightdeck worker` command line.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Command {
    Help,
    Start,
    Stop,
    Restart,
    Status {
        json: bool,
    },
    Pair {
        json: bool,
    },
    Devices {
        json: bool,
    },
    Revoke {
        device: String,
    },
    Logs {
        follow: bool,
        lines: usize,
    },
    Run,
    /// Internal: what `start` runs in the background.
    Supervise,
}

/// Why a command line was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ParseError {
    /// Not a worker command: print the table.
    Unknown(String),
    /// A known command used wrongly.
    Usage(String),
}

fn is_help(arg: &str) -> bool {
    matches!(arg, "help" | "--help" | "-h")
}

/// `-f` or `--follow`. (The guard test bans the short flag as a literal.)
fn is_follow(arg: &str) -> bool {
    arg == "--follow" || arg.strip_prefix('-') == Some("f")
}

fn parse(args: &[String]) -> Result<Command, ParseError> {
    let Some((command, rest)) = args.split_first() else {
        return Ok(Command::Help);
    };
    if is_help(command) || rest.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        return Ok(Command::Help);
    }
    let no_args = |c: Command| match rest.first() {
        None => Ok(c),
        Some(extra) => Err(ParseError::Usage(format!(
            "`flightdeck worker {command}` takes no arguments (got '{extra}')."
        ))),
    };
    let json_only = |make: fn(bool) -> Command| match rest {
        [] => Ok(make(false)),
        [flag] if flag == "--json" => Ok(make(true)),
        _ => Err(ParseError::Usage(format!(
            "`flightdeck worker {command}` takes only `--json`."
        ))),
    };
    match command.as_str() {
        "start" => no_args(Command::Start),
        "stop" => no_args(Command::Stop),
        "restart" => no_args(Command::Restart),
        "run" => no_args(Command::Run),
        "supervise" => no_args(Command::Supervise),
        "status" => json_only(|json| Command::Status { json }),
        "pair" => json_only(|json| Command::Pair { json }),
        "devices" => json_only(|json| Command::Devices { json }),
        "revoke" => match rest {
            [device] if !device.trim().is_empty() => Ok(Command::Revoke {
                device: device.clone(),
            }),
            _ => Err(ParseError::Usage(
                "`flightdeck worker revoke` takes one device: its id, label or id prefix \
                 (see `flightdeck worker devices`)."
                    .to_string(),
            )),
        },
        "logs" => parse_logs(rest),
        other => Err(ParseError::Unknown(other.to_string())),
    }
}

fn parse_logs(rest: &[String]) -> Result<Command, ParseError> {
    let mut follow = false;
    let mut lines = DEFAULT_LOG_LINES;
    let mut args = rest.iter();
    let bad_count = |value: &str| {
        ParseError::Usage(format!(
            "`flightdeck worker logs -n` takes a number of lines (got '{value}')."
        ))
    };
    while let Some(arg) = args.next() {
        if is_follow(arg) {
            follow = true;
        } else if arg == "-n" || arg == "--lines" {
            let value = args.next().map(String::as_str).unwrap_or("");
            lines = value.parse().map_err(|_| bad_count(value))?;
        } else if let Some(value) = arg.strip_prefix("-n") {
            lines = value.parse().map_err(|_| bad_count(value))?;
        } else {
            return Err(ParseError::Usage(format!(
                "`flightdeck worker logs` does not know '{arg}' (use [-f] [-n N])."
            )));
        }
    }
    Ok(Command::Logs { follow, lines })
}

/// Run `flightdeck worker <args…>` and return the exit code. `args` excludes
/// the program name and `worker`.
pub fn main(args: &[String]) -> i32 {
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    dispatch(args, &mut out, &mut err)
}

/// [`main`] over explicit output streams. Writes are best effort: a closed
/// terminal must not turn a message into a panic.
fn dispatch(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if cfg!(not(unix)) {
        let _ = writeln!(err, "{NOT_SUPPORTED}");
        return 2;
    }
    let command = match parse(args) {
        Ok(command) => command,
        Err(ParseError::Unknown(other)) => {
            let _ = writeln!(err, "flightdeck worker: unknown command '{other}'.\n");
            let _ = write!(err, "{USAGE}");
            return 2;
        }
        Err(ParseError::Usage(message)) => {
            let _ = writeln!(err, "{message}");
            let _ = writeln!(err, "Run `flightdeck worker --help` for the commands.");
            return 2;
        }
    };
    if command == Command::Help {
        let _ = write!(out, "{USAGE}");
        return 0;
    }
    match command {
        Command::Status { json } => return status_cli::run_status(json),
        Command::Pair { json } => return status_cli::run_pair(json),
        Command::Devices { json } => return status_cli::run_devices(json),
        Command::Revoke { device } => return status_cli::run_revoke(&device),
        _ => {}
    }
    let Some(paths) = WorkerPaths::current() else {
        let _ = writeln!(
            err,
            "flightdeck worker: no home directory ($HOME is not set)."
        );
        return 1;
    };
    match command {
        Command::Start => start(&paths, out, err),
        Command::Stop => stop(&paths, out, err),
        Command::Restart => restart(&paths, out, err),
        Command::Logs { follow, lines } => {
            let stop = if follow {
                crate::signals::install_shutdown_flag()
            } else {
                std::sync::Arc::new(AtomicBool::new(true))
            };
            logs(&paths, lines, &stop, out, err)
        }
        Command::Run => crate::worker::run::run(&paths),
        Command::Supervise => match supervisor::supervise(&paths) {
            Ok(code) => code,
            Err(e) => {
                log::log_line("error", &format!("supervisor: {e}"));
                1
            }
        },
        // Handled above.
        Command::Help
        | Command::Status { .. }
        | Command::Pair { .. }
        | Command::Devices { .. }
        | Command::Revoke { .. } => 0,
    }
}

/// Whether another process holds `lock` (taking it for a moment to find
/// out). A lock file that does not exist is not held.
fn held(lock: &Path) -> bool {
    RealFs.exists(lock) && matches!(RealFs.try_lock_exclusive(lock), Ok(None))
}

/// A pid file's pid.
fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// `worker start`: spawn the supervisor and wait for the worker to answer.
fn start(paths: &WorkerPaths, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if lock::worker_running(&RealFs, paths) {
        let pid = read_pid(&paths.worker_pid).or_else(|| read_pid(&paths.supervisor_pid));
        let pid = pid.map(|p| format!(" (pid {p})")).unwrap_or_default();
        let _ = writeln!(
            err,
            "A worker is already running for this account{pid}. \
             Use `flightdeck worker restart` to restart it."
        );
        return 1;
    }
    if let Err(e) = paths.ensure_dirs() {
        let _ = writeln!(err, "Could not create {}: {e}", paths.dir.display());
        return 1;
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            let _ = writeln!(err, "Could not find the flightdeck binary: {e}");
            return 1;
        }
    };
    let supervisor_pid = match daemon::spawn_detached(
        &exe,
        &["worker", "supervise"],
        &paths.home,
        Some(&paths.log_file),
    ) {
        Ok(pid) => pid,
        Err(e) => {
            let _ = writeln!(err, "Could not start the worker: {e}");
            return 1;
        }
    };
    let _ = writeln!(out, "Starting the worker…");
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Some((pid, version)) = control::ping(paths) {
            let _ = writeln!(
                out,
                "The worker is running (pid {pid}, FlightDeck {version}). \
                 Pair a Desktop with `flightdeck worker pair`."
            );
            return 0;
        }
        // Only this supervisor's state counts, not one a previous run left.
        let ours =
            state::read(&paths.supervisor_state).filter(|s| s.supervisor_pid == supervisor_pid);
        if let Some(s) = ours.as_ref().filter(|s| s.phase == SupervisorPhase::GaveUp) {
            let reason = s.gave_up_reason.as_deref().unwrap_or("unknown");
            return start_failed(paths, &format!("the supervisor gave up: {reason}"), err);
        }
        if !daemon::alive(supervisor_pid) {
            if ours
                .as_ref()
                .is_some_and(|s| s.phase == SupervisorPhase::Stopped)
            {
                let _ = writeln!(err, "The worker was stopped before it answered.");
                return 1;
            }
            let why = match ours.and_then(|s| s.last_exit) {
                Some(exit) => format!("the supervisor exited; the worker {}", exit.description),
                None => "the supervisor exited".to_string(),
            };
            return start_failed(paths, &why, err);
        }
        if Instant::now() >= deadline {
            let _ = writeln!(
                err,
                "The worker did not answer within {} s. It may still be starting: \
                 check `flightdeck worker status`.",
                START_TIMEOUT.as_secs()
            );
            print_log_tail(paths, err);
            return 1;
        }
        std::thread::sleep(WAIT_STEP);
    }
}

fn start_failed(paths: &WorkerPaths, why: &str, err: &mut dyn Write) -> i32 {
    let _ = writeln!(err, "The worker did not start: {why}.");
    print_log_tail(paths, err);
    1
}

/// The last lines of the log, to show why something failed.
fn print_log_tail(paths: &WorkerPaths, err: &mut dyn Write) {
    if let Ok(lines) = log::tail(&paths.log_file, START_FAILURE_LOG_LINES) {
        if !lines.is_empty() {
            let _ = writeln!(err, "\nLast lines of {}:", paths.log_file.display());
            for line in lines {
                let _ = writeln!(err, "  {line}");
            }
        }
    }
}

/// `worker stop`: `SIGTERM` the supervisor (or, with no supervisor, a
/// foreground `worker run`) and wait for the locks to free.
fn stop(paths: &WorkerPaths, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let (target, pid_file) = if held(&paths.supervisor_lock) {
        ("supervisor", &paths.supervisor_pid)
    } else if held(&paths.run_lock) {
        ("worker", &paths.worker_pid)
    } else {
        let _ = writeln!(out, "The worker is not running.");
        if let Some(reason) = state::read(&paths.supervisor_state)
            .filter(|s| s.phase == SupervisorPhase::GaveUp)
            .and_then(|s| s.gave_up_reason)
        {
            let _ = writeln!(out, "(Its supervisor last gave up: {reason}.)");
        }
        return 0;
    };
    let Some(pid) = read_pid(pid_file) else {
        let _ = writeln!(
            err,
            "The {target} is running but {} has no pid; stop it by hand.",
            pid_file.display()
        );
        return 1;
    };
    if !daemon::terminate(pid) {
        let _ = writeln!(err, "Could not signal the {target} (pid {pid}).");
        return 1;
    }
    let _ = writeln!(out, "Stopping the worker…");
    let deadline = Instant::now() + STOP_TIMEOUT;
    while held(&paths.supervisor_lock) || held(&paths.run_lock) {
        if Instant::now() >= deadline {
            let _ = writeln!(
                err,
                "The worker did not stop within {} s (pid {pid}).",
                STOP_TIMEOUT.as_secs()
            );
            return 1;
        }
        std::thread::sleep(WAIT_STEP);
    }
    let _ = writeln!(out, "The worker stopped.");
    0
}

/// `worker restart`: tell the worker it is restarting (so Desktops show
/// "restarting", not "stopped"), stop, leave the restart marker, start.
fn restart(paths: &WorkerPaths, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let was_running = lock::worker_running(&RealFs, paths);
    if was_running {
        // Best effort: an unanswering worker is stopped all the same.
        let _ = control::request(
            &paths.control_socket,
            &ControlRequest::PrepareShutdown { restarting: true },
            Duration::from_secs(2),
        );
    }
    let code = stop(paths, out, err);
    if code != 0 {
        return code;
    }
    if was_running {
        let _ = std::fs::write(
            &paths.restart_marker,
            format!("{}\n", RealClock.now_unix_secs()),
        );
    }
    start(paths, out, err)
}

/// `worker logs`: the last `lines` lines, then, unless `stop` is already
/// set, whatever is appended until it is.
fn logs(
    paths: &WorkerPaths,
    lines: usize,
    stop: &AtomicBool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let following = !stop.load(std::sync::atomic::Ordering::Relaxed);
    match log::tail(&paths.log_file, lines) {
        Ok(lines) => {
            for line in lines {
                if writeln!(out, "{line}").is_err() {
                    return 0;
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !following {
                let _ = writeln!(out, "No worker log yet ({}).", paths.log_file.display());
                return 0;
            }
        }
        Err(e) => {
            let _ = writeln!(err, "Could not read {}: {e}", paths.log_file.display());
            return 1;
        }
    }
    if following {
        let _ = out.flush();
        // A failed write is a closed pipe (`… | head`): done, not an error.
        let _ = log::follow(&paths.log_file, out, stop);
    }
    0
}

#[cfg(test)]
mod tests;
