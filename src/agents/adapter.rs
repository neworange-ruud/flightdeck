//! Agent command validation and launch-command building (SPECS §16, §17).

use crate::contracts::{AgentDef, Result};
use std::path::{Path, PathBuf};

/// Whether `command` resolves on a given `PATH` string (pure, testable form).
/// An absolute/relative path is checked directly; a bare name is searched in
/// each `PATH` entry (SPECS §16).
pub fn command_in_path(command: &str, path_var: &str) -> bool {
    let _ = (command, path_var);
    todo!("T4: resolve command against PATH entries")
}

/// Whether `command` exists on the process `PATH` (uses the real environment).
pub fn command_exists(command: &str) -> bool {
    let _ = command;
    todo!("T4: read std::env PATH and delegate to command_in_path")
}

/// Validate that an agent's command exists, before any git mutation (SPECS §16).
/// Returns [`crate::contracts::FlightDeckError::AgentMissing`] if not found.
pub fn validate_agent(agent: &AgentDef) -> Result<()> {
    let _ = agent;
    todo!("T4: command_exists check -> AgentMissing on failure")
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
    let _ = (agent, cwd);
    todo!("T4: build LaunchSpec from agent.command/args and cwd; no initial prompt")
}
