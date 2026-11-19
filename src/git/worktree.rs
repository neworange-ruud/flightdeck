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
///
/// Returns a warning for the user when the worktree was created but may run
/// into trouble — today only the long-paths case of [`prepare_long_paths`].
pub fn create_worktree(
    git: &dyn GitExecutor,
    branch: &str,
    base: &str,
    target: &Path,
    create_branch: bool,
) -> Result<Option<String>> {
    // Only Git for Windows has the 260-character path limit.
    let warning = if cfg!(windows) {
        prepare_long_paths(git)
    } else {
        None
    };
    if create_branch {
        // Create the branch from base first (no checkout), then materialize the
        // worktree onto the existing branch.
        git.create_branch(branch, base)?;
    }
    git.add_worktree(target, branch)?;
    Ok(warning)
}

/// Shown when the user's global git config turns `core.longpaths` off.
pub const LONG_PATHS_OFF_WARNING: &str = "core.longpaths is off in your global git config, so \
     files with a path over 260 characters cannot be checked out in this worktree. Turn it on \
     with: git config --global core.longpaths true";

/// Make sure Git for Windows may check out paths over 260 characters before a
/// worktree is added, and return a warning when it may not.
///
/// A worktree lives under `<repo>/.flightdeck/worktrees/<slug>/`, so every
/// path in it is tens of characters longer than in the main checkout, and a
/// repository whose deepest files fit there can fail `git worktree add` with
/// "Filename too long".
///
/// - Set globally to `true`: nothing to do.
/// - Set globally to `false`: that is the user's choice, so FlightDeck changes
///   nothing and only warns.
/// - Not set globally: FlightDeck turns it on in this repository's config,
///   which all its worktrees share, so the agent's own git works there too.
///
/// Best-effort: a failing config read or write never blocks the add, which
/// then reports git's own error if the paths are indeed too long.
pub fn prepare_long_paths(git: &dyn GitExecutor) -> Option<String> {
    match git.global_long_paths() {
        Ok(Some(true)) | Err(_) => None,
        Ok(Some(false)) => Some(LONG_PATHS_OFF_WARNING.to_string()),
        Ok(None) => {
            let _ = git.enable_repo_long_paths();
            None
        }
    }
}

/// Remove a managed worktree (SPECS §5, §15). When `force` is false the removal
/// is refused if the worktree has uncommitted changes; when `force` is true the
/// worktree is removed regardless (the caller is responsible for having obtained
/// the user's confirmation first).
///
/// If git reports the path is not a tracked worktree, the directory is treated
/// as an orphan — left behind by an earlier removal that unregistered the
/// worktree but failed to delete the directory (e.g. it was locked by a live
/// process on Windows). In that case the directory is removed directly and any
/// dangling worktree metadata is pruned, so the operation still succeeds and the
/// tab can be dropped.
pub fn remove_worktree_if_safe(
    git: &dyn GitExecutor,
    fs: &dyn FileSystem,
    path: &Path,
    force: bool,
) -> Result<()> {
    if !force && git.is_dirty(path)? {
        return Err(FlightDeckError::Refused(format!(
            "worktree '{}' has uncommitted changes; refusing to remove",
            path.display()
        )));
    }
    match git.remove_worktree(path, force) {
        Ok(()) => Ok(()),
        // Two recoverable cases:
        //  - the path is not a tracked worktree (an orphan directory left by an
        //    earlier removal that unregistered the worktree but failed to delete
        //    the directory), or
        //  - on Windows, git could not delete the directory because a just-killed
        //    process was still releasing its handles.
        // In both cases delete the directory directly (the real filesystem retries
        // through the brief Windows teardown window) and prune any dangling
        // metadata so the operation still succeeds and the tab can be dropped.
        Err(e) if is_unregistered_worktree(&e) || (cfg!(windows) && is_lock_error(&e)) => {
            if fs.exists(path) {
                fs.remove_dir_all(path)?;
            }
            let _ = git.prune_worktrees();
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Whether a git error means the path is not a tracked worktree, so a directory
/// at that path (if any) is an orphan we can remove directly. Matches git's
/// "'<path>' is not a working tree" message from `git worktree remove` (and the
/// older/alternate "is not a worktree" wording, for safety across git versions).
fn is_unregistered_worktree(err: &FlightDeckError) -> bool {
    matches!(err, FlightDeckError::Git(msg) if msg.contains("is not a working tree") || msg.contains("is not a worktree"))
}

/// Whether an error reflects a transient file lock — on Windows a directory
/// cannot be deleted while a just-killed process is still releasing its handles,
/// surfacing as a permission/access-denied or "in use" message.
fn is_lock_error(err: &FlightDeckError) -> bool {
    match err {
        FlightDeckError::Git(msg) | FlightDeckError::Io(msg) => {
            let msg = msg.to_ascii_lowercase();
            msg.contains("permission denied")
                || msg.contains("access is denied")
                || msg.contains("being used by another process")
                || msg.contains("failed to delete")
        }
        _ => false,
    }
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
    fn long_paths_set_globally_on_needs_nothing() {
        let git = FakeGit::new();
        git.set_global_long_paths(Some(true));
        assert_eq!(prepare_long_paths(&git), None);
        assert_eq!(git.repo_long_paths_enabled(), 0);
    }

    #[test]
    fn long_paths_set_globally_off_only_warns() {
        // The user turned it off on purpose: warn, but never override them.
        let git = FakeGit::new();
        git.set_global_long_paths(Some(false));
        assert_eq!(
            prepare_long_paths(&git).as_deref(),
            Some(LONG_PATHS_OFF_WARNING)
        );
        assert_eq!(git.repo_long_paths_enabled(), 0);
    }

    #[test]
    fn long_paths_unset_globally_is_enabled_on_the_repo() {
        let git = FakeGit::new();
        git.set_global_long_paths(None);
        assert_eq!(prepare_long_paths(&git), None);
        assert_eq!(git.repo_long_paths_enabled(), 1);
    }

    #[test]
    fn long_paths_read_failure_changes_nothing() {
        // A git that cannot answer is not a reason to write config or warn.
        let git = FakeGit::new();
        git.set_global_long_paths_error("fatal: bad config");
        assert_eq!(prepare_long_paths(&git), None);
        assert_eq!(git.repo_long_paths_enabled(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn create_worktree_on_windows_returns_the_long_paths_warning() {
        let git = FakeGit::new();
        git.set_global_long_paths(Some(false));
        let target = Path::new("/repo/.flightdeck/worktrees/feat");
        let warning = create_worktree(&git, "flightdeck/feat", "main", target, true).unwrap();
        assert_eq!(warning.as_deref(), Some(LONG_PATHS_OFF_WARNING));
        assert_eq!(git.added_worktrees().len(), 1, "the add still runs");
    }

    #[cfg(not(windows))]
    #[test]
    fn create_worktree_off_windows_leaves_long_paths_alone() {
        let git = FakeGit::new();
        git.set_global_long_paths(None);
        let target = Path::new("/repo/.flightdeck/worktrees/feat");
        let warning = create_worktree(&git, "flightdeck/feat", "main", target, true).unwrap();
        assert_eq!(warning, None);
        assert_eq!(git.repo_long_paths_enabled(), 0);
    }

    #[test]
    fn remove_refuses_when_dirty_without_force() {
        let git = FakeGit::new();
        let fs = FakeFs::new();
        let path = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_dirty_at(path, true);
        let err = remove_worktree_if_safe(&git, &fs, path, false).unwrap_err();
        assert!(matches!(err, FlightDeckError::Refused(_)));
        assert!(git.removed_worktrees().is_empty());
    }

    #[test]
    fn remove_forced_when_dirty() {
        let git = FakeGit::new();
        let fs = FakeFs::new();
        let path = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_dirty_at(path, true);
        remove_worktree_if_safe(&git, &fs, path, true).unwrap();
        assert_eq!(git.removed_worktrees(), vec![path.to_path_buf()]);
    }

    #[test]
    fn remove_when_clean() {
        let git = FakeGit::new();
        let fs = FakeFs::new();
        let path = Path::new("/repo/.flightdeck/worktrees/feat");
        git.set_dirty_at(path, false);
        remove_worktree_if_safe(&git, &fs, path, false).unwrap();
        assert_eq!(git.removed_worktrees(), vec![path.to_path_buf()]);
    }

    #[test]
    fn orphan_dir_removed_directly_when_not_a_worktree() {
        // git no longer tracks the path as a worktree, but the directory still
        // exists on disk: it must be deleted directly and metadata pruned.
        let git = FakeGit::new();
        let path = Path::new("/repo/.flightdeck/worktrees/orphan");
        let fs = FakeFs::new().with_dir(path);
        git.set_remove_worktree_error(format!("fatal: '{}' is not a working tree", path.display()));

        remove_worktree_if_safe(&git, &fs, path, true).unwrap();

        assert!(!fs.exists(path), "orphan directory should be deleted");
        assert_eq!(git.prune_count(), 1, "stale metadata should be pruned");
    }

    #[test]
    fn unregistered_with_no_dir_is_a_noop_success() {
        // Neither a tracked worktree nor a directory on disk: nothing to remove,
        // but the abandon must still succeed so the tab can be dropped.
        let git = FakeGit::new();
        let fs = FakeFs::new();
        let path = Path::new("/repo/.flightdeck/worktrees/gone");
        git.set_remove_worktree_error(format!("fatal: '{}' is not a working tree", path.display()));

        remove_worktree_if_safe(&git, &fs, path, true).unwrap();
        assert_eq!(git.prune_count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_lock_error_falls_back_to_direct_removal() {
        // git could not delete the directory because a just-killed process was
        // still releasing its handles: fall back to a direct (retrying) delete.
        let git = FakeGit::new();
        let path = Path::new("/repo/.flightdeck/worktrees/feat");
        let fs = FakeFs::new().with_dir(path);
        git.set_remove_worktree_error(
            "worktree remove feat failed: failed to delete 'feat': Permission denied".to_string(),
        );

        remove_worktree_if_safe(&git, &fs, path, true).unwrap();

        assert!(!fs.exists(path), "locked directory should be force-removed");
        assert_eq!(git.prune_count(), 1);
    }

    #[test]
    fn other_git_errors_propagate() {
        // A removal failure that is NOT an unregistered-worktree error must
        // surface, not be silently swallowed by the orphan fallback.
        let git = FakeGit::new();
        let path = Path::new("/repo/.flightdeck/worktrees/feat");
        let fs = FakeFs::new().with_dir(path);
        git.set_remove_worktree_error("worktree remove feat failed: disk on fire".to_string());

        let err = remove_worktree_if_safe(&git, &fs, path, true).unwrap_err();
        assert!(matches!(err, FlightDeckError::Git(_)));
        assert!(
            fs.exists(path),
            "directory must be left intact on unknown error"
        );
    }
}
