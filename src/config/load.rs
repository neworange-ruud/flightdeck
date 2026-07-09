//! Loading and serializing `config.toml` (SPECS §8).

use crate::contracts::{Config, FileSystem, FlightDeckError, Result};
use std::path::Path;

/// Parse config from a TOML string, populating each [`crate::contracts::AgentDef::key`]
/// from its table key.
pub fn parse_config(toml_str: &str) -> Result<Config> {
    let mut config: Config = toml::from_str(toml_str)
        .map_err(|e| FlightDeckError::Config(format!("failed to parse config.toml: {e}")))?;

    // Populate the `key` field from the map key (key is #[serde(skip)] so it's
    // not stored inside the table body).
    for (key, agent) in config.agents.iter_mut() {
        agent.key = key.clone();
    }

    Ok(config)
}

/// Serialize a config back to a human-readable TOML string (SPECS §8).
pub fn serialize_config(config: &Config) -> Result<String> {
    toml::to_string_pretty(config)
        .map_err(|e| FlightDeckError::Config(format!("failed to serialize config: {e}")))
}

/// Load and parse the config at `path` via the filesystem abstraction.
pub fn load_config(fs: &dyn FileSystem, path: &Path) -> Result<Config> {
    let contents = fs.read_to_string(path)?;
    let config = parse_config(&contents)?;
    crate::config::schema::validate(&config)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::default_config;
    use crate::testing::FakeFs;
    use std::path::Path;

    #[test]
    fn parse_config_populates_agent_keys() {
        let cfg = default_config("proj", "main");
        let toml_str = serialize_config(&cfg).unwrap();
        let parsed = parse_config(&toml_str).unwrap();
        // Keys must be populated from the map entry name
        assert_eq!(parsed.agents.get("opencode").unwrap().key, "opencode");
        assert_eq!(parsed.agents.get("claude").unwrap().key, "claude");
        assert_eq!(parsed.agents.get("codex").unwrap().key, "codex");
    }

    #[test]
    fn serialize_then_parse_round_trip() {
        let original = default_config("round-trip", "develop");
        let toml_str = serialize_config(&original).unwrap();
        let parsed = parse_config(&toml_str).unwrap();

        assert_eq!(parsed.project.name, original.project.name);
        assert_eq!(
            parsed.project.default_base_branch,
            original.project.default_base_branch
        );
        assert_eq!(parsed.ui.default_agent, original.ui.default_agent);
        assert_eq!(parsed.agents.len(), original.agents.len());

        // Verify opencode status patterns survived the round-trip
        let opencode = parsed.agents.get("opencode").unwrap();
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
    fn parse_config_rejects_invalid_toml() {
        let err = parse_config("not valid toml ][[[").unwrap_err();
        assert!(err.to_string().contains("config error"));
    }

    #[test]
    fn parse_config_defaults_update_check_to_true() {
        let cfg = parse_config(
            r#"
[project]
name = "proj"
default_base_branch = "main"
"#,
        )
        .unwrap();

        assert!(cfg.update.check);
    }

    #[test]
    fn load_config_reads_from_fakefs() {
        let cfg = default_config("fakefs-proj", "main");
        let toml_str = serialize_config(&cfg).unwrap();
        let fs = FakeFs::new().with_file("/repo/.flightdeck/config.toml", toml_str);
        let loaded = load_config(&fs, Path::new("/repo/.flightdeck/config.toml")).unwrap();
        assert_eq!(loaded.project.name, "fakefs-proj");
        assert_eq!(loaded.agents.len(), 3);
    }

    #[test]
    fn load_config_propagates_missing_file_error() {
        let fs = FakeFs::new();
        let err = load_config(&fs, Path::new("/repo/.flightdeck/config.toml")).unwrap_err();
        // FakeFs returns Io error for missing files
        assert!(err.to_string().contains("io error") || err.to_string().contains("no such file"));
    }

    #[test]
    fn load_config_validates_after_parse() {
        // Seed an invalid config (empty agents section)
        let toml_str = r#"
[project]
name = "bad"
default_base_branch = "main"

[worktrees]
root = ".flightdeck/worktrees"

[git]
default_remote = "origin"
primary_host = "github"
branch_prefix = "flightdeck/"

[ui]
agent_tab_position = "left"
default_agent = "opencode"
"#;
        let fs = FakeFs::new().with_file("/repo/.flightdeck/config.toml", toml_str);
        let err = load_config(&fs, Path::new("/repo/.flightdeck/config.toml")).unwrap_err();
        assert!(err.to_string().contains("config error"));
    }
}
