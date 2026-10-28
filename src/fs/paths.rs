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

    /// Build a platform-absolute path from a `/`-separated suffix. Windows
    /// requires a drive prefix for `Path::is_absolute()` to hold (a bare
    /// `/repo` has a root but no prefix, so it reads as *relative* there), so
    /// these tests can't hardcode Unix-rooted literals. `Path` comparison is
    /// component-wise and Windows accepts `/` as a separator, so the relative
    /// *expectations* below can keep their `/` literals unchanged.
    fn abs(suffix: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:\\{}", suffix.replace('/', "\\")))
        } else {
            PathBuf::from(format!("/{suffix}"))
        }
    }

    // --- to_relative ---

    #[test]
    fn to_relative_strips_root_prefix() {
        let root = abs("repo");
        let target = abs("repo/.flightdeck/worktrees/my-task");
        let rel = to_relative(&root, &target).unwrap();
        assert_eq!(rel, Path::new(".flightdeck/worktrees/my-task"));
    }

    #[test]
    fn to_relative_returns_relative_as_is() {
        let root = abs("repo");
        let already_rel = Path::new(".flightdeck/worktrees/my-task");
        let rel = to_relative(&root, already_rel).unwrap();
        assert_eq!(rel, already_rel);
    }

    #[test]
    fn to_relative_errors_outside_root() {
        let root = abs("repo");
        let outside = abs("other/path");
        let err = to_relative(&root, &outside).unwrap_err();
        match err {
            crate::contracts::FlightDeckError::Io(msg) => {
                assert!(msg.contains("outside"), "expected 'outside' in: {msg}");
            }
            other => panic!("expected Io error, got {other:?}"),
        }
    }

    #[test]
    fn to_relative_root_itself_returns_empty() {
        let root = abs("repo");
        let rel = to_relative(&root, &root).unwrap();
        assert_eq!(rel, Path::new(""));
    }

    // --- to_absolute ---

    #[test]
    fn to_absolute_joins_root_with_relative() {
        let root = abs("repo");
        let rel = Path::new(".flightdeck/worktrees/my-task");
        let target = to_absolute(&root, rel);
        assert_eq!(target, abs("repo/.flightdeck/worktrees/my-task"));
    }

    #[test]
    fn to_absolute_returns_absolute_unchanged() {
        let root = abs("repo");
        let already_abs = abs("other/place");
        let target = to_absolute(&root, &already_abs);
        assert_eq!(target, already_abs);
    }

    // --- round-trip ---

    #[test]
    fn round_trip_relative_to_absolute_and_back() {
        let root = abs("repo");
        let original = abs("repo/.flightdeck/worktrees/feature-x");
        let rel = to_relative(&root, &original).unwrap();
        let target = to_absolute(&root, &rel);
        assert_eq!(target, original);
    }

    // --- worktree_path ---

    #[test]
    fn worktree_path_shape() {
        let root = abs("repo");
        let path = worktree_path(&root, ".flightdeck/worktrees", "add-auth-tests");
        assert_eq!(path, abs("repo/.flightdeck/worktrees/add-auth-tests"));
    }

    #[test]
    fn worktree_path_with_nested_worktrees_root() {
        let root = abs("projects/myapp");
        let path = worktree_path(&root, ".flightdeck/worktrees", "fix-bug");
        assert_eq!(path, abs("projects/myapp/.flightdeck/worktrees/fix-bug"));
    }
}
