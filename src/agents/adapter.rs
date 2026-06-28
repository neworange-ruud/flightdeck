//! Agent command validation and launch-command building (SPECS §16, §17).

use crate::contracts::{AgentDef, FlightDeckError, Result};
use std::path::{Path, PathBuf};

/// Whether `command` resolves on a given `PATH` string (pure, testable form).
/// A path-like `command` (contains a path separator) is checked directly for
/// existence as a file; a bare name is searched in each `PATH` entry, split on
/// the platform separator (`:` on Unix, `;` on Windows) (SPECS §16).
///
/// On Windows a bare name without an extension also matches when one of the
/// `PATHEXT` extensions (`.EXE`, `.CMD`, …) makes it a real file — so `claude`
/// resolves to `claude.exe`/`claude.cmd`.
pub fn command_in_path(command: &str, path_var: &str) -> bool {
    if is_path_like(command) {
        // Treat as a direct filesystem path.
        candidate_is_executable(Path::new(command))
    } else {
        // Search each entry in path_var.
        for dir in path_var.split(PATH_SEPARATOR) {
            if dir.is_empty() {
                continue;
            }
            if candidate_is_executable(&Path::new(dir).join(command)) {
                return true;
            }
        }
        false
    }
}

/// The `PATH` entry separator for the current platform.
#[cfg(windows)]
const PATH_SEPARATOR: char = ';';
#[cfg(not(windows))]
const PATH_SEPARATOR: char = ':';

/// Whether `command` looks like a filesystem path rather than a bare command
/// name. `/` counts everywhere; `\` additionally counts on Windows.
fn is_path_like(command: &str) -> bool {
    command.contains('/') || (cfg!(windows) && command.contains('\\'))
}

/// Whether `path` names an existing file we could launch. On Windows, a path
/// with no extension also matches when appending a `PATHEXT` extension yields a
/// real file.
fn candidate_is_executable(path: &Path) -> bool {
    if path.is_file() {
        return true;
    }
    #[cfg(windows)]
    {
        if path.extension().is_none() {
            for ext in pathext() {
                if path.with_extension(ext.trim_start_matches('.')).is_file() {
                    return true;
                }
            }
        }
    }
    false
}

/// The executable extensions from `PATHEXT` (without the leading dot stripped),
/// falling back to the Windows defaults when the variable is unset.
#[cfg(windows)]
fn pathext() -> Vec<String> {
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
        .split(';')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Whether `command` exists on the process `PATH` (uses the real environment).
pub fn command_exists(command: &str) -> bool {
    let path_var = std::env::var("PATH").unwrap_or_default();
    command_in_path(command, &path_var)
}

/// Validate that an agent's command exists, before any git mutation (SPECS §16).
/// Returns [`FlightDeckError::AgentMissing`] if not found.
pub fn validate_agent(agent: &AgentDef) -> Result<()> {
    if command_exists(&agent.command) {
        Ok(())
    } else {
        Err(FlightDeckError::AgentMissing(agent.command.clone()))
    }
}

/// A fully-resolved launch command for an agent (SPECS §17). No initial prompt
/// is ever included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// Build the launch spec for an agent in a worktree (SPECS §17). The task name
/// is a label only and is never passed to the agent.
pub fn build_launch(agent: &AgentDef, cwd: &Path) -> LaunchSpec {
    LaunchSpec {
        command: agent.command.clone(),
        args: agent.args.clone(),
        cwd: cwd.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{AgentDef, StatusPatterns};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn make_agent(command: &str) -> AgentDef {
        AgentDef {
            key: "test".to_string(),
            display_name: "Test Agent".to_string(),
            command: command.to_string(),
            args: vec!["--no-tty".to_string()],
            status_patterns: StatusPatterns::default(),
        }
    }

    fn create_executable(dir: &TempDir, name: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, "#!/bin/sh\n").expect("write fake binary");
        // Make it a regular file (executable bit not required for our check — we
        // only check exists+is_file, and Windows has no POSIX mode bits).
        #[cfg(unix)]
        {
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        path
    }

    // -------------------------------------------------------------------------
    // command_in_path tests
    // -------------------------------------------------------------------------

    #[test]
    fn command_in_path_finds_command_in_tempdir() {
        let dir = TempDir::new().expect("tempdir");
        create_executable(&dir, "opencode");
        let path_var = dir.path().to_str().expect("valid utf8 path");
        assert!(command_in_path("opencode", path_var));
    }

    #[test]
    fn command_in_path_returns_false_for_missing_command() {
        let dir = TempDir::new().expect("tempdir");
        let path_var = dir.path().to_str().expect("valid utf8 path");
        assert!(!command_in_path("nonexistent_cmd_xyz", path_var));
    }

    #[test]
    fn command_in_path_searches_multiple_dirs() {
        let dir1 = TempDir::new().expect("tempdir1");
        let dir2 = TempDir::new().expect("tempdir2");
        create_executable(&dir2, "myagent");

        let path_var = format!(
            "{}{PATH_SEPARATOR}{}",
            dir1.path().to_str().unwrap(),
            dir2.path().to_str().unwrap()
        );
        assert!(command_in_path("myagent", &path_var));
    }

    #[test]
    fn command_in_path_with_slash_checks_direct_path() {
        let dir = TempDir::new().expect("tempdir");
        let file_path = create_executable(&dir, "myagent");
        let path_str = file_path.to_str().unwrap();
        // Direct path with slash should succeed.
        assert!(command_in_path(path_str, ""));
    }

    #[test]
    fn command_in_path_with_slash_returns_false_for_missing_path() {
        assert!(!command_in_path("/nonexistent/path/to/agent", ""));
    }

    #[test]
    fn command_in_path_ignores_empty_path_entries() {
        // Empty path_var should not panic.
        assert!(!command_in_path("somecommand", ""));
        assert!(!command_in_path("somecommand", "::"));
    }

    // -------------------------------------------------------------------------
    // validate_agent tests — demonstrates SPECS §26 "detects missing command
    // before git mutation": the error is returned without any git call.
    // -------------------------------------------------------------------------

    #[test]
    fn validate_agent_returns_agent_missing_when_command_not_found() {
        let agent = make_agent("__definitely_not_in_path_xyz__");
        let err = validate_agent(&agent).expect_err("should fail for missing command");
        match err {
            FlightDeckError::AgentMissing(cmd) => {
                assert_eq!(cmd, "__definitely_not_in_path_xyz__");
            }
            other => panic!("expected AgentMissing, got: {other:?}"),
        }
    }

    #[test]
    fn validate_agent_ok_when_command_exists_on_path() {
        let dir = TempDir::new().expect("tempdir");
        create_executable(&dir, "myagent");
        let path_var = dir.path().to_str().unwrap();

        // Temporarily override PATH so command_exists picks it up.
        // We can't call command_in_path directly here without changing the
        // function under test, so we use a different approach: build the agent
        // with the full path (contains '/') to bypass PATH lookup.
        let full_path = dir.path().join("myagent");
        let agent = make_agent(full_path.to_str().unwrap());

        // Use command_in_path with our controlled path_var to verify logic.
        assert!(command_in_path(full_path.to_str().unwrap(), path_var));

        // validate_agent uses command_exists which reads real PATH, so we test
        // the absolute-path branch: an agent with an absolute path to an
        // existing file must pass validation.
        validate_agent(&agent).expect("should succeed for existing file path");
    }

    // -------------------------------------------------------------------------
    // build_launch tests
    // -------------------------------------------------------------------------

    #[test]
    fn build_launch_sets_command_and_args_from_agent() {
        let agent = make_agent("opencode");
        let cwd = Path::new("/tmp/worktrees/my-task");
        let spec = build_launch(&agent, cwd);

        assert_eq!(spec.command, "opencode");
        assert_eq!(spec.args, vec!["--no-tty"]);
        assert_eq!(spec.cwd, PathBuf::from("/tmp/worktrees/my-task"));
    }

    #[test]
    fn build_launch_does_not_append_initial_prompt() {
        // SPECS §17: no initial prompt is ever included.
        let agent = AgentDef {
            key: "claude".to_string(),
            display_name: "Claude Code".to_string(),
            command: "claude".to_string(),
            args: vec![],
            status_patterns: StatusPatterns::default(),
        };
        let cwd = Path::new("/tmp/worktrees/task-slug");
        let spec = build_launch(&agent, cwd);

        // Args must be exactly what the agent config says — nothing extra.
        assert_eq!(spec.args, Vec::<String>::new());
        assert_eq!(spec.command, "claude");
    }

    #[test]
    fn build_launch_uses_provided_cwd() {
        let agent = make_agent("opencode");
        let cwd = Path::new("/home/user/projects/my-repo/.flightdeck/worktrees/feat-x");
        let spec = build_launch(&agent, cwd);
        assert_eq!(spec.cwd, cwd.to_path_buf());
    }
}
