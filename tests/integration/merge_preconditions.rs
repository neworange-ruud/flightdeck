//! Integration: local merge-back preconditions and the §5 "FlightDeck creates no
//! commits of its own" guarantee (T10, SPECS §26, §5, §13, §15).
//!
//! Drives the real `GitCli` against temporary repositories. The base worktree is
//! the main repo root (with `main` checked out); agent worktrees live under
//! `.flightdeck/worktrees/`.

use flightdeck::contracts::GitExecutor;
use flightdeck::git::repo::GitCli;
use flightdeck::git::status::{check_merge_preconditions, merge_back, MergeDecision, MergeRequest};
use flightdeck::git::worktree::create_worktree;
use std::path::Path;
use std::process::Command;

/// Create a hermetic temp git repo with a pinned `main` branch and one commit.
///
/// Returns the `TempDir` plus the canonicalized root. `.flightdeck/` is
/// gitignored so the agent worktrees living under it never make the base
/// worktree look dirty (which would otherwise trip the §13 base-dirty guard).
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
    let canonical = crate::util::canonical_root(root);
    (dir, canonical)
}

/// Run a `git -C <root> ...` command hermetically; panics on failure.
fn git(root: &Path, args: &[&str]) -> String {
    let out = run_git(root, args);
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Run a git command without asserting success; returns the raw `Output`.
fn run_git(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
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
        .expect("spawn git")
}

/// Count commits reachable from `refname` (`git rev-list --count`).
fn commit_count(root: &Path, refname: &str) -> u32 {
    git(root, &["rev-list", "--count", refname])
        .parse()
        .expect("commit count is a number")
}

/// Whether `ancestor` is an ancestor of `descendant`.
fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> bool {
    run_git(root, &["merge-base", "--is-ancestor", ancestor, descendant])
        .status
        .success()
}

#[test]
fn merge_refused_when_base_worktree_dirty() {
    let (_repo, root) = setup_repo();
    let git_cli = GitCli::new(root.clone());

    let branch = "flightdeck/feat";
    let agent_wt = root.join(".flightdeck/worktrees/feat");
    create_worktree(&git_cli, branch, "main", &agent_wt, true).expect("create worktree");

    // Make the base worktree (repo root, main checked out) dirty.
    std::fs::write(root.join("dirty.txt"), "uncommitted\n").expect("dirty base");

    let req = MergeRequest {
        base_branch: "main",
        agent_branch: branch,
        base_worktree: &root,
        agent_worktree: &agent_wt,
    };
    let decision = check_merge_preconditions(&git_cli, &req).expect("check");
    match decision {
        MergeDecision::Refused(msg) => assert!(
            msg.contains("Local merge is disabled"),
            "expected §13 base-dirty message, got: {msg}"
        ),
        MergeDecision::Allowed => panic!("expected refusal when base is dirty"),
    }
}

#[test]
fn merge_allowed_and_succeeds_when_all_preconditions_pass() {
    let (_repo, root) = setup_repo();
    let git_cli = GitCli::new(root.clone());

    let branch = "flightdeck/feat";
    let agent_wt = root.join(".flightdeck/worktrees/feat");
    create_worktree(&git_cli, branch, "main", &agent_wt, true).expect("create worktree");

    // Commit real agent work on a non-conflicting file.
    std::fs::write(agent_wt.join("feature.txt"), "shiny\n").expect("write feature");
    git(&agent_wt, &["add", "."]);
    git(&agent_wt, &["commit", "-m", "agent feature work"]);
    let agent_sha = git(&agent_wt, &["rev-parse", "HEAD"]);

    // Both worktrees clean, both branches exist, no primary running, confirmed.
    let req = MergeRequest {
        base_branch: "main",
        agent_branch: branch,
        base_worktree: &root,
        agent_worktree: &agent_wt,
    };
    assert_eq!(
        check_merge_preconditions(&git_cli, &req).expect("check"),
        MergeDecision::Allowed
    );

    let outcome = merge_back(&git_cli, &req).expect("merge_back");
    assert!(outcome.merged, "merge should succeed: {}", outcome.message);
    assert!(!outcome.conflicted);

    // The agent branch's commit is now reachable from base (main).
    assert!(
        is_ancestor(&root, &agent_sha, "main"),
        "agent commit {agent_sha} should be an ancestor of main after merge"
    );
    assert!(git_cli.branch_exists("main").expect("branch_exists"));
}

#[test]
fn flightdeck_creates_no_commits_during_worktree_and_push_flow() {
    // SPECS §5: FlightDeck must NEVER create commits on its own. Only commits
    // made by the test (raw git) may appear. We capture the base commit count
    // before and after a full worktree-create + push flow and assert it is
    // unchanged.
    let (_repo, root) = setup_repo();
    let git_cli = GitCli::new(root.clone());

    // Local bare remote — no network.
    let bare_dir = tempfile::tempdir().expect("bare tempdir");
    let bare_path = bare_dir.path().join("origin.git");
    let init_out = Command::new("git")
        .args(["init", "--bare", bare_path.to_str().unwrap()])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git init --bare");
    assert!(init_out.status.success());
    git(
        &root,
        &["remote", "add", "origin", bare_path.to_str().unwrap()],
    );

    let base_before = commit_count(&root, "main");

    // FlightDeck creates a branch + worktree (a branch ref, NOT a commit).
    let branch = "flightdeck/nocommit";
    let agent_wt = root.join(".flightdeck/worktrees/nocommit");
    create_worktree(&git_cli, branch, "main", &agent_wt, true).expect("create worktree");

    // The agent branch starts at exactly the base commit — FlightDeck added none.
    let branch_after_create = commit_count(&root, branch);
    assert_eq!(
        branch_after_create, base_before,
        "creating a worktree/branch must not add commits"
    );

    // Only the TEST makes a commit (simulating the agent's own work).
    std::fs::write(agent_wt.join("work.txt"), "agent\n").expect("write work");
    git(&agent_wt, &["add", "."]);
    git(
        &agent_wt,
        &[
            "commit",
            "-m",
            "agent commit (made by test, not flightdeck)",
        ],
    );

    // FlightDeck pushes — push moves refs, never creates commits.
    flightdeck::git::remote::push_branch(&git_cli, "origin", branch, &agent_wt)
        .expect("push_branch");

    // Base branch commit count is unchanged by anything FlightDeck did.
    let base_after = commit_count(&root, "main");
    assert_eq!(
        base_after, base_before,
        "FlightDeck must not have added any commits to the base branch"
    );

    // The branch has exactly one more commit than base — the single TEST commit.
    let branch_count = commit_count(&root, branch);
    assert_eq!(
        branch_count,
        base_before + 1,
        "only the one test-made commit should exist on the branch"
    );
}

#[test]
fn conflicting_merge_is_reported_and_not_auto_resolved() {
    let (_repo, root) = setup_repo();
    let git_cli = GitCli::new(root.clone());

    // Seed a shared file on main.
    std::fs::write(root.join("shared.txt"), "base line\n").expect("seed shared");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "add shared file"]);

    // Agent worktree branches from main, then edits shared.txt differently.
    let branch = "flightdeck/conflict";
    let agent_wt = root.join(".flightdeck/worktrees/conflict");
    create_worktree(&git_cli, branch, "main", &agent_wt, true).expect("create worktree");
    std::fs::write(agent_wt.join("shared.txt"), "agent version\n").expect("agent edit");
    git(&agent_wt, &["add", "."]);
    git(&agent_wt, &["commit", "-m", "agent edits shared"]);

    // Base advances with a conflicting edit, then is committed (so base is clean).
    std::fs::write(root.join("shared.txt"), "base version\n").expect("base edit");
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "base edits shared"]);

    let req = MergeRequest {
        base_branch: "main",
        agent_branch: branch,
        base_worktree: &root,
        agent_worktree: &agent_wt,
    };
    // Preconditions pass (both clean, both branches exist, confirmed).
    assert_eq!(
        check_merge_preconditions(&git_cli, &req).expect("check"),
        MergeDecision::Allowed
    );

    let outcome = merge_back(&git_cli, &req).expect("merge_back");
    assert!(!outcome.merged, "conflicting merge must not report success");
    assert!(outcome.conflicted, "conflict must be reported");
    assert!(
        outcome
            .message
            .contains("Manual git intervention is required"),
        "must tell the user to resolve manually, got: {}",
        outcome.message
    );

    // It must NOT have auto-resolved: a merge is in progress (MERGE_HEAD exists),
    // leaving the conflict for the user to resolve manually.
    assert!(
        root.join(".git/MERGE_HEAD").exists(),
        "merge should be left in-progress (not auto-resolved/aborted)"
    );
}
