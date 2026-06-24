//! Real [`GitExecutor`] over the `git` binary, plus repo-root / base-branch
//! detection (SPECS §5, §27).

use crate::contracts::{GitExecutor, MergeOutcome, Result, WorktreeInfo};
use std::path::{Path, PathBuf};

/// `git`-binary-backed [`GitExecutor`]. Constructed for a specific repository
/// root; methods without a `cwd` argument run against that root.
#[derive(Debug, Clone)]
pub struct GitCli {
    root: PathBuf,
}

impl GitCli {
    /// Construct bound to a known repository root.
    pub fn new(root: PathBuf) -> Self {
        GitCli { root }
    }

    /// Discover the repository root from `cwd` and construct a [`GitCli`].
    pub fn discover(cwd: &Path) -> Result<Self> {
        let _ = cwd;
        todo!("T3: run `git -C cwd rev-parse --show-toplevel`")
    }

    /// The repository root this executor is bound to.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl GitExecutor for GitCli {
    fn repo_root(&self, cwd: &Path) -> Result<PathBuf> {
        let _ = cwd;
        todo!("T3")
    }
    fn current_branch(&self, cwd: &Path) -> Result<String> {
        let _ = cwd;
        todo!("T3")
    }
    fn is_dirty(&self, cwd: &Path) -> Result<bool> {
        let _ = cwd;
        todo!("T3: `git status --porcelain`")
    }
    fn branch_exists(&self, name: &str) -> Result<bool> {
        let _ = name;
        todo!("T3")
    }
    fn create_branch(&self, name: &str, from: &str) -> Result<()> {
        let _ = (name, from);
        todo!("T3: `git branch <name> <from>` (no checkout, no history rewrite)")
    }
    fn rev_parse(&self, refname: &str) -> Result<String> {
        let _ = refname;
        todo!("T3")
    }
    fn add_worktree(&self, path: &Path, branch: &str) -> Result<()> {
        let _ = (path, branch);
        todo!("T3: `git worktree add <path> <branch>`")
    }
    fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        todo!("T3: parse `git worktree list --porcelain`")
    }
    fn remove_worktree(&self, path: &Path) -> Result<()> {
        let _ = path;
        todo!("T3: `git worktree remove`")
    }
    fn ahead_behind(&self, base: &str, branch: &str) -> Result<(u32, u32)> {
        let _ = (base, branch);
        todo!("T3: `git rev-list --left-right --count base...branch`")
    }
    fn upstream_of(&self, branch: &str) -> Result<Option<String>> {
        let _ = branch;
        todo!("T3")
    }
    fn push(&self, remote: &str, branch: &str, cwd: &Path) -> Result<()> {
        let _ = (remote, branch, cwd);
        todo!("T3: `git push` (after confirmation, SPECS §14)")
    }
    fn remote_url(&self, remote: &str) -> Result<Option<String>> {
        let _ = remote;
        todo!("T3")
    }
    fn merge_no_ff(&self, branch: &str, cwd: &Path) -> Result<MergeOutcome> {
        let _ = (branch, cwd);
        todo!("T3: `git merge --no-ff` (guarded by precondition checks, SPECS §15)")
    }
}

/// Detect the base branch: the configured value if given and valid, else the
/// current branch (SPECS §7, §12).
pub fn detect_base_branch(
    git: &dyn GitExecutor,
    cwd: &Path,
    configured: Option<&str>,
) -> Result<String> {
    let _ = (git, cwd, configured);
    todo!("T3: choose configured base branch or fall back to current branch")
}
