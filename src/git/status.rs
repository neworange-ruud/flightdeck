//! Status collection, base-drift calculation, and local merge-back precondition
//! checks (SPECS §12, §13, §15, §21).

use crate::contracts::{GitExecutor, MergeOutcome, Result};
use std::path::{Path, PathBuf};

/// Number of commits the base branch has moved since the stored base SHA
/// (SPECS §12 "Base moved: N commits ahead since tab creation").
pub fn base_drift(git: &dyn GitExecutor, base_branch: &str, base_commit_sha: &str) -> Result<u32> {
    let _ = (git, base_branch, base_commit_sha);
    todo!("T3: ahead count of base_branch over base_commit_sha")
}

/// Lightweight git status for an Agent Tab's worktree (SPECS §21).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeStatus {
    pub branch: String,
    pub base_branch: String,
    pub dirty: bool,
    pub ahead: u32,
    pub behind: u32,
    pub upstream: Option<String>,
    pub base_drift: u32,
    pub worktree_path: PathBuf,
}

/// Collect the status panel data for an Agent Tab (SPECS §21).
pub fn collect_status(
    git: &dyn GitExecutor,
    branch: &str,
    base_branch: &str,
    base_commit_sha: &str,
    worktree_path: &Path,
) -> Result<WorktreeStatus> {
    let _ = (git, branch, base_branch, base_commit_sha, worktree_path);
    todo!("T3: assemble dirty/ahead-behind/upstream/base_drift")
}

/// Whether a guarded local merge-back is allowed, or why it is refused
/// (SPECS §13, §15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeDecision {
    Allowed,
    Refused(String),
}

/// Inputs to a local merge-back precondition check (SPECS §15).
#[derive(Debug, Clone)]
pub struct MergeRequest<'a> {
    pub base_branch: &'a str,
    pub agent_branch: &'a str,
    pub base_worktree: &'a Path,
    pub agent_worktree: &'a Path,
    pub primary_running: bool,
    pub user_confirmed: bool,
}

/// Check all local merge-back preconditions (SPECS §15): both worktrees clean,
/// both branches exist, no running primary agent unless explicitly stopped,
/// user confirmed. Refuses with a clear reason otherwise; base-dirty disables
/// merge entirely (SPECS §13).
pub fn check_merge_preconditions(
    git: &dyn GitExecutor,
    req: &MergeRequest<'_>,
) -> Result<MergeDecision> {
    let _ = (git, req);
    todo!("T3: evaluate all §15 preconditions, returning Allowed or Refused(reason)")
}

/// Perform a guarded local merge-back: re-checks preconditions, then merges
/// `--no-ff`. Stops and explains on conflict (no auto conflict resolution,
/// SPECS §15).
pub fn merge_back(git: &dyn GitExecutor, req: &MergeRequest<'_>) -> Result<MergeOutcome> {
    let _ = (git, req);
    todo!("T3: check_merge_preconditions then merge_no_ff; never resolve conflicts")
}
