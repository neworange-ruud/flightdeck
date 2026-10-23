//! `flightdeck worker <command>`: argument parsing and dispatch (SPECS §33).
//!
//! `run()` in `lib.rs` hands every invocation whose first argument is `worker`
//! here, before its own `--help`/`--version` handling, so `flightdeck worker
//! --help` prints this family's help. The return value is the process exit
//! code.

// stub: filled by F (start/stop/restart/logs/run/supervise, and the
// status/pair/devices/revoke routes into `status_cli`)

use std::io::Write;

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

/// The commands this family knows (`supervise` is internal: `start` runs it).
const COMMANDS: &[&str] = &[
    "start",
    "stop",
    "restart",
    "status",
    "pair",
    "devices",
    "revoke",
    "logs",
    "run",
    "supervise",
];

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
    match args.first().map(String::as_str) {
        None | Some("help" | "--help" | "-h") => {
            let _ = write!(out, "{USAGE}");
            0
        }
        Some(command) if COMMANDS.contains(&command) => {
            let _ = writeln!(
                err,
                "flightdeck worker {command}: not implemented in this build yet."
            );
            1
        }
        Some(other) => {
            let _ = writeln!(err, "flightdeck worker: unknown command '{other}'.\n");
            let _ = write!(err, "{USAGE}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = dispatch(&args, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn no_command_or_help_prints_the_table() {
        for args in [&[][..], &["--help"], &["help"]] {
            let (code, out, _) = run(args);
            assert_eq!(code, 0, "{args:?}");
            assert_eq!(out, USAGE);
        }
        assert!(USAGE.contains("revoke <device>"));
        assert!(USAGE.contains("logs [-f] [-n N]"));
    }

    #[cfg(unix)]
    #[test]
    fn an_unknown_command_prints_the_table_and_fails() {
        let (code, out, err) = run(&["frobnicate"]);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(err.contains("unknown command 'frobnicate'"), "{err}");
        assert!(err.contains(USAGE), "{err}");
    }

    #[cfg(not(unix))]
    #[test]
    fn every_command_is_refused_off_unix() {
        for args in [&[][..], &["start"], &["--help"]] {
            let (code, _, err) = run(args);
            assert_eq!(code, 2);
            assert_eq!(err.trim_end(), NOT_SUPPORTED);
        }
    }
}
