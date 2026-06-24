//! Relative↔absolute path helpers. State stores relative paths; runtime
//! computes absolute paths (SPECS §9).

use crate::contracts::Result;
use std::path::{Path, PathBuf};

/// Make `abs` relative to `root` (for storing in `state.json`).
pub fn to_relative(root: &Path, abs: &Path) -> Result<PathBuf> {
    let _ = (root, abs);
    todo!("T2: strip root prefix to produce a relative path")
}

/// Resolve a stored relative path against `root` (runtime absolute path).
pub fn to_absolute(root: &Path, rel: &Path) -> PathBuf {
    let _ = (root, rel);
    todo!("T2: join root with rel (rel may already be absolute)")
}

/// Compute the worktree path for a slug: `<root>/<worktrees_root>/<slug>`.
pub fn worktree_path(root: &Path, worktrees_root: &str, slug: &str) -> PathBuf {
    let _ = (root, worktrees_root, slug);
    todo!("T2: build worktree path from root, worktrees_root and slug")
}
