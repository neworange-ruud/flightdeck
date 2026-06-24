//! Service traits — the seams that make every logic module testable (SPECS §26).
//!
//! Git, filesystem, and PTY access all sit behind these traits so logic can be
//! unit-tested against the fakes in [`crate::testing`]. The [`GitExecutor`] trait
//! deliberately exposes **no** history-rewriting operation (no stage, commit,
//! amend, squash, rebase, cherry-pick, or PR creation) — the SPECS §5 safety
//! boundary is enforced by construction.

use crate::contracts::domain::{MergeOutcome, ProcessState, PtySize, WorktreeInfo};
use crate::contracts::error::Result;
use std::path::{Path, PathBuf};

/// Abstraction over the `git` binary (SPECS §27).
///
/// Implementations shell out to `git` via `std::process::Command`. Methods that
/// do not take a `cwd` operate against the repository the implementation was
/// constructed for. **Never** add a history-rewriting method here (SPECS §5).
pub trait GitExecutor {
    /// Top-level directory of the repository containing `cwd`.
    fn repo_root(&self, cwd: &Path) -> Result<PathBuf>;
    /// Current branch checked out in `cwd`.
    fn current_branch(&self, cwd: &Path) -> Result<String>;
    /// Whether the working tree at `cwd` has uncommitted changes.
    fn is_dirty(&self, cwd: &Path) -> Result<bool>;
    /// The lines of `git status --porcelain` for `cwd` (one per changed path).
    /// An empty vector means the worktree is clean.
    fn status_porcelain(&self, cwd: &Path) -> Result<Vec<String>>;
    /// Whether a local branch exists.
    fn branch_exists(&self, name: &str) -> Result<bool>;
    /// Create a new branch `name` starting at `from` (does not check it out).
    fn create_branch(&self, name: &str, from: &str) -> Result<()>;
    /// Resolve a refname to a commit SHA.
    fn rev_parse(&self, refname: &str) -> Result<String>;
    /// Add a worktree at `path` checking out the existing branch `branch`.
    fn add_worktree(&self, path: &Path, branch: &str) -> Result<()>;
    /// List all worktrees of the repository.
    fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>>;
    /// Remove a worktree at `path`. When `force` is set, remove it even with
    /// uncommitted/untracked changes (`git worktree remove --force`); otherwise
    /// git refuses on a dirty worktree (SPECS §5/§15).
    fn remove_worktree(&self, path: &Path, force: bool) -> Result<()>;
    /// `(ahead, behind)` counts of `branch` relative to `base`.
    fn ahead_behind(&self, base: &str, branch: &str) -> Result<(u32, u32)>;
    /// Configured upstream of `branch`, if any (e.g. `origin/foo`).
    fn upstream_of(&self, branch: &str) -> Result<Option<String>>;
    /// Push `branch` to `remote` from working dir `cwd` (SPECS §14).
    fn push(&self, remote: &str, branch: &str, cwd: &Path) -> Result<()>;
    /// URL of `remote`, if configured.
    fn remote_url(&self, remote: &str) -> Result<Option<String>>;
    /// Perform a `--no-ff` merge of `branch` in `cwd` (SPECS §15, guarded by
    /// precondition checks in the git workflow layer — never call directly
    /// without those checks).
    fn merge_no_ff(&self, branch: &str, cwd: &Path) -> Result<MergeOutcome>;
}

/// Abstraction over filesystem operations (SPECS §26).
pub trait FileSystem {
    /// Whether a path exists.
    fn exists(&self, p: &Path) -> bool;
    /// Recursively create a directory.
    fn create_dir_all(&self, p: &Path) -> Result<()>;
    /// Read a file to a string.
    fn read_to_string(&self, p: &Path) -> Result<String>;
    /// Write (truncating) a file.
    fn write(&self, p: &Path, contents: &str) -> Result<()>;
    /// Append a single line (with trailing newline) to a file, creating it if
    /// absent. Used for the append-only `.gitignore` updater (SPECS §6).
    fn append_line(&self, p: &Path, line: &str) -> Result<()>;
    /// List the immediate entries of a directory.
    fn list_dir(&self, p: &Path) -> Result<Vec<PathBuf>>;
}

/// Spawns PTY-backed processes (SPECS §26).
pub trait PtyBackend {
    /// Spawn `cmd` with `args` in working directory `cwd` at the given size.
    fn spawn(
        &self,
        cmd: &str,
        args: &[String],
        cwd: &Path,
        size: PtySize,
    ) -> Result<Box<dyn PtySession>>;
}

/// A live PTY-backed process session (SPECS §19, §25).
pub trait PtySession: Send {
    /// Write raw bytes to the PTY input.
    fn write_input(&mut self, bytes: &[u8]) -> Result<()>;
    /// Resize the PTY.
    fn resize(&mut self, size: PtySize) -> Result<()>;
    /// Non-blocking drain of any available output bytes.
    fn try_read_output(&mut self) -> Result<Vec<u8>>;
    /// Send Ctrl-C (SIGINT) to the foreground process (SPECS §25).
    fn send_ctrl_c(&mut self) -> Result<()>;
    /// Current process state.
    fn process_state(&self) -> ProcessState;
    /// Force-terminate the whole process tree (SPECS §25 force path).
    fn terminate_tree(&mut self) -> Result<()>;
}

/// A clock, abstracted so timestamps are deterministic in tests (SPECS §26).
pub trait Clock {
    /// Current time as an ISO-8601 string.
    fn now_iso8601(&self) -> String;
}
