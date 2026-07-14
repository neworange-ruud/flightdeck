//! Default config construction and validation (SPECS §8).

use crate::contracts::{
    AgentDef, Config, ContainersConfig, FlightDeckError, GitConfig, NotificationsConfig,
    ProjectConfig, Result, StatusPatterns, UiConfig, UpdateConfig, WorktreesConfig,
};
use std::collections::BTreeMap;

/// Build the default `config.toml` contents for a project (SPECS §8), including
/// the three initial agents (OpenCode default, Claude Code, Codex CLI).
pub fn default_config(project_name: &str, base_branch: &str) -> Config {
    let mut agents: BTreeMap<String, AgentDef> = BTreeMap::new();

    // opencode — default agent
    agents.insert(
        "opencode".to_string(),
        AgentDef {
            key: "opencode".to_string(),
            display_name: "OpenCode".to_string(),
            command: "opencode".to_string(),
            args: vec![],
            status_patterns: StatusPatterns::default(),
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
            ..UiConfig::default()
        },
        notifications: NotificationsConfig::default(),
        update: UpdateConfig::default(),
        containers: ContainersConfig::default(),
        agents,
    }
}

/// Build the default GLOBAL base config: the same defaults as [`default_config`]
/// but with placeholder project identity, since `[project]` is stripped when the
/// global file is written (SPECS §8). Every other section carries the shipping
/// defaults so a fresh `~/.flightdeck/config.toml` documents them all.
pub fn default_global_config() -> Config {
    default_config("project", "main")
}

/// Allowed mode-cue color names (SPECS §23).
const MODE_COLORS: &[&str] = &["green", "cyan", "blue", "magenta", "yellow", "red", "white"];
/// Allowed live-pane border levels (SPECS §23).
const MODE_BORDER_LEVELS: &[&str] = &["off", "dim", "normal", "bright"];

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

    if !MODE_COLORS.contains(&config.ui.terminal_mode_color.as_str()) {
        return Err(FlightDeckError::Config(format!(
            "ui.terminal_mode_color '{}' is not a valid color (expected one of {MODE_COLORS:?})",
            config.ui.terminal_mode_color
        )));
    }
    if !MODE_COLORS.contains(&config.ui.app_mode_color.as_str()) {
        return Err(FlightDeckError::Config(format!(
            "ui.app_mode_color '{}' is not a valid color (expected one of {MODE_COLORS:?})",
            config.ui.app_mode_color
        )));
    }
    if !MODE_BORDER_LEVELS.contains(&config.ui.mode_border.as_str()) {
        return Err(FlightDeckError::Config(format!(
            "ui.mode_border '{}' is not valid (expected one of {MODE_BORDER_LEVELS:?})",
            config.ui.mode_border
        )));
    }

    validate_containers(&config.containers)?;

    Ok(())
}

/// Validate the `[containers]` section (SPECS §31). Only enforced when the
/// section is `enabled`, so a disabled-but-malformed table never blocks startup.
pub fn validate_containers(exec: &crate::contracts::ContainersConfig) -> Result<()> {
    if !exec.enabled {
        return Ok(());
    }
    if exec.runtime != "podman" {
        return Err(FlightDeckError::Config(format!(
            "containers.runtime '{}' is not supported (only 'podman')",
            exec.runtime
        )));
    }
    // Advanced (own Containerfile) is mutually exclusive with declarative
    // customization.
    if exec.containerfile.is_some() && (!exec.packages.is_empty() || exec.setup_script.is_some()) {
        return Err(FlightDeckError::Config(
            "containers.containerfile cannot be combined with packages/setup_script".to_string(),
        ));
    }
    // Ports must be non-zero and unique.
    let mut seen = std::collections::HashSet::new();
    for &port in &exec.forward_ports {
        if port == 0 {
            return Err(FlightDeckError::Config(
                "containers.forward_ports must not contain 0".to_string(),
            ));
        }
        if !seen.insert(port) {
            return Err(FlightDeckError::Config(format!(
                "containers.forward_ports contains duplicate port {port}"
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
    fn default_config_uses_platform_leave_focus_key() {
        let cfg = default_config("my-project", "main");
        assert!(!cfg.ui.use_f2_to_leave_terminal_focus);
    }

    #[test]
    fn default_config_enables_update_check() {
        let cfg = default_config("my-project", "main");
        assert!(cfg.update.check);
    }

    #[test]
    fn default_config_opencode_uses_explicit_lifecycle_status() {
        let cfg = default_config("my-project", "main");
        let opencode = cfg.agents.get("opencode").unwrap();
        assert_eq!(opencode.display_name, "OpenCode");
        assert_eq!(opencode.command, "opencode");
        assert_eq!(opencode.status_patterns, StatusPatterns::default());
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

    // --- [containers] validation (SPECS §31) ---

    #[test]
    fn validate_ignores_disabled_containers() {
        let mut cfg = default_config("proj", "main");
        // Garbage runtime is tolerated while disabled.
        cfg.containers.runtime = "docker".to_string();
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn validate_rejects_unsupported_runtime_when_enabled() {
        let mut cfg = default_config("proj", "main");
        cfg.containers.enabled = true;
        cfg.containers.runtime = "docker".to_string();
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("podman"));
    }

    #[test]
    fn validate_rejects_containerfile_with_packages() {
        let mut cfg = default_config("proj", "main");
        cfg.containers.enabled = true;
        cfg.containers.containerfile = Some("c/Containerfile".to_string());
        cfg.containers.packages = vec!["jq".to_string()];
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn validate_rejects_duplicate_and_zero_ports() {
        let mut cfg = default_config("proj", "main");
        cfg.containers.enabled = true;
        cfg.containers.forward_ports = vec![3000, 3000];
        assert!(validate(&cfg).is_err());
        cfg.containers.forward_ports = vec![0];
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn validate_accepts_valid_containers() {
        let mut cfg = default_config("proj", "main");
        cfg.containers.enabled = true;
        cfg.containers.packages = vec!["jq".to_string(), "curl".to_string()];
        cfg.containers.forward_ports = vec![3000, 8080];
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn ui_config_defaults_for_mode_cues() {
        let cfg = default_config("proj", "main");
        assert_eq!(cfg.ui.terminal_mode_color, "green");
        assert_eq!(cfg.ui.app_mode_color, "cyan");
        assert_eq!(cfg.ui.mode_border, "off");
        assert!(cfg.ui.dim_terminal_in_app_mode);
    }

    #[test]
    fn ui_config_partial_table_fills_mode_defaults() {
        // A config that includes the required fields must still get the new defaults.
        let cfg: Config = "[ui]\nagent_tab_position = \"right\"\ndefault_agent = \"opencode\"\n"
            .parse::<toml::Table>()
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(cfg.ui.agent_tab_position, "right");
        assert_eq!(cfg.ui.default_agent, "opencode");
        assert_eq!(cfg.ui.terminal_mode_color, "green");
        assert_eq!(cfg.ui.mode_border, "off");
        assert!(cfg.ui.dim_terminal_in_app_mode);
    }

    #[test]
    fn validate_rejects_unknown_mode_color() {
        let mut cfg = default_config("proj", "main");
        cfg.ui.terminal_mode_color = "chartreuse".to_string();
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("terminal_mode_color"));
    }

    #[test]
    fn validate_rejects_unknown_border_level() {
        let mut cfg = default_config("proj", "main");
        cfg.ui.mode_border = "flashing".to_string();
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("mode_border"));
    }

    #[test]
    fn validate_accepts_valid_mode_cue_config() {
        let mut cfg = default_config("proj", "main");
        cfg.ui.terminal_mode_color = "magenta".to_string();
        cfg.ui.app_mode_color = "yellow".to_string();
        cfg.ui.mode_border = "bright".to_string();
        assert!(validate(&cfg).is_ok());
    }
}
