//! First-run initialization of `.flightdeck/` (SPECS §7).
//!
//! Creates the metadata directory, `config.toml`, `state.json`, and
//! `worktrees/` if missing. Idempotent: does not duplicate work if already
//! present. The `.gitignore` update is handled separately by [`crate::fs::ignore`]
//! and orchestrated by startup (SPECS §7 step 8).

use crate::contracts::{FileSystem, Result};
use std::path::Path;

/// What first-run init created (each `true` only if it was missing before).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitOutcome {
    pub created_flightdeck_dir: bool,
    pub created_config: bool,
    pub created_state: bool,
    pub created_worktrees_dir: bool,
}

/// Ensure `.flightdeck/` and its contents exist under `repo_root` (SPECS §7).
pub fn initialize(
    fs: &dyn FileSystem,
    repo_root: &Path,
    project_name: &str,
    base_branch: &str,
) -> Result<InitOutcome> {
    let _ = (fs, repo_root, project_name, base_branch);
    todo!("T1: create .flightdeck/, config.toml, state.json, worktrees/ if missing")
}
