//! Integration: initialize FlightDeck in a fresh git repo (T10, SPECS §26, §6, §7, §8).
//!
//! Exercises the real `RealFs` + real `git` against a temporary repository:
//! - `initialize` creates `.flightdeck/`, `config.toml`, `state.json`, `worktrees/`.
//! - `ensure_flightdeck_gitignore` appends the two required entries, is idempotent,
//!   and preserves prior `.gitignore` content.
//! - `load_config` reads back what was written.

use flightdeck::config::init::{ensure_global_config, initialize};
use flightdeck::config::load::{load_layered_config, parse_config};
use flightdeck::contracts::{FileSystem, RealFs};
use flightdeck::fs::ignore::{
    ensure_flightdeck_gitignore, STATE_IGNORE_ENTRY, STATUS_IGNORE_ENTRY,
    STATUS_RUNTIME_IGNORE_ENTRY, WORKTREES_IGNORE_ENTRY,
};
use std::path::Path;
use std::process::Command;

/// Create a hermetic temp git repo with a pinned `main` branch and one commit.
///
/// Isolation: `GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` point at `/dev/null` and
/// identity is supplied inline so the developer's global git config cannot leak
/// in. The initial branch is pinned to `main` so tests do not depend on the
/// machine's `init.defaultBranch`.
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

#[test]
fn initialize_creates_all_flightdeck_artifacts() {
    let repo = setup_repo();
    let root = repo.path();
    let fs = RealFs;

    let outcome = initialize(&fs, root, "my-project", "main").expect("initialize");
    assert!(outcome.created_flightdeck_dir);
    assert!(outcome.created_config);
    assert!(outcome.created_state);
    assert!(outcome.created_worktrees_dir);

    assert!(root.join(".flightdeck").is_dir());
    assert!(root.join(".flightdeck/config.toml").is_file());
    assert!(root.join(".flightdeck/state.json").is_file());
    assert!(root.join(".flightdeck/worktrees").is_dir());
}

#[test]
fn initialize_is_idempotent_on_second_run() {
    let repo = setup_repo();
    let root = repo.path();
    let fs = RealFs;

    initialize(&fs, root, "my-project", "main").expect("first init");
    let config_before =
        std::fs::read_to_string(root.join(".flightdeck/config.toml")).expect("read config");
    let state_before =
        std::fs::read_to_string(root.join(".flightdeck/state.json")).expect("read state");

    let second = initialize(&fs, root, "my-project", "main").expect("second init");
    assert!(!second.created_flightdeck_dir);
    assert!(!second.created_config);
    assert!(!second.created_state);
    assert!(!second.created_worktrees_dir);

    // Existing files must not be overwritten.
    let config_after =
        std::fs::read_to_string(root.join(".flightdeck/config.toml")).expect("read config");
    let state_after =
        std::fs::read_to_string(root.join(".flightdeck/state.json")).expect("read state");
    assert_eq!(config_before, config_after);
    assert_eq!(state_before, state_after);
}

#[test]
fn ensure_gitignore_adds_both_entries_and_is_idempotent() {
    let repo = setup_repo();
    let root = repo.path();
    let fs = RealFs;

    // Pre-seed an unrelated entry so we can assert it is preserved.
    std::fs::write(root.join(".gitignore"), "/target\n").expect("seed gitignore");

    let first = ensure_flightdeck_gitignore(&fs, root).expect("first ensure");
    assert!(first.changed);
    assert_eq!(
        first.added,
        vec![
            STATE_IGNORE_ENTRY,
            WORKTREES_IGNORE_ENTRY,
            STATUS_IGNORE_ENTRY,
            STATUS_RUNTIME_IGNORE_ENTRY,
        ]
    );

    let contents = std::fs::read_to_string(root.join(".gitignore")).expect("read gitignore");
    assert!(contents.contains(STATE_IGNORE_ENTRY));
    assert!(contents.contains(WORKTREES_IGNORE_ENTRY));
    assert!(contents.contains(STATUS_IGNORE_ENTRY));
    assert!(contents.contains(STATUS_RUNTIME_IGNORE_ENTRY));
    // Prior content preserved and still first.
    assert_eq!(contents.lines().next(), Some("/target"));

    // Second run is a no-op and does not duplicate.
    let second = ensure_flightdeck_gitignore(&fs, root).expect("second ensure");
    assert!(!second.changed);
    assert!(second.added.is_empty());

    let after = std::fs::read_to_string(root.join(".gitignore")).expect("read gitignore");
    let state_count = after
        .lines()
        .filter(|l| l.trim() == STATE_IGNORE_ENTRY)
        .count();
    let wt_count = after
        .lines()
        .filter(|l| l.trim() == WORKTREES_IGNORE_ENTRY)
        .count();
    assert_eq!(state_count, 1, "state entry must appear exactly once");
    assert_eq!(wt_count, 1, "worktrees entry must appear exactly once");
    assert!(after.contains("/target"), "prior content preserved");
}

#[test]
fn ensure_gitignore_creates_file_when_absent() {
    let repo = setup_repo();
    let root = repo.path();
    let fs = RealFs;

    assert!(!fs.exists(&root.join(".gitignore")));
    let update = ensure_flightdeck_gitignore(&fs, root).expect("ensure");
    assert!(update.changed);
    assert!(fs.exists(&root.join(".gitignore")));
}

#[test]
fn initialize_writes_minimal_project_config() {
    let repo = setup_repo();
    let root = repo.path();
    let fs = RealFs;

    initialize(&fs, root, "round-trip-project", "develop").expect("initialize");

    // The project config is minimal — only project identity, parsed directly
    // (it does not validate stand-alone because agents live in the global base).
    let contents = fs
        .read_to_string(&root.join(".flightdeck/config.toml"))
        .expect("read project config");
    let cfg = parse_config(&contents).expect("parse project config");
    assert_eq!(cfg.project.name, "round-trip-project");
    assert_eq!(cfg.project.default_base_branch, "develop");
    assert!(cfg.agents.is_empty(), "project config carries no agents");
}

#[test]
fn layered_config_reads_back_global_plus_project() {
    let repo = setup_repo();
    let root = repo.path();
    let fs = RealFs;

    // Write the global base into a temp home, then the minimal project config,
    // and load the effective (layered) config back.
    let home = tempfile::tempdir().expect("tempdir");
    let global_path = home.path().join(".flightdeck/config.toml");
    ensure_global_config(&fs, &global_path).expect("ensure global");
    initialize(&fs, root, "round-trip-project", "develop").expect("initialize");

    let cfg = load_layered_config(&fs, &global_path, &root.join(".flightdeck/config.toml"))
        .expect("load layered config");
    // Project identity from the project file...
    assert_eq!(cfg.project.name, "round-trip-project");
    assert_eq!(cfg.project.default_base_branch, "develop");
    // ...and the three known agents inherited from the global base.
    assert!(cfg.agents.contains_key("opencode"));
    assert!(cfg.agents.contains_key("claude"));
    assert!(cfg.agents.contains_key("codex"));
    // Notifications are on by default.
    assert!(cfg.notifications.enabled);
}
