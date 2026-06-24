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
        let _ = config;
        todo!("T4: collect agents, set default_key from ui.default_agent")
    }

    /// Look up an agent by key.
    pub fn get(&self, key: &str) -> Option<&AgentDef> {
        let _ = key;
        todo!("T4")
    }

    /// The default agent (SPECS §4 — defaults to OpenCode).
    pub fn default_agent(&self) -> Option<&AgentDef> {
        todo!("T4")
    }

    /// All agents in stable order.
    pub fn all(&self) -> Vec<&AgentDef> {
        todo!("T4")
    }
}
