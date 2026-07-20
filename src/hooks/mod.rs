//! Repository worktree lifecycle hooks (`.flightdeck/hooks.toml`).
//!
//! A repository may ship a `.flightdeck/hooks.toml` describing shell commands to
//! run automatically at points in a worktree's lifecycle:
//!
//! - `[worktree_created]` — right after a new worktree is created for an Agent
//!   Tab, run in the new worktree directory (e.g. `npm install`).
//! - `[worktree_update]` — after an Agent Tab's worktree is rebased onto an
//!   updated base branch, run in that worktree.
//!
//! Each hook has a `commands` array of shell scripts. Commands run sequentially
//! through the platform shell ([`crate::contracts::CommandRunner`]); a command
//! may span multiple lines using TOML triple-quoted strings, in which case the
//! whole block runs as one script. If a command exits non-zero the remaining
//! commands for that hook are skipped.
//!
//! Hooks are **best-effort**: a failing hook never fails (or rolls back) the
//! worktree it ran in — the worktree already exists, so removing it would lose
//! the user's work. The caller surfaces failures as a warning.
//!
//! The file is created (empty, only commented) on first run and is gitignored by
//! default, so a developer consciously opts in — and can share it with their team
//! by un-ignoring and committing it (SPECS §7).

use crate::contracts::{CommandRunner, FileSystem, Result};
use serde::Deserialize;
use std::path::Path;

/// File name of the per-repo hooks file, under `.flightdeck/`.
pub const HOOKS_FILE_NAME: &str = "hooks.toml";

/// The `.flightdeck/hooks.toml` written on first run: fully documented, with both
/// hooks present but empty so nothing runs until the user opts in (SPECS §7).
pub const DEFAULT_HOOKS_TEMPLATE: &str = r#"# FlightDeck hooks (.flightdeck/hooks.toml)
#
# Commands to run automatically at points in a worktree's lifecycle.
#
# This file is gitignored by default, so it only affects your own machine until
# you opt in. To share these hooks with your team, remove the
# ".flightdeck/hooks.toml" line from your .gitignore and commit this file.
#
# Each hook has a `commands` array. Commands run sequentially in the worktree
# directory, through your shell (sh -c on macOS/Linux, cmd /C on Windows). If a
# command exits non-zero, the remaining commands for that hook are skipped.
#
# A command may span multiple lines using TOML's triple-quoted strings — the
# whole block runs as a single shell script:
#
#   [worktree_created]
#   commands = [
#     "cp .env.example .env",
#     """
#     npm install
#     npm run build
#     """,
#   ]

# Runs in a new worktree right after it is created for an Agent Tab.
[worktree_created]
commands = []

# Runs in an Agent Tab's worktree after it is rebased onto an updated base branch.
[worktree_update]
commands = []
"#;

/// Parsed hooks for a repository. Absent sections are empty (no commands).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hooks {
    /// Commands to run in a freshly-created worktree.
    pub worktree_created: Vec<String>,
    /// Commands to run in a worktree after it is rebased onto an updated base.
    pub worktree_update: Vec<String>,
}

impl Hooks {
    /// Whether no hook defines any command (nothing to run).
    pub fn is_empty(&self) -> bool {
        self.worktree_created.is_empty() && self.worktree_update.is_empty()
    }
}

/// Serde shape of `hooks.toml`. Unknown keys are ignored so a file written by a
/// newer FlightDeck (with more hooks) still parses on an older one.
#[derive(Debug, Default, Deserialize)]
struct HooksFile {
    #[serde(default)]
    worktree_created: HookSection,
    #[serde(default)]
    worktree_update: HookSection,
}

#[derive(Debug, Default, Deserialize)]
struct HookSection {
    #[serde(default)]
    commands: Vec<String>,
}

/// Parse hooks from a TOML string. Empty input is valid (no hooks).
pub fn parse_hooks(toml_str: &str) -> Result<Hooks> {
    let file: HooksFile = toml::from_str(toml_str).map_err(|e| {
        crate::contracts::FlightDeckError::Config(format!("failed to parse hooks.toml: {e}"))
    })?;
    Ok(Hooks {
        worktree_created: file.worktree_created.commands,
        worktree_update: file.worktree_update.commands,
    })
}

/// Load the hooks for the repository rooted at `repo_root` from
/// `<repo_root>/.flightdeck/hooks.toml`. Best-effort: a missing file yields empty
/// hooks, and an unparsable file is ignored (with a stderr note) rather than
/// blocking worktree creation — hooks must never break the core flow.
pub fn load_hooks(fs: &dyn FileSystem, repo_root: &Path) -> Hooks {
    let path = repo_root.join(".flightdeck").join(HOOKS_FILE_NAME);
    if !fs.exists(&path) {
        return Hooks::default();
    }
    let contents = match fs.read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FlightDeck: could not read {}: {e}", path.display());
            return Hooks::default();
        }
    };
    match parse_hooks(&contents) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("FlightDeck: ignoring unparsable {}: {e}", path.display());
            Hooks::default()
        }
    }
}

/// A single hook command that exited non-zero (or whose shell failed to launch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookFailure {
    /// 1-based position of the failing command in the hook's `commands` array.
    pub position: usize,
    /// The command script that failed.
    pub command: String,
    /// Exit code, if the process exited normally.
    pub code: Option<i32>,
    /// Captured combined stdout+stderr (may be truncated when surfaced).
    pub output: String,
}

/// Result of running one hook's commands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookReport {
    /// How many commands ran (including the one that failed, if any).
    pub ran: usize,
    /// The first failing command, if any. Subsequent commands were skipped.
    pub failure: Option<HookFailure>,
}

impl HookReport {
    /// A one-line, human-readable summary suitable for a toast, or `None` when
    /// nothing ran or everything succeeded.
    pub fn warning_message(&self, hook_name: &str) -> Option<String> {
        let f = self.failure.as_ref()?;
        let code = f
            .code
            .map(|c| format!("exit {c}"))
            .unwrap_or_else(|| "terminated".to_string());
        // Keep the toast compact; the failing command identifies the problem.
        let snippet = first_line(&f.command);
        Some(format!(
            "Hook '{hook_name}' command {}/{} failed ({code}): {snippet}",
            f.position, self.ran
        ))
    }
}

/// Run `commands` sequentially through `runner` in `cwd`, stopping at the first
/// command that exits non-zero (so a hook behaves like a `set -e` script). A
/// command whose shell fails to launch is treated as a failure too. Returns a
/// [`HookReport`]; the caller decides how to surface a failure.
pub fn run_commands(runner: &dyn CommandRunner, commands: &[String], cwd: &Path) -> HookReport {
    let mut report = HookReport::default();
    for (i, command) in commands.iter().enumerate() {
        // Skip blank entries so a trailing "" in the array is a harmless no-op.
        if command.trim().is_empty() {
            continue;
        }
        report.ran += 1;
        match runner.run_shell(command, cwd) {
            Ok(outcome) if outcome.success => {}
            Ok(outcome) => {
                report.failure = Some(HookFailure {
                    position: i + 1,
                    command: command.clone(),
                    code: outcome.code,
                    output: outcome.output,
                });
                break;
            }
            Err(e) => {
                report.failure = Some(HookFailure {
                    position: i + 1,
                    command: command.clone(),
                    code: None,
                    output: e.to_string(),
                });
                break;
            }
        }
    }
    report
}

/// First non-empty line of a (possibly multi-line) command, for compact display.
fn first_line(command: &str) -> String {
    command
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::CommandOutcome;
    use crate::testing::{FakeCommandRunner, FakeFs};

    #[test]
    fn parse_empty_is_no_hooks() {
        let hooks = parse_hooks("").unwrap();
        assert!(hooks.is_empty());
    }

    #[test]
    fn parse_both_sections() {
        let toml = r#"
[worktree_created]
commands = ["npm install", "cp .env.example .env"]

[worktree_update]
commands = ["npm install"]
"#;
        let hooks = parse_hooks(toml).unwrap();
        assert_eq!(
            hooks.worktree_created,
            vec![
                "npm install".to_string(),
                "cp .env.example .env".to_string()
            ]
        );
        assert_eq!(hooks.worktree_update, vec!["npm install".to_string()]);
    }

    #[test]
    fn parse_multiline_triple_quoted_command() {
        // A triple-quoted TOML string is one command spanning multiple lines.
        let toml =
            "[worktree_created]\ncommands = [\n\"\"\"\nnpm install\nnpm run build\n\"\"\",\n]\n";
        let hooks = parse_hooks(toml).unwrap();
        assert_eq!(hooks.worktree_created.len(), 1);
        assert!(hooks.worktree_created[0].contains("npm install"));
        assert!(hooks.worktree_created[0].contains("npm run build"));
    }

    #[test]
    fn parse_tolerates_unknown_future_hook() {
        // A hook this version doesn't know about must not break parsing.
        let toml = r#"
[worktree_created]
commands = ["echo hi"]

[some_future_hook]
commands = ["echo future"]
"#;
        let hooks = parse_hooks(toml).unwrap();
        assert_eq!(hooks.worktree_created, vec!["echo hi".to_string()]);
    }

    #[test]
    fn parse_rejects_invalid_toml() {
        assert!(parse_hooks("not = valid = toml").is_err());
    }

    #[test]
    fn default_template_parses_to_empty_hooks() {
        let hooks = parse_hooks(DEFAULT_HOOKS_TEMPLATE).unwrap();
        assert!(hooks.is_empty(), "shipped template must define no commands");
    }

    #[test]
    fn load_missing_file_is_empty() {
        let fs = FakeFs::new();
        assert!(load_hooks(&fs, Path::new("/repo")).is_empty());
    }

    #[test]
    fn load_reads_and_parses() {
        let fs = FakeFs::new().with_file(
            "/repo/.flightdeck/hooks.toml",
            "[worktree_created]\ncommands = [\"echo hi\"]\n",
        );
        let hooks = load_hooks(&fs, Path::new("/repo"));
        assert_eq!(hooks.worktree_created, vec!["echo hi".to_string()]);
    }

    #[test]
    fn load_unparsable_file_is_empty() {
        let fs = FakeFs::new().with_file("/repo/.flightdeck/hooks.toml", "]]] not toml");
        assert!(load_hooks(&fs, Path::new("/repo")).is_empty());
    }

    #[test]
    fn run_commands_runs_all_in_order_on_success() {
        let runner = FakeCommandRunner::new();
        let cmds = vec!["a".to_string(), "b".to_string()];
        let report = run_commands(&runner, &cmds, Path::new("/wt"));
        assert_eq!(report.ran, 2);
        assert!(report.failure.is_none());
        assert_eq!(
            runner.invocations(),
            vec![
                ("a".to_string(), "/wt".to_string()),
                ("b".to_string(), "/wt".to_string()),
            ]
        );
    }

    #[test]
    fn run_commands_stops_at_first_failure() {
        let runner = FakeCommandRunner::new();
        runner.set_result(
            "b",
            CommandOutcome {
                success: false,
                code: Some(2),
                output: "boom".to_string(),
            },
        );
        let cmds = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let report = run_commands(&runner, &cmds, Path::new("/wt"));
        assert_eq!(report.ran, 2, "c must be skipped after b fails");
        let failure = report.failure.expect("b should have failed");
        assert_eq!(failure.position, 2);
        assert_eq!(failure.command, "b");
        assert_eq!(failure.code, Some(2));
        // c was never invoked.
        assert_eq!(runner.invocations().len(), 2);
    }

    #[test]
    fn run_commands_skips_blank_entries() {
        let runner = FakeCommandRunner::new();
        let cmds = vec!["".to_string(), "  ".to_string(), "real".to_string()];
        let report = run_commands(&runner, &cmds, Path::new("/wt"));
        assert_eq!(report.ran, 1);
        assert_eq!(
            runner.invocations(),
            vec![("real".to_string(), "/wt".to_string())]
        );
    }

    #[test]
    fn warning_message_summarizes_failure() {
        let runner = FakeCommandRunner::new();
        runner.set_result(
            "npm install\nnpm run build",
            CommandOutcome {
                success: false,
                code: Some(1),
                output: String::new(),
            },
        );
        let cmds = vec!["npm install\nnpm run build".to_string()];
        let report = run_commands(&runner, &cmds, Path::new("/wt"));
        let msg = report.warning_message("worktree_created").unwrap();
        assert!(msg.contains("worktree_created"));
        assert!(msg.contains("exit 1"));
        assert!(msg.contains("npm install"), "shows the first line: {msg}");
    }
}
