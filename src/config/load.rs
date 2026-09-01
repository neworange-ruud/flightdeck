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
    UpdateConfig, WebConfig, WorktreesConfig,
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
    web: &'a WebConfig,
    containers: &'a ContainersConfig,
    agents: &'a BTreeMap<String, AgentDef>,
}

/// The explanatory comment injected above the `[remote]` section of any global
/// `config.toml` we write. It documents that FlightDeck Remote is off by default,
/// that the default relay requires a shared password that is not published, and
/// how to supply that password.
const REMOTE_SECTION_COMMENT: &str = "\
# FlightDeck Remote (optional phone <-> desktop link). Off by default.
#
# NOTE: the default relay URL below (relay.flightdeckai.app) is the author's
# hosted relay. It is reachable from any network but gated by a shared
# `relay_password` that is not published — enabling remote against it without the
# password will not connect. You may point relay_url at a relay you host yourself
# (set its FLIGHTDECK_RELAY_PASSWORD and mirror it in relay_password, or leave
# both empty for an open relay), but self-hosting is not supported by the author
# in any way. See the docs for details: https://flightdeckai.app/remote
#
# relay_password: shared secret presented to the relay on connect (empty = none).
# The FLIGHTDECK_RELAY_PASSWORD environment variable overrides this value.
";

/// The explanatory comment injected above the `[web]` section of any global
/// `config.toml` we write (`specs/WEB_INTERFACE.md` D5, D10, Q2). Documents why
/// each field exists so a user editing this file by hand understands the
/// consequence of changing it, not just its type.
const WEB_SECTION_COMMENT: &str = "\
# FlightDeck Web (embedded browser access to your terminals). Off by default.
#
# enabled: auto-start the web interface on launch. Leave false to start it only
# on demand via the \"Start Web Interface\" command palette action.
# port: TCP port the embedded server listens on. Kept stable across restarts so
# a bookmarked URL / saved QR code keeps working.
# bind: address the server listens on. 127.0.0.1 (loopback) by default, so
# nothing outside this machine can reach it. Binding a routable address (e.g.
# 0.0.0.0 or a LAN IP) is a deliberate opt-in you type yourself, and the app
# warns when it actually does so — see the docs before changing this.
# replay_bytes: per-terminal replay buffer, in bytes, replayed to a browser tab
# that joins or reconnects so it can repaint recent history.
";

/// Insert [`REMOTE_SECTION_COMMENT`] above `[remote]` and [`WEB_SECTION_COMMENT`]
/// above `[web]` in a serialized TOML body. The `toml` crate drops comments on
/// serialization, so we re-attach these every time we write the global file. A
/// no-op for any section the body does not contain.
fn annotate_sections(body: &str) -> String {
    let with_remote = insert_section_comment(body, "[remote]", REMOTE_SECTION_COMMENT);
    insert_section_comment(&with_remote, "[web]", WEB_SECTION_COMMENT)
}

/// Insert `comment` directly above the first line matching `header` (trimmed)
/// in `body`. A no-op if `header` is not found.
fn insert_section_comment(body: &str, header: &str, comment: &str) -> String {
    match body.lines().position(|l| l.trim() == header) {
        Some(idx) => {
            let mut out = String::with_capacity(body.len() + comment.len());
            for (i, line) in body.lines().enumerate() {
                if i == idx {
                    out.push_str(comment);
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
        web: &config.web,
        containers: &config.containers,
        agents: &config.agents,
    };
    let body = toml::to_string_pretty(&view)
        .map_err(|e| FlightDeckError::Config(format!("failed to serialize global config: {e}")))?;
    Ok(format!(
        "{GLOBAL_CONFIG_HEADER}{}",
        annotate_sections(&body)
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
        annotate_sections(&body)
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

/// Update only `[project].default_base_branch` while preserving the rest of the
/// user-authored TOML, including comments and ordering. The configuration
/// manager serializes whole tables, but this focused picker should not rewrite a
/// committed file just to change one string.
pub fn set_project_default_base(contents: &str, branch: &str) -> Result<String> {
    use toml_edit::{Item, Table, Value};

    let uses_crlf = contents.contains('\n')
        && contents
            .match_indices('\n')
            .all(|(index, _)| index > 0 && contents.as_bytes()[index - 1] == b'\r');
    let mut document = if contents.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        contents
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| FlightDeckError::Config(format!("failed to parse config.toml: {e}")))?
    };
    let project = document["project"]
        .or_insert(Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or_else(|| FlightDeckError::Config("[project] must be a table".to_string()))?;
    if let Some(item) = project.get_mut("default_base_branch") {
        let current = item.as_value_mut().ok_or_else(|| {
            FlightDeckError::Config("project.default_base_branch must be a string".to_string())
        })?;
        let decor = current.decor().clone();
        let mut replacement = Value::from(branch);
        *replacement.decor_mut() = decor;
        *current = replacement;
    } else {
        project.insert("default_base_branch", toml_edit::value(branch));
    }

    let output = document.to_string();
    if uses_crlf {
        Ok(output.replace("\r\n", "\n").replace('\n', "\r\n"))
    } else {
        Ok(output)
    }
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
        // The password-gating note is attached as a comment above the section,
        // and the `relay_password` field is present and documented.
        assert!(
            toml.contains("gated by a shared") && toml.contains("not published"),
            "global: {toml}"
        );
        assert!(toml.contains("relay_password"), "global: {toml}");
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
        assert!(out.contains("relay_password"), "out: {out}");
        // The comment sits directly above the section header.
        let comment_idx = out.find("# FlightDeck Remote").unwrap();
        let header_idx = out.find("[remote]").unwrap();
        assert!(comment_idx < header_idx, "comment must precede header");
    }

    // --- [web] section (specs/WEB_INTERFACE.md D5, D10, Q2) ---

    #[test]
    fn global_config_documents_web_section_disabled_and_loopback() {
        let toml = serialize_global_config(&default_config("x", "main")).unwrap();
        assert!(toml.contains("[web]"), "global: {toml}");
        assert!(
            toml.contains("127.0.0.1") && toml.contains("7420") && toml.contains("262144"),
            "global: {toml}"
        );
        // Explanatory comment sits above the section header.
        let comment_idx = toml.find("# FlightDeck Web").unwrap();
        let header_idx = toml.find("[web]").unwrap();
        assert!(
            comment_idx < header_idx,
            "comment must precede [web] header"
        );
        // Round-trips back into a config with the web section disabled/loopback.
        let cfg = parse_config(&toml).unwrap();
        assert!(!cfg.web.enabled);
        assert_eq!(cfg.web.bind, "127.0.0.1");
        assert_eq!(cfg.web.port, 7420);
        assert_eq!(cfg.web.replay_bytes, 262_144);
    }

    #[test]
    fn global_table_save_reattaches_web_comment() {
        let table: toml::Table =
            "[web]\nenabled = true\nport = 9000\nbind = \"127.0.0.1\"\nreplay_bytes = 262144\n"
                .parse()
                .unwrap();
        let out = serialize_global_table(&table).unwrap();
        let comment_idx = out.find("# FlightDeck Web").unwrap();
        let header_idx = out.find("[web]").unwrap();
        assert!(comment_idx < header_idx, "comment must precede header");
    }

    #[test]
    fn global_table_save_reattaches_both_remote_and_web_comments() {
        // Both sections annotated in the same save, in the order they appear.
        let table: toml::Table = "[remote]\nenabled = false\n\n[web]\nenabled = false\n"
            .parse()
            .unwrap();
        let out = serialize_global_table(&table).unwrap();
        assert!(out.contains("# FlightDeck Remote"), "out: {out}");
        assert!(out.contains("# FlightDeck Web"), "out: {out}");
    }

    #[test]
    fn project_overrides_one_web_field_leaving_siblings_at_global() {
        // The per-field-default regression this convention guards against:
        // overriding only `web.port` at the project layer must not wipe
        // `enabled`/`bind`/`replay_bytes` back to Rust zero values.
        let global = global_base();
        let project: toml::Table = "[web]\nport = 9000\n".parse().unwrap();
        let cfg = effective_config(global, project).unwrap();
        assert_eq!(cfg.web.port, 9000);
        assert!(!cfg.web.enabled);
        assert_eq!(cfg.web.bind, "127.0.0.1");
        assert_eq!(cfg.web.replay_bytes, 262_144);
    }

    #[test]
    fn project_web_enabled_override_keeps_global_port_and_bind() {
        let global = global_base();
        let project: toml::Table = "[web]\nenabled = true\n".parse().unwrap();
        let cfg = effective_config(global, project).unwrap();
        assert!(cfg.web.enabled);
        assert_eq!(cfg.web.port, 7420);
        assert_eq!(cfg.web.bind, "127.0.0.1");
    }

    #[test]
    fn missing_web_section_anywhere_still_loads_loopback_defaults() {
        // Old config predating [web] entirely: neither the global base (as it
        // existed before this setting) nor the minimal project mentions it.
        let mut global = global_base();
        global.remove("web");
        let project = minimal_project_config("p", "main").parse().unwrap();
        let cfg = effective_config(global, project).unwrap();
        assert!(!cfg.web.enabled);
        assert_eq!(cfg.web.bind, "127.0.0.1");
        assert_eq!(cfg.web.port, 7420);
        assert_eq!(cfg.web.replay_bytes, 262_144);
    }

    #[test]
    fn a_routable_bind_is_never_the_resolved_default() {
        // However a config is assembled (global-only, a project that sets an
        // unrelated web field, or a fully self-sufficient project with no
        // global layer at all), the resolved bind must be loopback unless a
        // human explicitly wrote a different bind value somewhere.
        let self_sufficient_project: toml::Table = serialize_config(&default_config("p", "main"))
            .unwrap()
            .parse()
            .unwrap();
        let cases: Vec<(toml::Table, toml::Table)> = vec![
            (global_base(), toml::Table::new()),
            (global_base(), "[web]\nenabled = true\n".parse().unwrap()),
            (global_base(), "[web]\nport = 8080\n".parse().unwrap()),
            (toml::Table::new(), self_sufficient_project),
        ];
        for (global, project) in cases {
            let cfg = effective_config(global, project).unwrap();
            assert_eq!(cfg.web.bind, "127.0.0.1");
        }
    }

    #[test]
    fn web_config_round_trips_through_toml() {
        let mut cfg = default_config("proj", "main");
        cfg.web.enabled = true;
        cfg.web.port = 8123;
        cfg.web.bind = "0.0.0.0".to_string();
        cfg.web.replay_bytes = 131_072;
        let toml_str = serialize_config(&cfg).unwrap();
        let parsed = parse_config(&toml_str).unwrap();
        assert_eq!(parsed.web, cfg.web);
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
    fn existing_ui_config_defaults_file_manager_to_empty() {
        let cfg = parse_config(
            r#"
[ui]
agent_tab_position = "left"
default_agent = "opencode"
"#,
        )
        .unwrap();

        assert_eq!(cfg.ui.file_manager, "");
    }

    #[test]
    fn ui_config_reads_file_manager_override() {
        let cfg = parse_config(
            r#"
[ui]
agent_tab_position = "left"
default_agent = "opencode"
file_manager = "nautilus"
"#,
        )
        .unwrap();

        assert_eq!(cfg.ui.file_manager, "nautilus");
    }

    #[test]
    fn load_config_reads_from_fakefs() {
        let cfg = default_config("fakefs-proj", "main");
        let toml_str = serialize_config(&cfg).unwrap();
        let fs = FakeFs::new().with_file("/repo/.flightdeck/config.toml", toml_str);
        let loaded = load_config(&fs, Path::new("/repo/.flightdeck/config.toml")).unwrap();
        assert_eq!(loaded.project.name, "fakefs-proj");
        assert_eq!(loaded.agents.len(), 4);
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
        assert_eq!(cfg.agents.len(), 4);
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
        assert_eq!(cfg.agents.len(), 4);
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
        assert_eq!(cfg.agents.len(), 4);
    }

    #[test]
    fn focused_base_update_preserves_comments_and_other_sections() {
        let original = "# project note\nnotes = '''\ndefault_base_branch = \"not a setting\"\n'''\n[project]\nname = \"demo\"\ndefault_base_branch = \"main\" # keep this note\n\n# ui note\n[ui]\nauto_continue = false\n";
        let updated = set_project_default_base(original, "release/next").unwrap();
        assert!(updated.contains("# project note"));
        assert!(updated.contains("# ui note"));
        assert!(updated.contains("default_base_branch = \"not a setting\""));
        assert!(updated.contains("# keep this note"));
        assert!(updated.contains("name = \"demo\""));
        assert!(updated.contains("default_base_branch = \"release/next\""));
        assert!(updated.contains("auto_continue = false"));
        assert_eq!(updated.lines().count(), original.lines().count());
    }

    #[test]
    fn focused_base_update_preserves_crlf_line_endings() {
        let original = "[project]\r\nname = \"demo\"\r\ndefault_base_branch = \"main\"\r\n";
        let updated = set_project_default_base(original, "develop").unwrap();
        assert_eq!(updated, original.replace("\"main\"", "\"develop\""));
    }

    #[test]
    fn focused_base_update_adds_a_missing_project_key() {
        let original = "[project]\nname = \"demo\"\n\n[ui]\nauto_continue = false\n";
        let updated = set_project_default_base(original, "develop").unwrap();
        let project = parse_table(&updated).unwrap();
        assert_eq!(
            project["project"]["default_base_branch"].as_str(),
            Some("develop")
        );
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
