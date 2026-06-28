//! Shared helpers for the integration suite.

use std::path::{Path, PathBuf};

/// Canonicalize a repo root for path comparisons against git's reported paths.
///
/// git reports worktree paths canonically (on macOS `/var/...` resolves to
/// `/private/var/...`), so tests build expected paths from the canonical root to
/// make `list_worktrees` comparisons line up.
///
/// On Windows `std::fs::canonicalize` returns an extended-length *verbatim* path
/// (`\\?\C:\...`). MSYS2 git mangles that to `//?/C:/...` and rejects it
/// ("could not create leading directories … Invalid argument"), so we strip the
/// `\\?\` prefix to hand git a path it accepts and whose reported form matches.
pub fn canonical_root(root: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(root).expect("canonicalize root");
    strip_verbatim_prefix(canonical)
}

#[cfg(windows)]
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

#[cfg(not(windows))]
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    path
}
