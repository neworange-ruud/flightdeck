//! Integration: push through a LOCAL bare repo acting as the remote — never the
//! network (T10, SPECS §26, §14). Also covers push planning and GitHub PR URLs.
//!
//! Drives the real `GitCli` against temporary repositories.

use flightdeck::contracts::GitExecutor;
use flightdeck::git::remote::{
    github_pr_url, parse_github_remote, plan_push, pr_compare_url, push_branch, PushPlan,
};
use flightdeck::git::repo::GitCli;
use flightdeck::git::worktree::create_worktree;
use std::path::Path;
use std::process::Command;

/// Create a hermetic temp git repo with a pinned `main` branch and one commit.
fn setup_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@flightdeck"]);
    git(root, &["config", "user.name", "Test"]);
    std::fs::write(root.join("README.md"), "hello\n").expect("write README");
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "initial commit"]);
    dir
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

/// Run `git --git-dir=<bare> ...` against a bare repo; panics on failure.
fn git_bare(bare: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg(format!("--git-dir={}", bare.display()))
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git --git-dir {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn push_branch_lands_in_local_bare_remote() {
    let repo = setup_repo();
    let root = repo.path().to_path_buf();
    let git_cli = GitCli::new(root.clone());

    // A local bare repo stands in for the network remote — nothing leaves disk.
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

    // Create a flightdeck branch + worktree and commit work onto it.
    let branch = "flightdeck/pushable";
    let target = root.join(".flightdeck/worktrees/pushable");
    create_worktree(&git_cli, branch, "main", &target, true).expect("create worktree");
    std::fs::write(target.join("work.txt"), "done\n").expect("write work");
    git(&target, &["add", "."]);
    git(&target, &["commit", "-m", "agent work"]);

    // The branch does not yet exist in the bare remote.
    let before = git_bare(&bare_path, &["branch", "--list", branch]);
    assert!(before.is_empty(), "branch should not yet be in remote");

    // Push via the real service, from the worktree cwd.
    push_branch(&git_cli, "origin", branch, &target).expect("push_branch");

    // The branch now exists in the bare remote.
    let after = git_bare(&bare_path, &["branch", "--list", branch]);
    assert!(
        after.contains("flightdeck/pushable"),
        "branch should now exist in remote, got: {after:?}"
    );
}

#[test]
fn plan_push_reports_ready_when_clean_and_uncommitted_when_dirty() {
    let repo = setup_repo();
    let root = repo.path().to_path_buf();
    let git_cli = GitCli::new(root.clone());

    let branch = "flightdeck/planpush";
    let target = root.join(".flightdeck/worktrees/planpush");
    create_worktree(&git_cli, branch, "main", &target, true).expect("create worktree");

    // Clean worktree → Ready.
    assert_eq!(
        plan_push(&git_cli, &target).expect("plan clean"),
        PushPlan::Ready
    );

    // Dirty worktree → UncommittedChanges.
    std::fs::write(target.join("wip.txt"), "wip\n").expect("write wip");
    assert_eq!(
        plan_push(&git_cli, &target).expect("plan dirty"),
        PushPlan::UncommittedChanges
    );
}

#[test]
fn github_remote_parsing_and_pr_urls() {
    // Pure parsing helpers (no git needed).
    assert_eq!(
        parse_github_remote("git@github.com:owner/repo.git"),
        Some(("owner".to_string(), "repo".to_string()))
    );
    assert_eq!(
        parse_github_remote("https://github.com/owner/repo.git"),
        Some(("owner".to_string(), "repo".to_string()))
    );
    assert_eq!(parse_github_remote("git@gitlab.com:owner/repo.git"), None);

    assert_eq!(
        pr_compare_url("owner", "repo", "main", "flightdeck/feat"),
        "https://github.com/owner/repo/compare/main...flightdeck/feat"
    );

    // github_pr_url against a real repo whose origin is a github-style URL.
    let repo = setup_repo();
    let root = repo.path().to_path_buf();
    let git_cli = GitCli::new(root.clone());
    git(
        &root,
        &["remote", "add", "origin", "git@github.com:acme/widgets.git"],
    );

    let url = github_pr_url(&git_cli, "origin", "main", "flightdeck/feat").expect("github_pr_url");
    assert_eq!(
        url,
        Some("https://github.com/acme/widgets/compare/main...flightdeck/feat".to_string())
    );

    // A non-github remote yields None.
    git(
        &root,
        &[
            "remote",
            "set-url",
            "origin",
            "git@gitlab.com:acme/widgets.git",
        ],
    );
    let none = github_pr_url(&git_cli, "origin", "main", "flightdeck/feat").expect("github_pr_url");
    assert_eq!(none, None);

    // Reference GitExecutor so the trait import is exercised meaningfully.
    assert!(git_cli.branch_exists("main").expect("branch_exists"));
}
