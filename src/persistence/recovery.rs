//! Startup recovery of Agent Tabs from `state.json` + on-disk worktrees
//! (SPECS §10). Never auto-relaunches agents.

use crate::contracts::{FileSystem, GitExecutor, ProjectState, Result};
use std::path::Path;

/// Actions offered for a recovered tab (SPECS §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveredAction {
    RestartAgent,
    OpenShell,
    PushBranch,
    LocalMerge,
    CloseTab,
    RemoveStaleEntry,
}

/// The fixed action set offered for recovered tabs (SPECS §10).
pub fn recovered_tab_actions() -> Vec<RecoveredAction> {
    todo!("T5: return the §10 recovered-tab action set")
}

/// Summary of what recovery did (SPECS §10).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Ids/slugs of tabs reconstructed from disk and marked `recovered`.
    pub recovered_tabs: Vec<String>,
    /// Ids/slugs of state entries with no valid worktree on disk.
    pub stale_entries: Vec<String>,
}

/// Recover tabs: validate stored tabs, scan `worktrees_root`, reconstruct
/// missing tabs (marking them `recovered = true`), and flag stale entries.
/// Never relaunches agents (SPECS §10, §24). Mutates `state` in place.
pub fn recover(
    fs: &dyn FileSystem,
    git: &dyn GitExecutor,
    repo_root: &Path,
    worktrees_root: &Path,
    state: &mut ProjectState,
) -> Result<RecoveryReport> {
    let _ = (fs, git, repo_root, worktrees_root, state);
    todo!("T5: validate tabs, scan worktrees, reconstruct + mark recovered, never relaunch")
}
