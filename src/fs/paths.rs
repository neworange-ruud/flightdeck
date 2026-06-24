//! Relative↔absolute path helpers. State stores relative paths; runtime
//! computes absolute paths (SPECS §9).

use crate::contracts::{FlightDeckError, Result};
use std::path::{Path, PathBuf};

/// Make `abs` relative to `root` (for storing in `state.json`).
///
/// - If `abs` is already relative, return it as-is.
/// - If `abs` is under `root`, return the relative suffix.
/// - Otherwise return `FlightDeckError::Io` describing that it is outside root.
pub fn to_relative(root: &Path, abs: &Path) -> Result<PathBuf> {
    if abs.is_relative() {
        return Ok(abs.to_path_buf());
    }
    match abs.strip_prefix(root) {
        Ok(rel) => Ok(rel.to_path_buf()),
        Err(_) => Err(FlightDeckError::Io(format!(
            "path {} is outside repository root {}",
            abs.display(),
            root.display()
        ))),
    }
}

/// Resolve a stored relative path against `root` (runtime absolute path).
///
/// - If `rel` is already absolute, return it unchanged.
/// - Otherwise return `root.join(rel)`.
pub fn to_absolute(root: &Path, rel: &Path) -> PathBuf {
    if rel.is_absolute() {
        rel.to_path_buf()
    } else {
        root.join(rel)
    }
}

/// Compute the worktree path for a slug: `<root>/<worktrees_root>/<slug>`.
pub fn worktree_path(root: &Path, worktrees_root: &str, slug: &str) -> PathBuf {
    root.join(worktrees_root).join(slug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // --- to_relative ---

    #[test]
    fn to_relative_strips_root_prefix() {
        let root = Path::new("/repo");
        let abs = Path::new("/repo/.flightdeck/worktrees/my-task");
        let rel = to_relative(root, abs).unwrap();
        assert_eq!(rel, Path::new(".flightdeck/worktrees/my-task"));
    }

    #[test]
    fn to_relative_returns_relative_as_is() {
        let root = Path::new("/repo");
        let already_rel = Path::new(".flightdeck/worktrees/my-task");
        let rel = to_relative(root, already_rel).unwrap();
        assert_eq!(rel, already_rel);
    }

    #[test]
    fn to_relative_errors_outside_root() {
        let root = Path::new("/repo");
        let outside = Path::new("/other/path");
        let err = to_relative(root, outside).unwrap_err();
        match err {
            crate::contracts::FlightDeckError::Io(msg) => {
                assert!(msg.contains("outside"), "expected 'outside' in: {msg}");
            }
            other => panic!("expected Io error, got {other:?}"),
        }
    }

    #[test]
    fn to_relative_root_itself_returns_empty() {
        let root = Path::new("/repo");
        let rel = to_relative(root, root).unwrap();
        assert_eq!(rel, Path::new(""));
    }

    // --- to_absolute ---

    #[test]
    fn to_absolute_joins_root_with_relative() {
        let root = Path::new("/repo");
        let rel = Path::new(".flightdeck/worktrees/my-task");
        let abs = to_absolute(root, rel);
        assert_eq!(abs, Path::new("/repo/.flightdeck/worktrees/my-task"));
    }

    #[test]
    fn to_absolute_returns_absolute_unchanged() {
        let root = Path::new("/repo");
        let already_abs = Path::new("/other/place");
        let abs = to_absolute(root, already_abs);
        assert_eq!(abs, already_abs);
    }

    // --- round-trip ---

    #[test]
    fn round_trip_relative_to_absolute_and_back() {
        let root = Path::new("/repo");
        let original = Path::new("/repo/.flightdeck/worktrees/feature-x");
        let rel = to_relative(root, original).unwrap();
        let abs = to_absolute(root, &rel);
        assert_eq!(abs, original);
    }

    // --- worktree_path ---

    #[test]
    fn worktree_path_shape() {
        let root = Path::new("/repo");
        let path = worktree_path(root, ".flightdeck/worktrees", "add-auth-tests");
        assert_eq!(
            path,
            Path::new("/repo/.flightdeck/worktrees/add-auth-tests")
        );
    }

    #[test]
    fn worktree_path_with_nested_worktrees_root() {
        let root = Path::new("/projects/myapp");
        let path = worktree_path(root, ".flightdeck/worktrees", "fix-bug");
        assert_eq!(
            path,
            Path::new("/projects/myapp/.flightdeck/worktrees/fix-bug")
        );
    }
}
