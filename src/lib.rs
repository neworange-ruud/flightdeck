//! FlightDeck: a terminal UI for orchestrating multiple local AI coding agents
//! working in parallel on the same Git project (SPECS §1).
//!
//! Architecture (SPECS §27): business logic lives in testable services behind
//! traits ([`contracts`]); the TUI dispatches commands into them and never
//! executes git/fs/pty directly. The SPECS §5 git-ownership boundary is
//! enforced by construction — no service can rewrite history or create PRs.

pub mod contracts;
pub mod testing;

pub mod agents;
pub mod app;
pub mod config;
pub mod fs;
pub mod git;
pub mod persistence;
pub mod terminal;
pub mod tui;

use crate::contracts::error::Result;

/// Entry point invoked by the binary: run first-run init, recover state, and
/// drive the Ratatui event loop (SPECS §4, §7, §10). Implemented in Phase 4.
pub fn run() -> Result<()> {
    todo!("T9: wire services, init, recover, run the Ratatui event loop")
}
