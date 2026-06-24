//! Worktree create / reuse / recover / safe-remove planning (SPECS §11).

use crate::contracts::{FileSystem, FlightDeckError, GitExecutor, Result};
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
    let _ = target;
    let worktrees = git.list_worktrees()?;
    for wt in worktrees {
        if wt.branch.as_deref() == Some(branch) {
            if is_under(&wt.path, worktrees_root) {
                return Ok(WorktreePlan::ReuseManaged { path: wt.path });
            }
            return Ok(WorktreePlan::RefuseCheckedOutElsewhere { path: wt.path });
        }
    }
    Ok(WorktreePlan::Create)
}

/// Whether `path` is the managed root itself or nested beneath it.
fn is_under(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
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
    if create_branch {
        // Create the branch from base first (no checkout), then materialize the
        // worktree onto the existing branch.
        git.create_branch(branch, base)?;
    }
    git.add_worktree(target, branch)
}

/// Remove a managed worktree only when it is safe to do so (SPECS §5, §15).
pub fn remove_worktree_if_safe(
    git: &dyn GitExecutor,
    fs: &dyn FileSystem,
    path: &Path,
) -> Result<()> {
    let _ = fs;
    if git.is_dirty(path)? {
        return Err(FlightDeckError::Refused(format!(
            "worktree '{}' has uncommitted changes; refusing to remove",
            path.display()
        )));
    }
    git.remove_worktree(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::WorktreeInfo;
    use crate::testing::{FakeFs, FakeGit};

    fn wt(path: &str, branch: &str) -> WorktreeInfo {
        WorktreeInfo {
            path: PathBuf::from(path),
            branch: Some(branch.to_string()),
            head: None,
        }
    }

    #[test]
    fn plan_create_when_branch_not_checked_out() {
        let git = FakeGit::new();
        git.add_existing_worktree(wt("/repo", "main"));
        let plan = plan_worktree(
            &git,
            "flightdeck/feat",
            Path::new("/repo/.flightdeck/worktrees/feat"),
            Path::new("/repo/.flightdeck/worktrees"),
        )
        .unwrap();
        assert_eq!(plan, WorktreePlan::Create);
    }

    #[test]
    fn plan_reuse_when_checked_out_under_managed_root() {
        let git = FakeGit::new();
        let managed = "/repo/.flightdeck/worktrees/feat";
        git.add_existing_worktree(wt(managed, "flightdeck/feat"));
        let plan = plan_worktree(
            &git,
            "flightdeck/feat",
            Path::new(managed),
            Path::new("/repo/.flightdeck/worktrees"),
        )
        .unwrap();
        assert_eq!(
            plan,
            WorktreePlan::ReuseManaged {
                path: PathBuf::from(managed)
            }
        );
    }

    #[test]
    fn plan_refuse_when_checked_out_elsewhere() {
        let git = FakeGit::new();
        let elsewhere = "/somewhere/else/feat";
        git.add_existing_worktree(wt(elsewhere, "flightdeck/feat"));
        let plan = plan_worktree(
            &git,
            "flightdeck/feat",
            Path::new("/repo/.flightdeck/worktrees/feat"),
            Path::new("/repo/.flightdeck/worktrees"),
        )
        .unwrap();
        assert_eq!(
            plan,
            WorktreePlan::RefuseCheckedOutElsewhere {
                path: PathBuf::from(elsewhere)
            }
        );
    }

    #[test]
    fn create_worktree_creates_branch_then_adds() {
        let git = FakeGit::new();
        let target = Path::new("/repo/.flightdeck/worktrees/feat");
        create_worktree(&git, "flightdeck/feat", "main", target, true).unwrap();
        assert_eq!(
            git.created_branches(),
            vec![("flightdeck/feat".to_string(), "main".to_string())]
        );
        assert_eq!(
            git.added_worktrees(),
            vec![(target.to_path_buf(), "flightdeck/feat".to_string())]
        );
    }

    #[test]
    fn create_worktree_attach_skips_branch_creation() {
        let git = FakeGit::new().with_branches(["main", "flightdeck/feat"]);
        let target = Path::new("/repo/.flightdeck/worktrees/feat");
        create_worktree(&git, "flightdeck/feat", "main", target, false).unwrap();
        assert!(git.created_branches().is_empty());
        assert_eq!(
            git.added_worktrees(),
            vec![(target.to_path_buf(), "flightdeck/feat".to_string())]
        );
    }

    #[test]
    fn remove_refuses_when_dirty() {
        let git = FakeGit::new();
        let fs = FakeFs::new();
        let path = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_dirty_at(path, true);
        let err = remove_worktree_if_safe(&git, &fs, path).unwrap_err();
        assert!(matches!(err, FlightDeckError::Refused(_)));
        assert!(git.removed_worktrees().is_empty());
    }

    #[test]
    fn remove_when_clean() {
        let git = FakeGit::new();
        let fs = FakeFs::new();
        let path = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_dirty_at(path, false);
        remove_worktree_if_safe(&git, &fs, path).unwrap();
        assert_eq!(git.removed_worktrees(), vec![path.to_path_buf()]);
    }
}
