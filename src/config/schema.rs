//! Default config construction and validation (SPECS §8).

use crate::contracts::{Config, Result};

/// Build the default `config.toml` contents for a project (SPECS §8), including
/// the three initial agents (OpenCode default, Claude Code, Codex CLI).
pub fn default_config(project_name: &str, base_branch: &str) -> Config {
    let _ = (project_name, base_branch);
    todo!("T1: build default Config with opencode/claude/codex agents")
}

/// Validate a parsed config, rejecting structurally invalid configs with clear
/// errors (SPECS §8, §26 "Rejects invalid config").
pub fn validate(config: &Config) -> Result<()> {
    let _ = config;
    todo!("T1: validate config (non-empty agents, default_agent exists, etc.)")
}
