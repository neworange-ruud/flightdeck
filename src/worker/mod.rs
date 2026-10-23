//! `flightdeck worker …`: the headless host FlightDeck Desktop controls over
//! Tailscale (SPECS §33, `specs/WORKER_PRD.md`).
//!
//! This module is the command-line half: argument parsing, the supervisor
//! that keeps a worker alive, the detached-process plumbing, the rotating log,
//! the control socket the CLI uses to reach a running worker, and the
//! `worker run` loop around an [`crate::host::AppHost`] in worker mode. The
//! host half (what a worker *does* with its projects) lives in
//! [`crate::host::worker`].
//!
//! Process model (SPECS §33): `worker start` spawns `worker supervise` as a
//! new process (never a fork), which spawns `worker run`. Each holds its own
//! lock file ([`lock`]), so "is a worker running?" is answered by the OS, not
//! by a pid file that may be stale. Linux and macOS only: on Windows every
//! subcommand exits with a clear "not supported" error.

pub mod cli;
pub mod control;
pub mod daemon;
pub mod lock;
pub mod log;
pub mod paths;
pub mod run;
pub mod state;
pub mod status;
pub mod status_cli;
pub mod supervisor;

pub use cli::main;
