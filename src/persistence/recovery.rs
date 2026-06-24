//! Startup recovery of Agent Tabs from `state.json` + on-disk worktrees
//! (SPECS §10). Never auto-relaunches agents.

use crate::contracts::{FileSystem, GitExecutor, ProjectState, Result, TabState};
use std::path::Path;

/// Actions offered for a recovered tab (SPECS §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveredAction {
    RestartAgent,
    OpenShell,
    PushBranch,
    LocalMerge,
    CloseTab,
    RemoveStaleEntry,
}

/// The fixed action set offered for recovered tabs (SPECS §10).
pub fn recovered_tab_actions() -> Vec<RecoveredAction> {
    vec![
        RecoveredAction::RestartAgent,
        RecoveredAction::OpenShell,
        RecoveredAction::PushBranch,
        RecoveredAction::LocalMerge,
        RecoveredAction::CloseTab,
        RecoveredAction::RemoveStaleEntry,
    ]
}

/// Summary of what recovery did (SPECS §10).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Ids/slugs of tabs reconstructed from disk and marked `recovered`.
    pub recovered_tabs: Vec<String>,
    /// Ids/slugs of state entries with no valid worktree on disk.
    pub stale_entries: Vec<String>,
}

/// Recover tabs: validate stored tabs, scan `worktrees_root`, reconstruct
/// missing tabs (marking them `recovered = true`), and flag stale entries.
/// Never relaunches agents (SPECS §10, §24). Mutates `state` in place.
pub fn recover(
    fs: &dyn FileSystem,
    git: &dyn GitExecutor,
    repo_root: &Path,
    worktrees_root: &Path,
    state: &mut ProjectState,
) -> Result<RecoveryReport> {
    let mut report = RecoveryReport::default();

    // Fetch all known git worktrees once.
    let known_worktrees = git.list_worktrees()?;

    // Step 1: Validate stored tabs — detect stale entries.
    for tab in &mut state.tabs {
        let abs_path = repo_root.join(&tab.worktree_path_relative);
        let on_disk = fs.exists(&abs_path);
        let in_git = known_worktrees.iter().any(|w| w.path == abs_path);

        if !on_disk || !in_git {
            // Record as stale; leave it in state for UI to offer "Remove stale entry".
            report.stale_entries.push(tab.id.clone());
        }
    }

    // Step 2: Scan worktrees_root for subdirectories that are not yet in state.tabs.
    // If the directory does not exist we treat it as empty (nothing to recover).
    let dir_entries = if fs.exists(worktrees_root) {
        fs.list_dir(worktrees_root)?
    } else {
        vec![]
    };

    // Build a set of worktree_path_relative values already tracked in state.
    let tracked_relatives: std::collections::HashSet<String> = state
        .tabs
        .iter()
        .map(|t| t.worktree_path_relative.clone())
        .collect();

    for entry in dir_entries {
        // Compute the relative path from repo_root to this entry.
        let relative = match entry.strip_prefix(repo_root) {
            Ok(rel) => rel.to_string_lossy().to_string(),
            Err(_) => {
                // entry is not under repo_root — use the full path string as relative key.
                entry.to_string_lossy().to_string()
            }
        };

        // Skip entries already tracked.
        if tracked_relatives.contains(&relative) {
            continue;
        }

        // Check whether this directory corresponds to a real git worktree.
        let matching_wt = known_worktrees.iter().find(|w| w.path == entry);
        if matching_wt.is_none() {
            continue;
        }
        let wt_info = matching_wt.unwrap();

        // Derive slug from directory name.
        let slug = entry
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| relative.clone());

        // Derive branch from WorktreeInfo or fall back to slug.
        let branch = wt_info
            .branch
            .clone()
            .unwrap_or_else(|| format!("flightdeck/{slug}"));

        // Stable id derived from slug.
        let id = format!("recovered-{slug}");

        let base_commit_sha = wt_info.head.clone().unwrap_or_default();

        let new_tab = TabState {
            id: id.clone(),
            name: slug.clone(),
            slug: slug.clone(),
            agent: String::new(),
            branch,
            worktree_path_relative: relative,
            base_branch: state.base_branch.clone(),
            base_commit_sha,
            created_at: String::new(),
            attached_existing_branch: false,
            recovered: true,
            last_known_status: "session lost".to_string(),
            manual_status: None,
        };

        state.tabs.push(new_tab);
        report.recovered_tabs.push(id);
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::domain::{TabState, WorktreeInfo};
    use crate::persistence::project_state::default_state;
    use crate::testing::{FakeFs, FakeGit};
    use std::path::{Path, PathBuf};

    fn make_tab(id: &str, slug: &str, relative: &str) -> TabState {
        TabState {
            id: id.to_string(),
            name: slug.to_string(),
            slug: slug.to_string(),
            agent: "opencode".to_string(),
            branch: format!("flightdeck/{slug}"),
            worktree_path_relative: relative.to_string(),
            base_branch: "main".to_string(),
            base_commit_sha: "abc".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            attached_existing_branch: false,
            recovered: false,
            last_known_status: "running".to_string(),
            manual_status: None,
        }
    }

    // -------------------------------------------------------------------------
    // recovered_tab_actions
    // -------------------------------------------------------------------------

    #[test]
    fn recovered_tab_actions_contains_all_six() {
        let actions = recovered_tab_actions();
        assert!(actions.contains(&RecoveredAction::RestartAgent));
        assert!(actions.contains(&RecoveredAction::OpenShell));
        assert!(actions.contains(&RecoveredAction::PushBranch));
        assert!(actions.contains(&RecoveredAction::LocalMerge));
        assert!(actions.contains(&RecoveredAction::CloseTab));
        assert!(actions.contains(&RecoveredAction::RemoveStaleEntry));
        assert_eq!(actions.len(), 6);
    }

    // -------------------------------------------------------------------------
    // Stale entry detection
    // -------------------------------------------------------------------------

    #[test]
    fn stale_entry_when_worktree_dir_absent() {
        // Tab refers to a worktree dir that does not exist on disk.
        let repo_root = Path::new("/repo");
        let worktrees_root = Path::new("/repo/.flightdeck/worktrees");

        let fs = FakeFs::new().with_dir(worktrees_root.to_str().unwrap());
        let git = FakeGit::new().with_root(repo_root);

        let mut state = default_state("main");
        // Tab's absolute path = /repo/.flightdeck/worktrees/missing — not in FakeFs
        state.tabs.push(make_tab(
            "tab-missing",
            "missing",
            ".flightdeck/worktrees/missing",
        ));

        let report = recover(&fs, &git, repo_root, worktrees_root, &mut state)
            .expect("recover should not fail");

        assert!(
            report.stale_entries.contains(&"tab-missing".to_string()),
            "stale_entries should contain tab-missing, got: {:?}",
            report.stale_entries
        );
    }

    // -------------------------------------------------------------------------
    // Worktree scanning and tab reconstruction
    // -------------------------------------------------------------------------

    #[test]
    fn reconstructs_tab_for_unknown_worktree_on_disk() {
        let repo_root = Path::new("/repo");
        let worktrees_root = Path::new("/repo/.flightdeck/worktrees");
        let wt_path = PathBuf::from("/repo/.flightdeck/worktrees/feature-x");

        // Filesystem has the worktree directory.
        let fs = FakeFs::new()
            .with_dir(worktrees_root.to_str().unwrap())
            .with_dir(wt_path.to_str().unwrap());

        // Git knows about this worktree.
        let git = FakeGit::new().with_root(repo_root);
        git.add_existing_worktree(WorktreeInfo {
            path: wt_path.clone(),
            branch: Some("flightdeck/feature-x".to_string()),
            head: Some("deadbeef".to_string()),
        });

        let mut state = default_state("main");

        let report = recover(&fs, &git, repo_root, worktrees_root, &mut state)
            .expect("recover should not fail");

        assert_eq!(
            report.recovered_tabs.len(),
            1,
            "should have one recovered tab"
        );
        assert_eq!(report.stale_entries.len(), 0);

        let tab = state
            .tabs
            .iter()
            .find(|t| t.slug == "feature-x")
            .expect("reconstructed tab should be in state.tabs");

        assert!(tab.recovered, "reconstructed tab must be marked recovered");
        assert_eq!(tab.branch, "flightdeck/feature-x");
        assert_eq!(tab.base_commit_sha, "deadbeef");
        assert_eq!(tab.last_known_status, "session lost");
        assert_eq!(tab.base_branch, "main");
        assert!(tab.manual_status.is_none());
    }

    // -------------------------------------------------------------------------
    // recovered = true
    // -------------------------------------------------------------------------

    #[test]
    fn recovered_flag_is_true_on_reconstructed_tabs() {
        let repo_root = Path::new("/repo");
        let worktrees_root = Path::new("/repo/.flightdeck/worktrees");
        let wt_path = PathBuf::from("/repo/.flightdeck/worktrees/task-a");

        let fs = FakeFs::new()
            .with_dir(worktrees_root.to_str().unwrap())
            .with_dir(wt_path.to_str().unwrap());

        let git = FakeGit::new().with_root(repo_root);
        git.add_existing_worktree(WorktreeInfo {
            path: wt_path.clone(),
            branch: Some("flightdeck/task-a".to_string()),
            head: None,
        });

        let mut state = default_state("main");
        recover(&fs, &git, repo_root, worktrees_root, &mut state).unwrap();

        let tab = state.tabs.iter().find(|t| t.slug == "task-a").unwrap();
        assert!(tab.recovered);
    }

    // -------------------------------------------------------------------------
    // Does NOT auto-restart agents
    // -------------------------------------------------------------------------

    #[test]
    fn recover_does_not_spawn_any_process() {
        // Recovery is purely data reconstruction — no PtyBackend available,
        // and recover() takes none. This test confirms the function signature
        // has no PtyBackend parameter and the returned state has no running
        // process markers set by recover itself.
        let repo_root = Path::new("/repo");
        let worktrees_root = Path::new("/repo/.flightdeck/worktrees");
        let wt_path = PathBuf::from("/repo/.flightdeck/worktrees/no-spawn");

        let fs = FakeFs::new()
            .with_dir(worktrees_root.to_str().unwrap())
            .with_dir(wt_path.to_str().unwrap());

        let git = FakeGit::new().with_root(repo_root);
        git.add_existing_worktree(WorktreeInfo {
            path: wt_path,
            branch: Some("flightdeck/no-spawn".to_string()),
            head: None,
        });

        let mut state = default_state("main");
        let _ = recover(&fs, &git, repo_root, worktrees_root, &mut state).unwrap();

        // No tab has last_known_status == "running" — recover sets "session lost"
        for tab in &state.tabs {
            assert_ne!(
                tab.last_known_status, "running",
                "recover must not mark tabs as running"
            );
        }
        // FakeGit tracks no added worktrees — recover never calls add_worktree
        assert!(
            git.added_worktrees().is_empty(),
            "recover must not call add_worktree"
        );
    }

    // -------------------------------------------------------------------------
    // Already-tracked worktrees are not duplicated
    // -------------------------------------------------------------------------

    #[test]
    fn does_not_duplicate_already_tracked_worktree() {
        let repo_root = Path::new("/repo");
        let worktrees_root = Path::new("/repo/.flightdeck/worktrees");
        let wt_path = PathBuf::from("/repo/.flightdeck/worktrees/known");

        let fs = FakeFs::new()
            .with_dir(worktrees_root.to_str().unwrap())
            .with_dir(wt_path.to_str().unwrap());

        let git = FakeGit::new().with_root(repo_root);
        git.add_existing_worktree(WorktreeInfo {
            path: wt_path.clone(),
            branch: Some("flightdeck/known".to_string()),
            head: None,
        });

        let mut state = default_state("main");
        // Already in state
        state.tabs.push(make_tab(
            "tab-known",
            "known",
            ".flightdeck/worktrees/known",
        ));

        let report = recover(&fs, &git, repo_root, worktrees_root, &mut state).unwrap();

        // The existing tab should NOT be in recovered_tabs
        assert!(
            report.recovered_tabs.is_empty(),
            "already-tracked tab should not be in recovered_tabs"
        );
        // And state.tabs should still have just the one entry
        assert_eq!(state.tabs.len(), 1);
    }
}
