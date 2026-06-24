//! Append-only `.gitignore` updater (SPECS §6).
//!
//! Adds the two required entries only if missing, preserving all existing
//! contents and order, and reports whether anything changed.

use crate::contracts::{FileSystem, Result};
use std::path::Path;

/// Required entry: the ignored runtime state file.
pub const STATE_IGNORE_ENTRY: &str = ".flightdeck/state.json";
/// Required entry: the ignored managed worktrees directory.
pub const WORKTREES_IGNORE_ENTRY: &str = ".flightdeck/worktrees/";

/// Result of an attempted `.gitignore` update.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitignoreUpdate {
    /// Whether the file was modified.
    pub changed: bool,
    /// The entries that were appended.
    pub added: Vec<String>,
}

/// Ensure the two required FlightDeck entries are present in `<repo_root>/.gitignore`,
/// appending only the missing ones (SPECS §6).
pub fn ensure_flightdeck_gitignore(
    fs: &dyn FileSystem,
    repo_root: &Path,
) -> Result<GitignoreUpdate> {
    let _ = (fs, repo_root);
    todo!("T2: append missing entries only, preserving existing contents/order")
}
