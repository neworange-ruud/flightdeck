//! Integration: branch + worktree creation, attach-to-existing, dirty detection,
//! and refusal when a branch is checked out elsewhere (T10, SPECS §26, §5, §11).
//!
//! Drives the real `GitCli` against a temporary repository.

use flightdeck::contracts::GitExecutor;
use flightdeck::git::branch::{branch_name, decide_branch, slugify, BranchDecision};
use flightdeck::git::repo::GitCli;
use flightdeck::git::worktree::{create_worktree, plan_worktree, WorktreePlan};
use std::path::Path;
use std::process::Command;

/// Create a hermetic temp git repo with a pinned `main` branch and one commit.
///
/// Returns the `TempDir` (kept alive by the caller) plus the **canonicalized**
/// root. git reports worktree paths canonically (on macOS `/var/...` resolves to
/// `/private/var/...`), so the test builds its expected paths from the canonical
/// root to make `list_worktrees` path comparisons line up. `.flightdeck/` is
/// gitignored so the managed worktrees living under it never make the base
/// worktree look dirty.
fn setup_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@flightdeck"]);
    git(root, &["config", "user.name", "Test"]);
    std::fs::write(root.join("README.md"), "hello\n").expect("write README");
    std::fs::write(root.join(".gitignore"), ".flightdeck/\n").expect("write gitignore");
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
fn create_branch_and_worktree_for_new_slug() {
    let (_repo, root) = setup_repo();
    let git_cli = GitCli::new(root.clone());

    let slug = slugify("Implement Login");
    let branch = branch_name("flightdeck/", &slug);
    assert_eq!(branch, "flightdeck/implement-login");

    // Brand-new branch: decision is Create.
    let decision = decide_branch(&git_cli, &branch).expect("decide");
    assert_eq!(decision, BranchDecision::Create);

    let worktrees_root = root.join(".flightdeck/worktrees");
    let target = worktrees_root.join(&slug);

    let plan = plan_worktree(&git_cli, &branch, &target, &worktrees_root).expect("plan");
    assert_eq!(plan, WorktreePlan::Create);

    create_worktree(&git_cli, &branch, "main", &target, true).expect("create worktree");

    // The worktree directory exists on disk.
    assert!(target.is_dir(), "worktree dir should exist");
    // The branch now exists.
    assert!(git_cli.branch_exists(&branch).expect("branch_exists"));
    // It is a real git worktree, tracked by `git worktree list`.
    let worktrees = git_cli.list_worktrees().expect("list_worktrees");
    let found = worktrees
        .iter()
        .find(|w| w.path == target)
        .expect("worktree should be listed");
    assert_eq!(found.branch.as_deref(), Some(branch.as_str()));
    // Raw `git worktree list` confirms it too.
    let raw = git(&root, &["worktree", "list"]);
    assert!(
        raw.contains(&target.to_string_lossy().to_string()) || raw.contains(&slug),
        "raw worktree list should mention the new worktree: {raw}"
    );
}

#[test]
fn attach_to_existing_branch_reuses_it() {
    let (_repo, root) = setup_repo();
    let git_cli = GitCli::new(root.clone());

    let branch = "flightdeck/preexisting";
    // Pre-create the branch with raw git (no checkout) from main.
    git(&root, &["branch", branch, "main"]);

    // decide_branch must surface AttachExisting (never silently create).
    let decision = decide_branch(&git_cli, branch).expect("decide");
    assert_eq!(decision, BranchDecision::AttachExisting);

    let worktrees_root = root.join(".flightdeck/worktrees");
    let target = worktrees_root.join("preexisting");

    // No worktree yet for this branch → plan is Create (attach happens by NOT
    // re-creating the branch in create_worktree below).
    let plan = plan_worktree(&git_cli, branch, &target, &worktrees_root).expect("plan");
    assert_eq!(plan, WorktreePlan::Create);

    // create_branch=false → attach onto the existing branch without creating it.
    create_worktree(&git_cli, branch, "main", &target, false).expect("create worktree");

    assert!(target.is_dir());
    let worktrees = git_cli.list_worktrees().expect("list");
    let found = worktrees
        .iter()
        .find(|w| w.path == target)
        .expect("worktree listed");
    assert_eq!(found.branch.as_deref(), Some(branch));

    // After the branch is checked out in the managed worktree, re-planning
    // reports ReuseManaged.
    let replan = plan_worktree(&git_cli, branch, &target, &worktrees_root).expect("replan");
    assert_eq!(replan, WorktreePlan::ReuseManaged { path: target });
}

#[test]
fn detect_dirty_agent_worktree() {
    let (_repo, root) = setup_repo();
    let git_cli = GitCli::new(root.clone());

    let branch = "flightdeck/dirty";
    let worktrees_root = root.join(".flightdeck/worktrees");
    let target = worktrees_root.join("dirty");
    create_worktree(&git_cli, branch, "main", &target, true).expect("create worktree");

    // Clean immediately after creation.
    assert!(!git_cli.is_dirty(&target).expect("is_dirty clean"));

    // Write an uncommitted file into the worktree → dirty.
    std::fs::write(target.join("scratch.txt"), "wip\n").expect("write scratch");
    assert!(git_cli.is_dirty(&target).expect("is_dirty dirty"));
}

#[test]
fn refuse_when_branch_checked_out_elsewhere() {
    let (_repo, root) = setup_repo();
    let git_cli = GitCli::new(root.clone());

    let branch = "flightdeck/elsewhere";
    git(&root, &["branch", branch, "main"]);

    // Check the branch out in a worktree OUTSIDE the managed root.
    let elsewhere = tempfile::tempdir().expect("elsewhere tempdir");
    let elsewhere_path = elsewhere.path().join("checkout");
    git(
        &root,
        &["worktree", "add", elsewhere_path.to_str().unwrap(), branch],
    );

    let worktrees_root = root.join(".flightdeck/worktrees");
    let target = worktrees_root.join("elsewhere");

    let plan = plan_worktree(&git_cli, branch, &target, &worktrees_root).expect("plan");
    match plan {
        WorktreePlan::RefuseCheckedOutElsewhere { path } => {
            // git may canonicalize the path; compare by ending component.
            assert!(
                path.ends_with("checkout"),
                "refused path should be the elsewhere checkout, got {path:?}"
            );
        }
        other => panic!("expected RefuseCheckedOutElsewhere, got {other:?}"),
    }
}
