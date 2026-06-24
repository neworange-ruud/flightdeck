//! Loading and serializing `config.toml` (SPECS §8).

use crate::contracts::{Config, FileSystem, Result};
use std::path::Path;

/// Parse config from a TOML string, populating each [`crate::contracts::AgentDef::key`]
/// from its table key.
pub fn parse_config(toml_str: &str) -> Result<Config> {
    let _ = toml_str;
    todo!("T1: parse TOML into Config and fill agent keys")
}

/// Serialize a config back to a human-readable TOML string (SPECS §8).
pub fn serialize_config(config: &Config) -> Result<String> {
    let _ = config;
    todo!("T1: serialize Config to TOML")
}

/// Load and parse the config at `path` via the filesystem abstraction.
pub fn load_config(fs: &dyn FileSystem, path: &Path) -> Result<Config> {
    let _ = (fs, path);
    todo!("T1: read file via fs, parse_config, validate")
}
