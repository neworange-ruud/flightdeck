//! Loading and serializing `config.toml` (SPECS §8).
//!
//! FlightDeck layers two files into one effective [`Config`] (SPECS §8):
//! a per-user **global** base (`~/.flightdeck/config.toml`, every setting
//! present so it is discoverable) and a per-project **override**
//! (`<repo>/.flightdeck/config.toml`, only the values a project changes). The
//! project layer wins field-by-field; the `[agents]` map is the one exception —
//! it is replaced wholesale when the project defines any agents (SPECS §8), so a
//! project either inherits the global agent set or specifies its own in full.

use crate::contracts::{
    AgentDef, Config, ContainersConfig, GitConfig, NotificationsConfig, RemoteConfig, UiConfig,
    UpdateConfig, WorktreesConfig,
};
use crate::contracts::{FileSystem, FlightDeckError, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The per-user global config path, `~/.flightdeck/config.toml` (alongside the
/// workspace file). Returns `None` when neither `$HOME` nor `%USERPROFILE%` is
/// set, so the caller simply skips the global layer rather than failing.
pub fn global_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".flightdeck").join("config.toml"))
}

/// Header prepended to a freshly-written global `config.toml` so the file
/// explains itself as the documented, overridable base (SPECS §8).
pub const GLOBAL_CONFIG_HEADER: &str = "\
# FlightDeck global configuration (~/.flightdeck/config.toml).
#
# This file is the base for every project. Each project may override any of
# these values in its own <repo>/.flightdeck/config.toml — a project only needs
# to store the values it changes. Every setting is listed here so you can see
# what is available to override. Project identity (project name and
# default_base_branch) lives per-repo and is intentionally absent here.

";

/// Header prepended to a freshly-written project `config.toml`. Only project
/// identity is written on first run; everything else is inherited from the
/// global config until explicitly overridden (SPECS §8).
pub const PROJECT_CONFIG_HEADER: &str = "\
# FlightDeck project configuration (<repo>/.flightdeck/config.toml).
#
# Only values that differ from the global config (~/.flightdeck/config.toml)
# need to live here. Anything omitted is inherited from the global base.

";

/// Parse config from a TOML string, populating each [`crate::contracts::AgentDef::key`]
/// from its table key.
pub fn parse_config(toml_str: &str) -> Result<Config> {
    let mut config: Config = toml::from_str(toml_str)
        .map_err(|e| FlightDeckError::Config(format!("failed to parse config.toml: {e}")))?;

    populate_agent_keys(&mut config);
    Ok(config)
}

/// Populate each agent's `key` from its map entry (the `key` field is
/// `#[serde(skip)]`, so it is not carried in the table body).
fn populate_agent_keys(config: &mut Config) {
    for (key, agent) in config.agents.iter_mut() {
        agent.key = key.clone();
    }
}

/// Serialize a config back to a human-readable TOML string (SPECS §8).
pub fn serialize_config(config: &Config) -> Result<String> {
    toml::to_string_pretty(config)
        .map_err(|e| FlightDeckError::Config(format!("failed to serialize config: {e}")))
}

/// A serialize-only projection of [`Config`] that omits `[project]` (project
/// identity is per-repo, never part of the shared global base). Every field is
/// a table so the emit order among them is irrelevant for TOML validity.
#[derive(Serialize)]
struct GlobalConfigView<'a> {
    worktrees: &'a WorktreesConfig,
    git: &'a GitConfig,
    ui: &'a UiConfig,
    notifications: &'a NotificationsConfig,
    update: &'a UpdateConfig,
    remote: &'a RemoteConfig,
    containers: &'a ContainersConfig,
    agents: &'a BTreeMap<String, AgentDef>,
}

/// The explanatory comment injected above the `[remote]` section of any global
/// `config.toml` we write. It documents that FlightDeck Remote is off by default
/// and — importantly — that the default relay is **not** open to the public.
const REMOTE_SECTION_COMMENT: &str = "\
# FlightDeck Remote (optional phone <-> desktop link). Off by default.
#
# NOTE: the default relay URL below (relay.flightdeckai.app) is currently
# RESTRICTED and is NOT accessible to the public — enabling remote against it
# will not connect. You may point relay_url at a relay you host yourself, but
# self-hosting is not supported by the author in any way. See the docs for
# details: https://flightdeckai.app/remote
";

/// Insert [`REMOTE_SECTION_COMMENT`] directly above the `[remote]` table header
/// in a serialized TOML body. The `toml` crate drops comments on serialization,
/// so we re-attach this one every time we write the global file. A no-op if the
/// body has no `[remote]` section.
fn annotate_remote_section(body: &str) -> String {
    match body.lines().position(|l| l.trim() == "[remote]") {
        Some(idx) => {
            let mut out = String::with_capacity(body.len() + REMOTE_SECTION_COMMENT.len());
            for (i, line) in body.lines().enumerate() {
                if i == idx {
                    out.push_str(REMOTE_SECTION_COMMENT);
                }
                out.push_str(line);
                out.push('\n');
            }
            out
        }
        None => body.to_string(),
    }
}

/// Serialize the global base config (all sections except `[project]`) with the
/// explanatory [`GLOBAL_CONFIG_HEADER`] (SPECS §8).
pub fn serialize_global_config(config: &Config) -> Result<String> {
    let view = GlobalConfigView {
        worktrees: &config.worktrees,
        git: &config.git,
        ui: &config.ui,
        notifications: &config.notifications,
        update: &config.update,
        remote: &config.remote,
        containers: &config.containers,
        agents: &config.agents,
    };
    let body = toml::to_string_pretty(&view)
        .map_err(|e| FlightDeckError::Config(format!("failed to serialize global config: {e}")))?;
    Ok(format!(
        "{GLOBAL_CONFIG_HEADER}{}",
        annotate_remote_section(&body)
    ))
}

/// The minimal initial project `config.toml`: only project identity, with an
/// explanatory header (SPECS §8). Everything else is inherited from the global
/// base until explicitly overridden.
pub fn minimal_project_config(name: &str, base_branch: &str) -> String {
    // `toml` string escaping is unnecessary for the derived project name (a
    // directory basename) and branch, but quoting keeps it valid for names with
    // spaces or punctuation.
    let name = name.replace('\\', "\\\\").replace('"', "\\\"");
    let base = base_branch.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "{PROJECT_CONFIG_HEADER}[project]\nname = \"{name}\"\ndefault_base_branch = \"{base}\"\n"
    )
}

/// Serialize a raw global table with the explanatory [`GLOBAL_CONFIG_HEADER`]
/// (SPECS §8). Used when the configuration manager writes edited global values.
pub fn serialize_global_table(table: &toml::Table) -> Result<String> {
    let body = toml::to_string_pretty(table)
        .map_err(|e| FlightDeckError::Config(format!("failed to serialize global config: {e}")))?;
    Ok(format!(
        "{GLOBAL_CONFIG_HEADER}{}",
        annotate_remote_section(&body)
    ))
}

/// Serialize a raw project override table with the [`PROJECT_CONFIG_HEADER`]
/// (SPECS §8). Only the overridden keys the table holds are written.
pub fn serialize_project_table(table: &toml::Table) -> Result<String> {
    let body = toml::to_string_pretty(table)
        .map_err(|e| FlightDeckError::Config(format!("failed to serialize project config: {e}")))?;
    Ok(format!("{PROJECT_CONFIG_HEADER}{body}"))
}

/// Parse a TOML string into a raw table (for layering). Empty input is a valid
/// empty table.
pub fn parse_table(toml_str: &str) -> Result<toml::Table> {
    toml_str
        .parse::<toml::Table>()
        .map_err(|e| FlightDeckError::Config(format!("failed to parse config.toml: {e}")))
}

/// Deep-merge `over` onto `base` in place (SPECS §8). Scalars and arrays in
/// `over` replace their counterparts in `base`; sub-tables merge recursively —
/// except the top-level `agents` table, which is replaced wholesale so a project
/// either inherits the global agents or defines its own set in full.
fn merge_into(base: &mut toml::Table, over: toml::Table, top_level: bool) {
    for (key, over_val) in over {
        let replace_whole = top_level && key == "agents";
        match base.get_mut(&key) {
            Some(toml::Value::Table(base_tbl)) if !replace_whole && over_val.is_table() => {
                if let toml::Value::Table(over_tbl) = over_val {
                    merge_into(base_tbl, over_tbl, false);
                }
            }
            _ => {
                base.insert(key, over_val);
            }
        }
    }
}

/// Merge a `global` base table with a `project` override table and deserialize
/// the result into a validated effective [`Config`] (SPECS §8). Either table may
/// be empty (a missing file layers as no-op).
pub fn effective_config(global: toml::Table, project: toml::Table) -> Result<Config> {
    let mut merged = global;
    merge_into(&mut merged, project, true);

    let value = toml::Value::Table(merged);
    let mut config: Config = value
        .try_into()
        .map_err(|e| FlightDeckError::Config(format!("failed to parse config.toml: {e}")))?;
    populate_agent_keys(&mut config);
    crate::config::schema::validate(&config)?;
    Ok(config)
}

/// Read a config file into a raw table. A missing file layers as an empty table.
/// When `lenient`, an unparsable file is also treated as empty (used for the
/// global base so a corrupt user-level file never blocks a project's own
/// config); otherwise a parse error propagates.
fn read_table(fs: &dyn FileSystem, path: &Path, lenient: bool) -> Result<toml::Table> {
    if !fs.exists(path) {
        return Ok(toml::Table::new());
    }
    let contents = fs.read_to_string(path)?;
    match parse_table(&contents) {
        Ok(t) => Ok(t),
        Err(e) if lenient => {
            eprintln!(
                "FlightDeck: ignoring unparsable global config {}: {e}",
                path.display()
            );
            Ok(toml::Table::new())
        }
        Err(e) => Err(e),
    }
}

/// Load the effective config by layering the global base (`global_path`) under
/// the project override (`project_path`) (SPECS §8). A missing global/project
/// file layers as empty; an unparsable global is ignored (best-effort base) while
/// an unparsable project file is a hard error.
pub fn load_layered_config(
    fs: &dyn FileSystem,
    global_path: &Path,
    project_path: &Path,
) -> Result<Config> {
    let global = read_table(fs, global_path, true)?;
    let project = read_table(fs, project_path, false)?;
    effective_config(global, project)
}

/// Load and parse the config at `path` via the filesystem abstraction. This
/// reads a single fully-populated file (no layering) and is retained for
/// callers that operate on one already-complete config.
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
    use crate::contracts::StatusPatterns;
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
        let mut original = default_config("round-trip", "develop");
        // Deprecated patterns are no longer generated, but existing configs
        // must continue to deserialize and round-trip unchanged.
        original.agents.get_mut("opencode").unwrap().status_patterns = StatusPatterns {
            waiting: vec!["Proceed?".to_string()],
            completed: vec!["Done".to_string()],
            error: vec!["Error".to_string()],
        };
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
    fn global_config_documents_remote_section_and_restriction() {
        let toml = serialize_global_config(&default_config("x", "main")).unwrap();
        // The [remote] section is present, off by default, with the default relay.
        assert!(toml.contains("[remote]"), "global: {toml}");
        assert!(toml.contains("enabled = false"), "global: {toml}");
        assert!(toml.contains("relay.flightdeckai.app"), "global: {toml}");
        // The restriction note is attached as a comment above the section.
        assert!(
            toml.contains("RESTRICTED") && toml.contains("NOT accessible to the public"),
            "global: {toml}"
        );
        // It still parses back into a valid config (comment is inert).
        let cfg = parse_config(&toml).unwrap();
        assert!(!cfg.remote.enabled);
        assert_eq!(cfg.remote.relay_url, "wss://relay.flightdeckai.app/ws");
    }

    #[test]
    fn global_table_save_reattaches_remote_comment() {
        // Simulate the config manager saving a global table that includes [remote].
        let table: toml::Table = "[remote]\nenabled = true\nrelay_url = \"wss://x/ws\"\n"
            .parse()
            .unwrap();
        let out = serialize_global_table(&table).unwrap();
        assert!(out.contains("RESTRICTED"), "out: {out}");
        // The comment sits directly above the section header.
        let comment_idx = out.find("# FlightDeck Remote").unwrap();
        let header_idx = out.find("[remote]").unwrap();
        assert!(comment_idx < header_idx, "comment must precede header");
    }

    #[test]
    fn default_config_omits_deprecated_status_patterns() {
        let serialized = serialize_config(&default_config("proj", "main")).unwrap();
        assert!(!serialized.contains("status_patterns"));
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
    fn existing_ui_config_defaults_f2_leave_focus_to_false() {
        let cfg = parse_config(
            r#"
[ui]
agent_tab_position = "left"
default_agent = "opencode"
"#,
        )
        .unwrap();

        assert!(!cfg.ui.use_f2_to_leave_terminal_focus);
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

    // --- Layered config (SPECS §8) ---

    /// The documented global base, as written to `~/.flightdeck/config.toml`.
    fn global_base() -> toml::Table {
        parse_table(&serialize_global_config(&default_config("x", "main")).unwrap()).unwrap()
    }

    #[test]
    fn project_scalar_overrides_global() {
        let global = global_base();
        let project = "[notifications]\nenabled = false\n".parse().unwrap();
        let cfg = effective_config(global, project).unwrap();
        // Project turned notifications off; the rest of [notifications] stays as
        // the global default (deep merge, not whole-table replace).
        assert!(!cfg.notifications.enabled);
        assert!(cfg.notifications.sound);
        assert!(cfg.notifications.on_finish);
    }

    #[test]
    fn missing_project_inherits_global_wholesale() {
        let cfg = effective_config(global_base(), toml::Table::new()).unwrap();
        assert_eq!(cfg.agents.len(), 3);
        assert_eq!(cfg.ui.default_agent, "opencode");
        assert!(cfg.notifications.enabled);
    }

    #[test]
    fn project_agents_replace_global_wholesale() {
        let global = global_base();
        // A project that defines its own single agent replaces the global set of
        // three entirely (whole-map replace), and points default_agent at it.
        let project = "\
[ui]
default_agent = \"mytool\"

[agents.mytool]
display_name = \"My Tool\"
command = \"mytool\"
"
        .parse()
        .unwrap();
        let cfg = effective_config(global, project).unwrap();
        assert_eq!(cfg.agents.len(), 1);
        assert!(cfg.agents.contains_key("mytool"));
        assert!(!cfg.agents.contains_key("opencode"));
        assert_eq!(cfg.agents.get("mytool").unwrap().key, "mytool");
    }

    #[test]
    fn load_layered_config_merges_files() {
        let fs = FakeFs::new()
            .with_file(
                "/home/u/.flightdeck/config.toml",
                serialize_global_config(&default_config("x", "main")).unwrap(),
            )
            .with_file(
                "/repo/.flightdeck/config.toml",
                minimal_project_config("my-repo", "develop")
                    + "\n[ui]\nagent_tab_position = \"right\"\n",
            );
        let cfg = load_layered_config(
            &fs,
            Path::new("/home/u/.flightdeck/config.toml"),
            Path::new("/repo/.flightdeck/config.toml"),
        )
        .unwrap();
        // Project identity comes from the project file...
        assert_eq!(cfg.project.name, "my-repo");
        assert_eq!(cfg.project.default_base_branch, "develop");
        // ...the ui override applies...
        assert_eq!(cfg.ui.agent_tab_position, "right");
        // ...and everything else is inherited from the global base.
        assert_eq!(cfg.agents.len(), 3);
        assert!(cfg.notifications.enabled);
    }

    #[test]
    fn load_layered_config_tolerates_corrupt_global() {
        let fs = FakeFs::new()
            .with_file("/home/u/.flightdeck/config.toml", "this is ][ not toml")
            .with_file(
                "/repo/.flightdeck/config.toml",
                serialize_config(&default_config("p", "main")).unwrap(),
            );
        // A corrupt global is ignored; the (self-sufficient) project config loads.
        let cfg = load_layered_config(
            &fs,
            Path::new("/home/u/.flightdeck/config.toml"),
            Path::new("/repo/.flightdeck/config.toml"),
        )
        .unwrap();
        assert_eq!(cfg.project.name, "p");
        assert_eq!(cfg.agents.len(), 3);
    }

    #[test]
    fn effective_config_rejects_when_no_agents_anywhere() {
        // Neither layer supplies agents → validation fails (empty agents map).
        let project = minimal_project_config("p", "main").parse().unwrap();
        assert!(effective_config(toml::Table::new(), project).is_err());
    }

    #[test]
    fn global_serialization_round_trips_through_layering() {
        // The generated global base parses, and layered under an empty project
        // yields a valid config equal to the defaults (minus project identity).
        let cfg = effective_config(global_base(), toml::Table::new()).unwrap();
        let defaults = default_config("project", "main");
        assert_eq!(cfg.notifications, defaults.notifications);
        assert_eq!(cfg.containers, defaults.containers);
        assert_eq!(cfg.git, defaults.git);
        assert_eq!(cfg.agents, defaults.agents);
    }
}
