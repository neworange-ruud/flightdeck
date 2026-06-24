//! Worktree create / reuse / recover / safe-remove planning (SPECS §11).

use crate::contracts::{FileSystem, GitExecutor, Result};
use std::path::{Path, PathBuf};

/// Plan for materializing a worktree for a branch (SPECS §11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreePlan {
    /// No worktree yet; create one at the managed target path.
    Create,
    /// The branch is already checked out under `.flightdeck/worktrees/`; reuse it.
    ReuseManaged { path: PathBuf },
    /// The branch is checked out elsewhere; refuse and show the path (SPECS §11).
    RefuseCheckedOutElsewhere { path: PathBuf },
}

/// Decide how to obtain a worktree for `branch` targeting `target`, where
/// `worktrees_root` is the managed worktrees directory (SPECS §11).
pub fn plan_worktree(
    git: &dyn GitExecutor,
    branch: &str,
    target: &Path,
    worktrees_root: &Path,
) -> Result<WorktreePlan> {
    let _ = (git, branch, target, worktrees_root);
    todo!("T3: inspect list_worktrees; reuse managed / refuse elsewhere / create")
}

/// Create a worktree at `target` for `branch`, creating the branch from `base`
/// first when `create_branch` is set (SPECS §11). Never force-checks-out.
pub fn create_worktree(
    git: &dyn GitExecutor,
    branch: &str,
    base: &str,
    target: &Path,
    create_branch: bool,
) -> Result<()> {
    let _ = (git, branch, base, target, create_branch);
    todo!("T3: optionally create_branch then add_worktree")
}

/// Remove a managed worktree only when it is safe to do so (SPECS §5, §15).
pub fn remove_worktree_if_safe(
    git: &dyn GitExecutor,
    fs: &dyn FileSystem,
    path: &Path,
) -> Result<()> {
    let _ = (git, fs, path);
    todo!("T3: refuse if dirty; otherwise remove_worktree")
}
