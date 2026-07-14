//! Agent registry built from config (SPECS §8).

use crate::contracts::{AgentDef, Config};
use std::collections::BTreeMap;

/// The set of configured agents plus the configured default (SPECS §8).
#[derive(Debug, Clone, Default)]
pub struct AgentRegistry {
    pub agents: BTreeMap<String, AgentDef>,
    pub default_key: String,
}

impl AgentRegistry {
    /// Build the registry from a parsed config (SPECS §8).
    pub fn from_config(config: &Config) -> Self {
        let mut agents: BTreeMap<String, AgentDef> = BTreeMap::new();
        for (key, def) in &config.agents {
            let mut agent = def.clone();
            agent.key = key.clone();
            agents.insert(key.clone(), agent);
        }
        AgentRegistry {
            agents,
            default_key: config.ui.default_agent.clone(),
        }
    }

    /// Look up an agent by key.
    pub fn get(&self, key: &str) -> Option<&AgentDef> {
        self.agents.get(key)
    }

    /// The default agent (SPECS §4 — defaults to OpenCode).
    pub fn default_agent(&self) -> Option<&AgentDef> {
        self.agents.get(&self.default_key)
    }

    /// All agents in stable order (BTreeMap iterates in sorted key order).
    pub fn all(&self) -> Vec<&AgentDef> {
        self.agents.values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{AgentDef, Config, UiConfig};

    fn make_config_with_agents() -> Config {
        let mut config = Config {
            ui: UiConfig {
                default_agent: "opencode".to_string(),
                agent_tab_position: "left".to_string(),
                ..UiConfig::default()
            },
            ..Config::default()
        };

        let opencode = AgentDef {
            display_name: "OpenCode".to_string(),
            command: "opencode".to_string(),
            ..AgentDef::default()
        };
        config.agents.insert("opencode".to_string(), opencode);

        let claude = AgentDef {
            display_name: "Claude Code".to_string(),
            command: "claude".to_string(),
            ..AgentDef::default()
        };
        config.agents.insert("claude".to_string(), claude);

        config
    }

    #[test]
    fn from_config_builds_map_and_default() {
        let config = make_config_with_agents();
        let registry = AgentRegistry::from_config(&config);

        assert_eq!(registry.agents.len(), 2);
        assert_eq!(registry.default_key, "opencode");
    }

    #[test]
    fn from_config_sets_key_field_on_each_agent() {
        let config = make_config_with_agents();
        let registry = AgentRegistry::from_config(&config);

        let opencode = registry
            .get("opencode")
            .expect("opencode should be present");
        assert_eq!(opencode.key, "opencode");

        let claude = registry.get("claude").expect("claude should be present");
        assert_eq!(claude.key, "claude");
    }

    #[test]
    fn get_returns_correct_agent() {
        let config = make_config_with_agents();
        let registry = AgentRegistry::from_config(&config);

        let agent = registry.get("opencode").expect("should find opencode");
        assert_eq!(agent.command, "opencode");
        assert_eq!(agent.display_name, "OpenCode");
    }

    #[test]
    fn get_returns_none_for_missing_key() {
        let config = make_config_with_agents();
        let registry = AgentRegistry::from_config(&config);

        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn default_agent_returns_configured_default() {
        let config = make_config_with_agents();
        let registry = AgentRegistry::from_config(&config);

        let default = registry
            .default_agent()
            .expect("default agent should exist");
        assert_eq!(default.key, "opencode");
        assert_eq!(default.command, "opencode");
    }

    #[test]
    fn default_agent_returns_none_when_key_not_in_map() {
        let mut config = make_config_with_agents();
        config.ui.default_agent = "missing_agent".to_string();
        let registry = AgentRegistry::from_config(&config);

        assert!(registry.default_agent().is_none());
    }

    #[test]
    fn all_returns_all_agents_in_sorted_order() {
        let config = make_config_with_agents();
        let registry = AgentRegistry::from_config(&config);

        let all = registry.all();
        assert_eq!(all.len(), 2);
        // BTreeMap guarantees sorted order: "claude" < "opencode"
        assert_eq!(all[0].key, "claude");
        assert_eq!(all[1].key, "opencode");
    }

    #[test]
    fn empty_config_produces_empty_registry() {
        let config = Config::default();
        let registry = AgentRegistry::from_config(&config);

        assert!(registry.agents.is_empty());
        assert!(registry.default_agent().is_none());
        assert!(registry.all().is_empty());
    }
}
