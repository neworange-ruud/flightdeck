//! Integration: recover an Agent Tab from an on-disk worktree, and flag a stale
//! state entry whose worktree directory is gone (T10, SPECS §26, §10).
//!
//! Drives real `RealFs` + real `GitCli` against a temporary repository.

use flightdeck::contracts::RealFs;
use flightdeck::git::repo::GitCli;
use flightdeck::git::worktree::create_worktree;
use flightdeck::persistence::project_state::default_state;
use flightdeck::persistence::recovery::recover;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Create a hermetic temp git repo with a pinned `main` branch and one commit.
///
/// Returns the `TempDir` plus the canonicalized root: git reports worktree paths
/// in canonical form (on macOS `/var/...` resolves to `/private/var/...`), and
/// `recover` matches `repo_root.join(rel)` against those paths, so the test must
/// use the canonical root for the comparison to line up.
fn setup_repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@flightdeck"]);
    git(root, &["config", "user.name", "Test"]);
    std::fs::write(root.join("README.md"), "hello\n").expect("write README");
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "initial commit"]);
    let canonical = std::fs::canonicalize(root).expect("canonicalize root");
    (dir, canonical)
}

/// Run a `git -C <root> ...` command hermetically; panics on failure.
fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@flightdeck")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@flightdeck")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn recovers_tab_from_on_disk_worktree_not_in_state() {
    let (_repo, root) = setup_repo();
    let git_cli = GitCli::new(root.clone());
    let fs = RealFs;

    // Materialize a real managed worktree, but DO NOT record it in state.
    let worktrees_root = root.join(".flightdeck/worktrees");
    let target = worktrees_root.join("recovered-feature");
    create_worktree(
        &git_cli,
        "flightdeck/recovered-feature",
        "main",
        &target,
        true,
    )
    .expect("create worktree");

    // Sanity: git really knows about it.
    assert!(target.is_dir());

    // Start from a fresh state that does NOT list the worktree.
    let mut state = default_state("main");
    assert!(state.tabs.is_empty());

    let report = recover(&fs, &git_cli, &root, &worktrees_root, &mut state).expect("recover");

    assert_eq!(
        report.recovered_tabs.len(),
        1,
        "exactly one tab should be recovered, report: {report:?}"
    );
    assert!(report.stale_entries.is_empty());

    let tab = state
        .tabs
        .iter()
        .find(|t| t.slug == "recovered-feature")
        .expect("reconstructed tab present in state");
    assert!(tab.recovered, "reconstructed tab must be recovered == true");
    assert_eq!(tab.branch, "flightdeck/recovered-feature");
    assert_eq!(tab.base_branch, "main");
}

#[test]
fn flags_stale_state_entry_when_worktree_dir_removed() {
    let (_repo, root) = setup_repo();
    let git_cli = GitCli::new(root.clone());
    let fs = RealFs;

    let worktrees_root = root.join(".flightdeck/worktrees");
    // Ensure the managed root exists so the scan step does not early-out.
    std::fs::create_dir_all(&worktrees_root).expect("create worktrees root");

    // State references a worktree that was never created on disk (removed).
    let mut state = default_state("main");
    state.tabs.push(flightdeck::contracts::TabState {
        id: "tab-gone".to_string(),
        name: "gone".to_string(),
        slug: "gone".to_string(),
        agent: "opencode".to_string(),
        branch: "flightdeck/gone".to_string(),
        worktree_path_relative: ".flightdeck/worktrees/gone".to_string(),
        base_branch: "main".to_string(),
        base_commit_sha: "deadbeef".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        attached_existing_branch: false,
        recovered: false,
        last_known_status: "running".to_string(),
        manual_status: None,
    });

    let report = recover(&fs, &git_cli, &root, &worktrees_root, &mut state).expect("recover");

    assert!(
        report.stale_entries.contains(&"tab-gone".to_string()),
        "stale entry should be reported, got {report:?}"
    );
    // The entry is left in state for the UI to offer "Remove stale entry".
    assert!(state.tabs.iter().any(|t| t.id == "tab-gone"));
}
