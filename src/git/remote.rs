//! Remote parsing, push planning, and GitHub PR compare URLs (SPECS §14).

use crate::contracts::{GitExecutor, Result};
use std::path::Path;

/// Parse a GitHub remote URL into `(owner, repo)` for both SSH and HTTPS forms
/// (SPECS §14):
/// - `git@github.com:owner/repo.git`
/// - `https://github.com/owner/repo.git`
pub fn parse_github_remote(url: &str) -> Option<(String, String)> {
    let _ = url;
    todo!("T3: parse owner/repo from ssh and https GitHub URLs")
}

/// Build the GitHub PR compare URL (SPECS §14):
/// `https://github.com/<owner>/<repo>/compare/<base>...<branch>`.
pub fn pr_compare_url(owner: &str, repo: &str, base: &str, branch: &str) -> String {
    let _ = (owner, repo, base, branch);
    todo!("T3: format PR compare URL")
}

/// Push planning: whether the worktree is ready to push or has uncommitted
/// changes that warrant a warning (SPECS §14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushPlan {
    /// Clean worktree; push will include all committed work.
    Ready,
    /// Worktree has uncommitted changes; warn that push only includes commits.
    UncommittedChanges,
}

/// Inspect the worktree to plan a push (SPECS §14).
pub fn plan_push(git: &dyn GitExecutor, worktree: &Path) -> Result<PushPlan> {
    let _ = (git, worktree);
    todo!("T3: is_dirty -> UncommittedChanges else Ready")
}

/// Push `branch` to `remote` from `worktree` after confirmation (SPECS §14).
pub fn push_branch(
    git: &dyn GitExecutor,
    remote: &str,
    branch: &str,
    worktree: &Path,
) -> Result<()> {
    let _ = (git, remote, branch, worktree);
    todo!("T3: GitExecutor::push")
}

/// Compute the PR compare URL for `branch` if `remote` is a GitHub remote
/// (SPECS §14).
pub fn github_pr_url(
    git: &dyn GitExecutor,
    remote: &str,
    base: &str,
    branch: &str,
) -> Result<Option<String>> {
    let _ = (git, remote, base, branch);
    todo!("T3: remote_url -> parse_github_remote -> pr_compare_url")
}
