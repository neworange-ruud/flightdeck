//! Creating a brand-new project (SPECS §5.3): a fresh folder, `git init`, and
//! one empty root commit so the repository has a base branch that agent
//! worktrees can branch from.
//!
//! Deliberately not on [`crate::contracts::GitExecutor`], which is
//! repository-scoped and history-safe by construction: there is no repository
//! yet. This is the **only** place FlightDeck creates a commit of its own, and
//! it only ever does so in a folder it has just created and initialised, so no
//! existing history can be touched. `tests/guards.rs` holds it to this file.
//!
//! Git runs by argv, never a shell, with prompts off; failures are reported in
//! git's own words, except a missing identity, which gets a sentence saying
//! what to configure.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::contracts::FileSystem;

/// The message of the empty root commit.
pub const ROOT_COMMIT_MESSAGE: &str = "Initial commit";

/// Turns an empty folder into a repository with one empty root commit. Real:
/// [`RealRepoInitializer`]; tests: [`FakeRepoInitializer`].
pub trait RepoInitializer: Send + Sync {
    /// Initialise `dir` (an existing, empty folder) and make its root commit.
    /// `Err` is a sentence for a person.
    fn init_with_root_commit(&self, dir: &Path) -> std::result::Result<(), String>;
}

/// Whether `name` is usable as a new project's folder name:
/// `[A-Za-z0-9._-]`, not `.` or `..`, not starting with `-` or `.`.
///
/// Strict on purpose: the name becomes a path that agents, worktrees and
/// shell commands all see, and a clone's folder name obeys the same rule.
pub fn valid_folder_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The sentence a refused folder name gets.
pub fn invalid_folder_name(name: &str) -> String {
    format!(
        "{name} is not a usable folder name: use letters, digits, dots, dashes and underscores."
    )
}

/// Create the project folder `dest` and initialise it: `dest`'s parent must
/// already be a folder, and `dest` must not exist. On any failure after the
/// folder was made, the folder is removed again, so a refused create leaves
/// nothing behind. Returns `dest`.
pub fn create_project_dir(
    fs: &dyn FileSystem,
    init: &dyn RepoInitializer,
    dest: &Path,
) -> std::result::Result<PathBuf, String> {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if name.is_empty() {
        return Err("Enter a name for the new project's folder.".to_string());
    }
    if !valid_folder_name(&name) {
        return Err(invalid_folder_name(&name));
    }
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| format!("{} has no parent folder.", dest.display()))?;
    if !fs.is_dir(parent) {
        return Err(format!("{} is not a folder.", parent.display()));
    }
    if fs.exists(dest) {
        return Err(format!("{} already exists.", dest.display()));
    }
    fs.create_dir_all(dest)
        .map_err(|e| format!("Could not create {}: {e}", dest.display()))?;
    if let Err(reason) = init.init_with_root_commit(dest) {
        let _ = fs.remove_dir_all(dest);
        return Err(reason);
    }
    Ok(dest.to_path_buf())
}

/// `git init` then an empty root commit.
#[derive(Debug, Clone, Copy)]
pub struct RealRepoInitializer {
    /// Extra environment for every git run: empty in production (git reads
    /// the user's own config); tests supply an identity here.
    env: &'static [(&'static str, &'static str)],
}

/// The production initializer: git reads the user's own config.
pub static REAL_REPO_INIT: RealRepoInitializer = RealRepoInitializer { env: &[] };

/// A real initializer with a fixed identity and signing off, so a test does
/// not depend on the machine's git config (CI runners have none).
pub static TEST_REPO_INIT: RealRepoInitializer = RealRepoInitializer {
    env: &[
        ("GIT_AUTHOR_NAME", "FlightDeck Test"),
        ("GIT_AUTHOR_EMAIL", "flightdeck@example.invalid"),
        ("GIT_COMMITTER_NAME", "FlightDeck Test"),
        ("GIT_COMMITTER_EMAIL", "flightdeck@example.invalid"),
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "commit.gpgSign"),
        ("GIT_CONFIG_VALUE_0", "false"),
    ],
};

/// The sentence for a commit refused because git has no name or email.
pub const MISSING_IDENTITY: &str = "Git needs your name and email before it can make the \
new project's first commit. Run `git config --global user.name \"Your Name\"` and \
`git config --global user.email you@example.com`, then try again.";

impl RepoInitializer for RealRepoInitializer {
    fn init_with_root_commit(&self, dir: &Path) -> std::result::Result<(), String> {
        run_git(dir, self.env, &["init", "--quiet"])
            .map_err(|e| format!("git init failed: {e}"))?;
        run_git(
            dir,
            self.env,
            &[
                "commit",
                "--allow-empty",
                // A user's commit hooks are for their own commits; an empty
                // root commit has nothing for them to check.
                "--no-verify",
                "--quiet",
                "-m",
                ROOT_COMMIT_MESSAGE,
            ],
        )
        .map_err(|e| {
            if is_missing_identity(&e) {
                MISSING_IDENTITY.to_string()
            } else {
                format!("Could not make the first commit: {e}")
            }
        })
    }
}

/// Whether git's stderr says it does not know who is committing.
fn is_missing_identity(stderr: &str) -> bool {
    stderr.contains("Please tell me who you are")
        || stderr.contains("unable to auto-detect email address")
        || stderr.contains("empty ident name")
}

/// Run `git -C <dir> <args>` with no terminal to prompt on; `Err` is git's
/// own stderr (or why it could not run).
fn run_git(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> std::result::Result<(), String> {
    let mut cmd = Command::new("git");
    cmd.envs(env.iter().copied());
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: the desktop app has no console to borrow.
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        format!("git exited with {}", out.status)
    } else {
        stderr
    })
}

/// A scripted initializer, for tests: records nothing on disk, answers
/// `outcome`.
#[derive(Debug, Clone)]
pub struct FakeRepoInitializer {
    pub outcome: std::result::Result<(), String>,
}

impl RepoInitializer for FakeRepoInitializer {
    fn init_with_root_commit(&self, _dir: &Path) -> std::result::Result<(), String> {
        self.outcome.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeFs;

    fn ok() -> FakeRepoInitializer {
        FakeRepoInitializer { outcome: Ok(()) }
    }

    #[test]
    fn folder_names_are_strict() {
        for good in ["app", "my-app", "my_app", "app.v2", "A1"] {
            assert!(valid_folder_name(good), "{good}");
        }
        for bad in ["", ".", "..", "my app", "a/b", "a\\b", "ü", "a:b"] {
            assert!(!valid_folder_name(bad), "{bad}");
        }
    }

    #[test]
    fn creates_the_folder_under_an_existing_parent() {
        let fs = FakeFs::new();
        fs.create_dir_all(Path::new("/home/me/code")).unwrap();
        let dest = Path::new("/home/me/code/new-app");
        assert_eq!(create_project_dir(&fs, &ok(), dest), Ok(dest.to_path_buf()));
        assert!(fs.is_dir(dest));
    }

    #[test]
    fn refuses_a_folder_that_already_exists() {
        let fs = FakeFs::new();
        fs.create_dir_all(Path::new("/home/me/code/app")).unwrap();
        let err = create_project_dir(&fs, &ok(), Path::new("/home/me/code/app")).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn refuses_a_missing_parent_and_creates_nothing() {
        let fs = FakeFs::new();
        let dest = Path::new("/home/me/nowhere/app");
        let err = create_project_dir(&fs, &ok(), dest).unwrap_err();
        assert!(err.contains("is not a folder"), "{err}");
        assert!(!fs.exists(dest));
    }

    #[test]
    fn refuses_an_unusable_name() {
        let fs = FakeFs::new();
        fs.create_dir_all(Path::new("/home/me")).unwrap();
        let err = create_project_dir(&fs, &ok(), Path::new("/home/me/my app")).unwrap_err();
        assert!(err.contains("not a usable folder name"), "{err}");
        assert!(!fs.exists(Path::new("/home/me/my app")));
    }

    #[test]
    fn a_failed_init_removes_the_folder_it_made() {
        let fs = FakeFs::new();
        fs.create_dir_all(Path::new("/home/me")).unwrap();
        let init = FakeRepoInitializer {
            outcome: Err(MISSING_IDENTITY.to_string()),
        };
        let dest = Path::new("/home/me/app");
        assert_eq!(
            create_project_dir(&fs, &init, dest),
            Err(MISSING_IDENTITY.to_string())
        );
        assert!(!fs.exists(dest));
    }

    #[test]
    fn missing_identity_is_recognised() {
        assert!(is_missing_identity(
            "Author identity unknown\n\n*** Please tell me who you are."
        ));
        assert!(is_missing_identity(
            "fatal: unable to auto-detect email address (got 'me@host.(none)')"
        ));
        assert!(!is_missing_identity("fatal: not a git repository"));
    }

    /// The real thing, against real git: a repository with one empty commit
    /// on its current branch, so FlightDeck can open it and branch from it.
    #[test]
    fn the_real_initializer_leaves_one_empty_root_commit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("fresh");
        std::fs::create_dir(&dir).unwrap();
        // The test machine's identity is not ours to rely on.
        TEST_REPO_INIT.init_with_root_commit(&dir).unwrap();

        let out = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["log", "--format=%s", "HEAD"])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            ROOT_COMMIT_MESSAGE
        );
        let branch = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap();
        assert!(branch.status.success(), "HEAD names a branch");
    }
}
