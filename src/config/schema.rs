//! Default config construction and validation (SPECS §8).

use crate::contracts::{
    AgentDef, Config, FlightDeckError, GitConfig, NotificationsConfig, ProjectConfig, Result,
    StatusPatterns, UiConfig, UpdateConfig, WorktreesConfig,
};
use std::collections::BTreeMap;

/// Build the default `config.toml` contents for a project (SPECS §8), including
/// the three initial agents (OpenCode default, Claude Code, Codex CLI).
pub fn default_config(project_name: &str, base_branch: &str) -> Config {
    let mut agents: BTreeMap<String, AgentDef> = BTreeMap::new();

    // opencode — default agent with status patterns (SPECS §8 example)
    agents.insert(
        "opencode".to_string(),
        AgentDef {
            key: "opencode".to_string(),
            display_name: "OpenCode".to_string(),
            command: "opencode".to_string(),
            args: vec![],
            status_patterns: StatusPatterns {
                waiting: vec![
                    "Proceed?".to_string(),
                    "Confirm".to_string(),
                    "Approve".to_string(),
                    "Do you want to".to_string(),
                ],
                completed: vec![
                    "Done".to_string(),
                    "Complete".to_string(),
                    "Task complete".to_string(),
                ],
                error: vec!["Error".to_string(), "Failed".to_string()],
            },
        },
    );

    // claude
    agents.insert(
        "claude".to_string(),
        AgentDef {
            key: "claude".to_string(),
            display_name: "Claude Code".to_string(),
            command: "claude".to_string(),
            args: vec![],
            status_patterns: StatusPatterns::default(),
        },
    );

    // codex
    agents.insert(
        "codex".to_string(),
        AgentDef {
            key: "codex".to_string(),
            display_name: "Codex CLI".to_string(),
            command: "codex".to_string(),
            args: vec![],
            status_patterns: StatusPatterns::default(),
        },
    );

    Config {
        project: ProjectConfig {
            name: project_name.to_string(),
            default_base_branch: base_branch.to_string(),
        },
        worktrees: WorktreesConfig::default(),
        git: GitConfig::default(),
        ui: UiConfig {
            agent_tab_position: "left".to_string(),
            default_agent: "opencode".to_string(),
        },
        notifications: NotificationsConfig::default(),
        update: UpdateConfig::default(),
        agents,
    }
}

/// Validate a parsed config, rejecting structurally invalid configs with clear
/// errors (SPECS §8, §26 "Rejects invalid config").
pub fn validate(config: &Config) -> Result<()> {
    if config.agents.is_empty() {
        return Err(FlightDeckError::Config(
            "agents map must not be empty".to_string(),
        ));
    }

    if !config.agents.contains_key(&config.ui.default_agent) {
        return Err(FlightDeckError::Config(format!(
            "ui.default_agent '{}' is not present in the agents map",
            config.ui.default_agent
        )));
    }

    for (key, agent) in &config.agents {
        if agent.command.is_empty() {
            return Err(FlightDeckError::Config(format!(
                "agent '{}' has an empty command",
                key
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_three_agents() {
        let cfg = default_config("my-project", "main");
        assert_eq!(cfg.agents.len(), 3);
        assert!(cfg.agents.contains_key("opencode"));
        assert!(cfg.agents.contains_key("claude"));
        assert!(cfg.agents.contains_key("codex"));
    }

    #[test]
    fn default_config_default_agent_is_opencode() {
        let cfg = default_config("my-project", "main");
        assert_eq!(cfg.ui.default_agent, "opencode");
    }

    #[test]
    fn default_config_opencode_has_status_patterns() {
        let cfg = default_config("my-project", "main");
        let opencode = cfg.agents.get("opencode").unwrap();
        assert_eq!(opencode.display_name, "OpenCode");
        assert_eq!(opencode.command, "opencode");
        assert!(!opencode.status_patterns.waiting.is_empty());
        assert!(opencode
            .status_patterns
            .waiting
            .contains(&"Proceed?".to_string()));
        assert!(opencode
            .status_patterns
            .completed
            .contains(&"Done".to_string()));
        assert!(opencode
            .status_patterns
            .error
            .contains(&"Error".to_string()));
    }

    #[test]
    fn default_config_agent_keys_populated() {
        let cfg = default_config("proj", "main");
        assert_eq!(cfg.agents.get("opencode").unwrap().key, "opencode");
        assert_eq!(cfg.agents.get("claude").unwrap().key, "claude");
        assert_eq!(cfg.agents.get("codex").unwrap().key, "codex");
    }

    #[test]
    fn default_config_project_fields() {
        let cfg = default_config("my-project", "develop");
        assert_eq!(cfg.project.name, "my-project");
        assert_eq!(cfg.project.default_base_branch, "develop");
    }

    #[test]
    fn validate_accepts_valid_config() {
        let cfg = default_config("proj", "main");
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn validate_rejects_empty_agents() {
        let mut cfg = default_config("proj", "main");
        cfg.agents.clear();
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn validate_rejects_missing_default_agent() {
        let mut cfg = default_config("proj", "main");
        cfg.ui.default_agent = "nonexistent".to_string();
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn validate_rejects_empty_command() {
        let mut cfg = default_config("proj", "main");
        cfg.agents.get_mut("claude").unwrap().command = "".to_string();
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("claude"));
    }
}
