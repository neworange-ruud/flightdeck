//! Default config construction and validation (SPECS §8).

use crate::contracts::{
    AgentDef, Config, ContainersConfig, FlightDeckError, GitConfig, NotificationsConfig,
    ProjectConfig, RemoteConfig, Result, StatusPatterns, UiConfig, UpdateConfig, WebConfig,
    WorktreesConfig,
};
use std::collections::BTreeMap;

/// Build the default `config.toml` contents for a project (SPECS §8), including
/// the four initial agents (OpenCode default, Claude Code, Codex CLI, Cursor
/// CLI).
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

    // cursor — the binary is `cursor-agent`, not `cursor` (which launches the
    // Cursor editor).
    agents.insert(
        "cursor".to_string(),
        AgentDef {
            key: "cursor".to_string(),
            display_name: "Cursor CLI".to_string(),
            command: "cursor-agent".to_string(),
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
            auto_continue: true,
            default_agent: "opencode".to_string(),
            ..UiConfig::default()
        },
        notifications: NotificationsConfig::default(),
        update: UpdateConfig::default(),
        remote: RemoteConfig::default(),
        web: WebConfig::default(),
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
/// Allowed Agent Tabs sidebar positions (SPECS §8, `specs/WEB_INTERFACE.md`
/// §6.5 R24). The same two words the configuration manager cycles.
const AGENT_TAB_POSITIONS: &[&str] = &["left", "right"];

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
    // Checked here for the reason its neighbours are, and only since R24 made
    // it worth checking: until the key moved the sidebar, an unexpected value
    // and the default drew the same screen, so rejecting one would have been
    // pedantry. Now `agent_tab_position = "rihgt"` would silently draw `left`,
    // which is the exact class of quiet-nothing this validation exists for.
    if !AGENT_TAB_POSITIONS.contains(&config.ui.agent_tab_position.as_str()) {
        return Err(FlightDeckError::Config(format!(
            "ui.agent_tab_position '{}' is not valid (expected one of {AGENT_TAB_POSITIONS:?})",
            config.ui.agent_tab_position
        )));
    }

    validate_containers(&config.containers)?;
    validate_web(&config.web)?;

    Ok(())
}

/// Largest sane `[web] replay_bytes` we accept: 64 MiB per terminal. There is
/// no protocol reason to cap it lower, but an unbounded value is a config typo
/// waiting to OOM a machine running several terminals at once (each terminal
/// gets its own ring buffer, see `crate::web::replay::ReplayBuffer`). This is a
/// generous multiple of the 256 KiB default, not a tuned production limit.
const MAX_REPLAY_BYTES: usize = 64 * 1024 * 1024;

/// Validate the `[web]` section (`specs/WEB_INTERFACE.md` D5, D10, Q2).
///
/// [`crate::web::replay::ReplayBuffer::new`] deliberately accepts *any*
/// capacity, including 0 — it is a pure data structure with no opinion on
/// sane sizing. Turning a *configured* `replay_bytes` (or `port`) into a
/// server that's actually usable is this config layer's job, so we reject the
/// nonsensical values here rather than let them silently reach the server:
///
/// - `port == 0` is rejected. `0` means "let the OS pick an ephemeral port" in
///   a raw bind() call, but FlightDeck Web is meant to be reachable at a
///   stable, bookmarkable address (D10) — a port that changes every launch
///   would break that promise silently, so we reject rather than reinterpret.
/// - `bind` empty (or all-whitespace) is rejected: an empty string is not a
///   valid listen address, and the correct spelling for "loopback only"
///   (the resolved default, D5) is the explicit `"127.0.0.1"`, not "".
/// - `replay_bytes == 0` is rejected: it would silently disable reconnect
///   resume (Q3 depends on there being a retained window to resume from),
///   which is exactly the kind of "quietly broken" behavior this validation
///   exists to prevent.
/// - `replay_bytes` above [`MAX_REPLAY_BYTES`] is rejected as a likely typo /
///   unit confusion (e.g. mistaking the field for KiB or MiB) rather than
///   silently accepting an allocation that could exhaust memory once
///   multiplied across every open terminal.
///
/// Note this section is only meaningful when `enabled` (or the palette starts
/// it manually), but — like `[containers]` — we validate it unconditionally:
/// unlike containers, a malformed `[web]` section is cheap to check and there
/// is no cost to catching the typo before the user flips `enabled` on and
/// wonders why the server won't start.
fn validate_web(web: &crate::contracts::WebConfig) -> Result<()> {
    if web.port == 0 {
        return Err(FlightDeckError::Config(
            "web.port must not be 0 (the web interface needs a stable, bookmarkable port)"
                .to_string(),
        ));
    }
    if web.bind.trim().is_empty() {
        return Err(FlightDeckError::Config(
            "web.bind must not be empty (use \"127.0.0.1\" for loopback)".to_string(),
        ));
    }
    if web.replay_bytes == 0 {
        return Err(FlightDeckError::Config(
            "web.replay_bytes must not be 0 (it would silently disable reconnect resume)"
                .to_string(),
        ));
    }
    if web.replay_bytes > MAX_REPLAY_BYTES {
        return Err(FlightDeckError::Config(format!(
            "web.replay_bytes {} exceeds the {}-byte sanity limit per terminal",
            web.replay_bytes, MAX_REPLAY_BYTES
        )));
    }
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
    fn default_config_has_the_four_builtin_agents() {
        let cfg = default_config("my-project", "main");
        assert_eq!(cfg.agents.len(), 4);
        assert!(cfg.agents.contains_key("opencode"));
        assert!(cfg.agents.contains_key("claude"));
        assert!(cfg.agents.contains_key("codex"));
        assert!(cfg.agents.contains_key("cursor"));
    }

    #[test]
    fn default_cursor_agent_launches_the_cli_not_the_editor() {
        // `cursor` is the Cursor editor launcher; the CLI agent is a separate
        // binary, and only that one is a recognised status backend.
        let cfg = default_config("my-project", "main");
        let cursor = cfg.agents.get("cursor").expect("cursor agent");
        assert_eq!(cursor.command, "cursor-agent");
        assert_eq!(cursor.display_name, "Cursor CLI");
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
    fn default_config_leaves_file_manager_empty() {
        // Empty means "use the per-OS default launcher" (open / explorer.exe /
        // xdg-open); the key is still written so the global config documents it.
        let cfg = default_config("my-project", "main");
        assert_eq!(cfg.ui.file_manager, "");
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

    #[test]
    fn validate_rejects_an_unknown_agent_tab_position() {
        let mut cfg = default_config("proj", "main");
        assert!(validate(&cfg).is_ok(), "the default is valid");
        cfg.ui.agent_tab_position = "right".to_string();
        assert!(validate(&cfg).is_ok(), "1h position 4's other value");
        cfg.ui.agent_tab_position = "rihgt".to_string();
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("ui.agent_tab_position"));
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

    // --- [web] section (specs/WEB_INTERFACE.md D5, D10, Q2) ---

    #[test]
    fn default_config_web_disabled_and_loopback() {
        let cfg = default_config("proj", "main");
        assert!(!cfg.web.enabled);
        assert_eq!(cfg.web.port, 7420);
        assert_eq!(cfg.web.bind, "127.0.0.1");
        assert_eq!(cfg.web.replay_bytes, 262_144);
    }

    #[test]
    fn validate_rejects_web_port_zero() {
        let mut cfg = default_config("proj", "main");
        cfg.web.port = 0;
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("web.port"));
    }

    #[test]
    fn validate_rejects_web_replay_bytes_zero() {
        let mut cfg = default_config("proj", "main");
        cfg.web.replay_bytes = 0;
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("replay_bytes"));
    }

    #[test]
    fn validate_rejects_web_replay_bytes_too_large() {
        let mut cfg = default_config("proj", "main");
        cfg.web.replay_bytes = MAX_REPLAY_BYTES + 1;
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("replay_bytes"));
    }

    #[test]
    fn validate_accepts_web_replay_bytes_at_max() {
        let mut cfg = default_config("proj", "main");
        cfg.web.replay_bytes = MAX_REPLAY_BYTES;
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn validate_rejects_empty_web_bind() {
        let mut cfg = default_config("proj", "main");
        cfg.web.bind = "".to_string();
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("web.bind"));
    }

    #[test]
    fn validate_rejects_whitespace_only_web_bind() {
        let mut cfg = default_config("proj", "main");
        cfg.web.bind = "   ".to_string();
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn validate_accepts_routable_web_bind() {
        // A routable bind is an explicit opt-in (D5) but is not itself
        // rejected by validation — the warning is a UI-layer concern, not a
        // config-validity concern.
        let mut cfg = default_config("proj", "main");
        cfg.web.bind = "0.0.0.0".to_string();
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn validate_accepts_default_web_config() {
        let cfg = default_config("proj", "main");
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn web_config_partial_table_still_resolves_loopback_default() {
        // A config that only sets `enabled` (old/partial config, or a project
        // override) must still resolve `bind` to loopback via the per-field
        // default — never to an empty string / struct-level default (the
        // regression this convention exists to prevent).
        let cfg: Config = "[web]\nenabled = true\n"
            .parse::<toml::Table>()
            .unwrap()
            .try_into()
            .unwrap();
        assert!(cfg.web.enabled);
        assert_eq!(cfg.web.bind, "127.0.0.1");
        assert_eq!(cfg.web.port, 7420);
        assert_eq!(cfg.web.replay_bytes, 262_144);
    }

    #[test]
    fn web_config_absent_section_defaults_to_disabled_loopback() {
        // No [web] table at all (an old config predating this setting) must
        // still parse and resolve every field to its default.
        let cfg: Config = "[project]\nname = \"p\"\ndefault_base_branch = \"main\"\n"
            .parse::<toml::Table>()
            .unwrap()
            .try_into()
            .unwrap();
        assert!(!cfg.web.enabled);
        assert_eq!(cfg.web.bind, "127.0.0.1");
        assert_eq!(cfg.web.port, 7420);
        assert_eq!(cfg.web.replay_bytes, 262_144);
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
