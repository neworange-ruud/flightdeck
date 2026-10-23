//! `flightdeck worker run`: the worker itself, in the foreground (SPECS §33).
//!
//! The headless driver loop around an [`crate::host::AppHost`] in worker mode:
//! take `run.lock`, load the global config, open the worker workspace, serve
//! the web protocol and the control socket, tick until asked to stop, then
//! tear down as a TUI quit does.

// stub: filled by G

use crate::worker::paths::WorkerPaths;

/// Exit code of `worker run` when another worker already holds `run.lock`.
/// The supervisor gives up at once on it rather than restarting.
pub const EXIT_ALREADY_RUNNING: i32 = 73;
/// Exit code of `worker run` when the global config is invalid. The
/// supervisor gives up at once on it rather than restarting.
pub const EXIT_CONFIG_INVALID: i32 = 78;

/// Run the worker until it is stopped; returns the process exit code.
pub fn run(_paths: &WorkerPaths) -> i32 {
    // stub: filled by G
    crate::worker::log::log_line("error", "`worker run` is not implemented in this build yet");
    1
}
