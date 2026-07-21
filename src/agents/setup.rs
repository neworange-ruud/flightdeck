//! Explicit lifecycle integrations for built-in agent backends, plus the
//! `flightdeck setup-status` command's reusable global artifacts (SPECS §24).
//!
//! FlightDeck injects launch-scoped Claude Code/Codex hooks or an OpenCode
//! plugin automatically. They append explicit lifecycle events to each
//! worktree's [`agent_status_file`]; PTY traffic is never interpreted as work.
//!
//! [`agent_status_file`]: crate::app::state::agent_status_file
//!
//! ## Design
//!
//! Every integration writes one keyword (`working` / `idle` / `waiting`) to
//! `<worktree>/.flightdeck/agent-status`, which is exactly the path FlightDeck
//! polls (it derives the same path from the worktree, so no value needs to be
//! injected into the agent). The shell hooks are **self-contained one-liners**
//! (no external script file) so they work inside every Git worktree without
//! needing a committed helper, and they are **gated on `.flightdeck/` existing**
//! so they only write inside FlightDeck-managed worktrees — running the same
//! agent in an unrelated project writes nothing.
//!
//! `setup-status` additionally writes standalone artifacts into
//! `<repo>/.flightdeck/integrations/` for users who want the same signals in
//! sessions launched outside FlightDeck.

use crate::contracts::{AgentDef, FileSystem, Result};
use crate::fs::ignore::{ensure_gitignore_entry, STATUS_IGNORE_ENTRY};
use std::path::{Path, PathBuf};

/// Private runtime directory used for launch-scoped status integrations.
/// Everything below this directory is generated and ignored by Git.
pub const STATUS_RUNTIME_DIR: &str = ".flightdeck/runtime/status";

/// A built-in agent backend with a supported, explicit lifecycle API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusBackend {
    Claude,
    Codex,
    OpenCode,
}

/// Agent arguments and environment after adding FlightDeck's lifecycle bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLaunch {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// False for custom/unknown agents. Such agents deliberately fail closed:
    /// FlightDeck shows them as neutral and never infers work from PTY output.
    pub explicit: bool,
}

/// Identify a supported backend from its executable name. Unknown wrappers
/// fail closed because passing backend-specific flags to an arbitrary command
/// would be unsafe.
pub fn status_backend(agent: &AgentDef) -> Option<StatusBackend> {
    fn classify(value: &str) -> Option<StatusBackend> {
        let name = Path::new(value)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(value)
            .to_ascii_lowercase();
        let name = name
            .strip_suffix(".exe")
            .or_else(|| name.strip_suffix(".cmd"))
            .or_else(|| name.strip_suffix(".bat"))
            .unwrap_or(&name);
        match name {
            "claude" => Some(StatusBackend::Claude),
            "codex" => Some(StatusBackend::Codex),
            "opencode" => Some(StatusBackend::OpenCode),
            _ => None,
        }
    }

    classify(&agent.command)
}

/// Materialize and attach a launch-scoped lifecycle integration for a built-in
/// backend. Hooks/plugins write `working`, `idle`, or `waiting` to the status
/// file that [`crate::app::state::AppState`] polls. OpenCode questions and
/// permission prompts both mean the agent is waiting for user input.
///
/// `containerized` changes only paths passed to the agent: the generated files
/// live in the bind-mounted worktree in both modes.
pub fn prepare_status_launch(
    fs: &dyn FileSystem,
    agent: &AgentDef,
    worktree: &Path,
    containerized: bool,
) -> Result<StatusLaunch> {
    let Some(backend) = status_backend(agent) else {
        return Ok(StatusLaunch {
            args: agent.args.clone(),
            env: Vec::new(),
            explicit: false,
        });
    };

    let runtime = worktree.join(STATUS_RUNTIME_DIR);
    fs.create_dir_all(&runtime)?;
    // A freshly launched interactive agent starts at its prompt. Writing this
    // before spawn gives the UI a deterministic initial state even if a backend
    // does not emit a session-start event.
    fs.write(&worktree.join(".flightdeck/agent-status"), "idle\n")?;

    let agent_runtime = if containerized {
        format!("/workspace/{STATUS_RUNTIME_DIR}")
    } else {
        runtime.to_string_lossy().to_string()
    };
    let mut args = agent.args.clone();
    let mut env = Vec::new();

    match backend {
        StatusBackend::Claude => {
            let root = runtime.join("claude");
            fs.create_dir_all(&root.join(".claude-plugin"))?;
            fs.create_dir_all(&root.join("hooks"))?;
            fs.write(
                &root.join(".claude-plugin/plugin.json"),
                CLAUDE_PLUGIN_MANIFEST,
            )?;
            fs.write(&root.join("hooks/hooks.json"), CLAUDE_PLUGIN_HOOKS)?;
            args.push("--plugin-dir".to_string());
            args.push(format!("{agent_runtime}/claude"));
        }
        StatusBackend::Codex => {
            // CLI overrides form a session config layer. Codex merges hooks
            // from all active layers, so this does not replace user hooks.
            args.push("--enable".to_string());
            args.push("hooks".to_string());
            for (event, state) in [
                ("UserPromptSubmit", "working"),
                ("Stop", "idle"),
                ("PermissionRequest", "waiting"),
                ("PostToolUse", "working"),
            ] {
                args.push("--config".to_string());
                args.push(codex_hook_override(event, state));
            }
        }
        StatusBackend::OpenCode => {
            let root = runtime.join("opencode");
            fs.create_dir_all(&root.join("plugins"))?;
            fs.write(&root.join("plugins/flightdeck.js"), OPENCODE_RUNTIME_PLUGIN)?;
            env.push((
                "OPENCODE_CONFIG_DIR".to_string(),
                format!("{agent_runtime}/opencode"),
            ));
        }
    }

    Ok(StatusLaunch {
        args,
        env,
        explicit: true,
    })
}

fn codex_hook_override(event: &str, state: &str) -> String {
    let command =
        format!("[ -d .flightdeck ] && printf '{state}\\n' >> .flightdeck/agent-status; exit 0");
    format!(
        "hooks.{event}=[{{hooks=[{{type=\"command\",command={}}}]}}]",
        toml::Value::String(command)
    )
}

const CLAUDE_PLUGIN_MANIFEST: &str = r#"{
  "name": "flightdeck-status",
  "version": "1.0.0",
  "description": "Reports Claude Code lifecycle state to FlightDeck"
}
"#;

const CLAUDE_PLUGIN_HOOKS: &str = r#"{
  "description": "FlightDeck agent lifecycle status",
  "hooks": {
    "SessionStart": [{"hooks": [{"type": "command", "command": "[ -d .flightdeck ] && printf 'idle\\n' >> .flightdeck/agent-status; exit 0"}]}],
    "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "[ -d .flightdeck ] && printf 'working\\n' >> .flightdeck/agent-status; exit 0"}]}],
    "Stop": [{"hooks": [{"type": "command", "command": "[ -d .flightdeck ] && printf 'idle\\n' >> .flightdeck/agent-status; exit 0"}]}],
    "StopFailure": [{"hooks": [{"type": "command", "command": "[ -d .flightdeck ] && printf 'idle\\n' >> .flightdeck/agent-status; exit 0"}]}],
    "PermissionRequest": [{"hooks": [{"type": "command", "command": "[ -d .flightdeck ] && printf 'waiting\\n' >> .flightdeck/agent-status; exit 0"}]}],
    "PreToolUse": [{"matcher": "AskUserQuestion", "hooks": [{"type": "command", "command": "[ -d .flightdeck ] && printf 'waiting\\n' >> .flightdeck/agent-status; exit 0"}]}],
    "PostToolUse": [{"hooks": [{"type": "command", "command": "[ -d .flightdeck ] && printf 'working\\n' >> .flightdeck/agent-status; exit 0"}]}],
    "Notification": [
      {"matcher": "elicitation_dialog", "hooks": [{"type": "command", "command": "[ -d .flightdeck ] && printf 'waiting\\n' >> .flightdeck/agent-status; exit 0"}]},
      {"matcher": "idle_prompt", "hooks": [{"type": "command", "command": "[ -d .flightdeck ] && printf 'idle\\n' >> .flightdeck/agent-status; exit 0"}]}
    ]
  }
}
"#;

const OPENCODE_RUNTIME_PLUGIN: &str = r#"// Generated by FlightDeck. Explicit lifecycle state only; no terminal heuristics.
import { appendFileSync, existsSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export const FlightDeckStatus = async ({ directory, worktree }) => {
  const root = worktree || directory;
  const fdDir = join(root, ".flightdeck");
  const write = (state) => {
    try {
      if (existsSync(fdDir)) appendFileSync(join(fdDir, "agent-status"), state + "\n");
    } catch (_) {}
  };
  // Serialize the structured prompt so FlightDeck can offer real options on the
  // phone. OpenCode's event.properties schema is not formally documented, so
  // probe the likely field names defensively; an empty options array makes the
  // desktop reader fall back to the binary allow/deny prompt.
  const writePrompt = (event) => {
    try {
      if (!existsSync(fdDir)) return;
      const p = event.properties || {};
      const kind = event.type === "question.asked" ? "question" : "permission";
      const m = p.metadata || {};
      const text = p.question ?? p.title ?? p.text ?? m.title ?? m.text ?? "";
      const raw = Array.isArray(p.options)
        ? p.options
        : Array.isArray(m.options)
          ? m.options
          : [];
      const options = raw.map((o) =>
        o && typeof o === "object"
          ? {
              label: String(o.label ?? o.title ?? o.text ?? o.value ?? ""),
              description: o.description ?? o.hint ?? o.detail ?? undefined,
            }
          : { label: String(o) },
      );
      writeFileSync(
        join(fdDir, "agent-prompt.json"),
        JSON.stringify({ kind, text: String(text), options }),
      );
    } catch (_) {}
  };
  return {
    event: async ({ event }) => {
      if (event.type === "session.status") {
        const type = event.properties?.status?.type;
        if (type === "idle") write("idle");
        if (type === "busy" || type === "retry") write("working");
        return;
      }
      if (event.type === "session.idle") write("idle");
      if (event.type === "permission.asked" || event.type === "question.asked") {
        write("waiting");
        writePrompt(event);
        return;
      }
      if (
        event.type === "permission.replied" ||
        event.type === "question.replied" ||
        event.type === "question.rejected"
      ) {
        write("working");
      }
    },
  };
};
"#;

/// Outcome of [`write_status_integrations`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetupReport {
    /// Absolute paths of the artifact files written.
    pub written: Vec<PathBuf>,
    /// Whether the `.flightdeck/agent-status` `.gitignore` entry was added.
    pub gitignore_added: bool,
}

/// The directory (relative to repo root) the integration artifacts are written to.
pub const INTEGRATIONS_DIR: &str = ".flightdeck/integrations";

/// Write the per-tool status-hook artifacts into
/// `<repo>/.flightdeck/integrations/` and ensure the `.flightdeck/agent-status`
/// `.gitignore` entry exists. Idempotent: re-running overwrites the artifact
/// files with the current templates and leaves `.gitignore` untouched if the
/// entry is already present.
pub fn write_status_integrations(fs: &dyn FileSystem, repo_root: &Path) -> Result<SetupReport> {
    let dir = repo_root.join(INTEGRATIONS_DIR);
    fs.create_dir_all(&dir)?;

    let files: &[(&str, &str)] = &[
        ("README.md", README),
        ("claude-code.settings.json", CLAUDE_SETTINGS),
        ("codex-config.toml", CODEX_CONFIG),
        ("opencode-flightdeck.js", OPENCODE_PLUGIN),
    ];

    let mut written = Vec::with_capacity(files.len());
    for (name, contents) in files {
        let path = dir.join(name);
        fs.write(&path, contents)?;
        written.push(path);
    }

    let gitignore_added = ensure_gitignore_entry(fs, repo_root, STATUS_IGNORE_ENTRY)?;

    Ok(SetupReport {
        written,
        gitignore_added,
    })
}

// ---------------------------------------------------------------------------
// Artifact templates
// ---------------------------------------------------------------------------

/// Overview + per-tool wiring instructions.
const README: &str = r#"# FlightDeck agent status integrations

FlightDeck shows each Agent Tab's status (idle / working / waiting / …) in the
sidebar. Sessions launched by FlightDeck already receive a launch-scoped status
integration automatically; terminal output is never used as activity.

These optional standalone integrations provide the same explicit status events
to sessions launched outside FlightDeck. Each agent writes a keyword to
`<worktree>/.flightdeck/agent-status` when it starts a turn, finishes, or needs
your input.

Every hook writes one of: `working`, `idle`, `waiting`. The hooks are gated on
`.flightdeck/` existing, so they only ever write inside a FlightDeck worktree.

`flightdeck setup-status` already added `.flightdeck/agent-status` to your
`.gitignore` (commit that change so new worktrees inherit it).

---

## Claude Code

Merge `claude-code.settings.json` (in this folder) into your **user** settings at
`~/.claude/settings.json` (or this project's `.claude/settings.json`). It wires:

- `UserPromptSubmit` → `working`
- `Stop` / `StopFailure` / `SessionStart` → `idle`
- `PermissionRequest` / elicitation prompt → `waiting`; `PostToolUse` → `working`
- idle notification → `idle`

The hooks write nothing to stdout, so they never disturb the session or get
injected into Claude's context.

## Codex CLI

Append the contents of `codex-config.toml` to your **user** config at
`~/.codex/config.toml` (Codex only honours hooks/notify in the user-level file).
It wires `UserPromptSubmit` → `working` and `Stop` → `idle`. A `notify`
fallback (idle-only, for older Codex) is included as a comment.

## OpenCode

Copy `opencode-flightdeck.js` to `~/.config/opencode/plugin/flightdeck.js`
(global) — or to `.opencode/plugin/` in your project. It maps `session.status`
busy/idle → `working`/`idle`, and permission or question prompts → `waiting`.

---

After wiring, restart the agent in a tab (Ctrl-r). The tab status should switch
to `waiting` the moment the agent asks for confirmation, and to `idle` the
moment it finishes.
"#;

/// Claude Code `settings.json` hooks. Self-contained command strings; no
/// external script. `${CLAUDE_PROJECT_DIR:-$PWD}` resolves to the worktree.
const CLAUDE_SETTINGS: &str = r##"{
  "hooks": {
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'working\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'idle\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
          }
        ]
      }
    ],
    "StopFailure": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'idle\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
          }
        ]
      }
    ],
    "PermissionRequest": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'waiting\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "AskUserQuestion",
        "hooks": [
          {
            "type": "command",
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'waiting\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'working\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
          }
        ]
      }
    ],
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'idle\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
          }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "elicitation_dialog",
        "hooks": [
          {
            "type": "command",
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'waiting\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
          }
        ]
      },
      {
        "matcher": "idle_prompt",
        "hooks": [
          {
            "type": "command",
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'idle\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
          }
        ]
      }
    ]
  }
}
"##;

/// Codex CLI `~/.codex/config.toml` lifecycle hooks (cwd = worktree). The
/// `notify` fallback is left commented since it signals turn-completion only.
const CODEX_CONFIG: &str = r##"# --- FlightDeck agent status (append to ~/.codex/config.toml) ---------------
# Lifecycle hooks run with the session cwd (the worktree) as their working dir.

[[hooks.UserPromptSubmit]]
[[hooks.UserPromptSubmit.hooks]]
type = "command"
command = "r=\"$PWD\"; [ -d \"$r/.flightdeck\" ] && printf 'working\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"

[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "r=\"$PWD\"; [ -d \"$r/.flightdeck\" ] && printf 'idle\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"

# Fallback for older Codex without lifecycle hooks (idle only, fires on
# agent-turn-complete). `notify` is honoured ONLY in the user-level config.
# notify = ["sh", "-c", "r=\"$PWD\"; [ -d \"$r/.flightdeck\" ] && printf 'idle\\n' >> \"$r/.flightdeck/agent-status\"; exit 0", "flightdeck-notify"]
"##;

/// OpenCode plugin (plain JS, no type imports so it works as a global plugin).
const OPENCODE_PLUGIN: &str = r#"// FlightDeck agent status plugin for OpenCode.
// Install globally: copy to ~/.config/opencode/plugin/flightdeck.js
// or per-project:    copy to .opencode/plugin/flightdeck.js
//
// Writes one of working/idle/waiting to <worktree>/.flightdeck/agent-status,
// which FlightDeck polls. Gated on .flightdeck/ existing, so it is a no-op
// outside FlightDeck worktrees.
import { appendFileSync, existsSync, writeFileSync } from "node:fs";
import { join } from "node:path";

export const FlightDeck = async ({ directory, worktree }) => {
  const root = worktree || directory;
  const fdDir = join(root, ".flightdeck");
  const write = (state) => {
    try {
      if (existsSync(fdDir)) appendFileSync(join(fdDir, "agent-status"), state + "\n");
    } catch (_) {
      /* never let status writing break the session */
    }
  };
  // Serialize the structured prompt (question/permission text + options) so
  // FlightDeck can offer real options on the phone. OpenCode's event.properties
  // schema is not formally documented, so probe the likely field names
  // defensively; an empty options array makes the desktop reader fall back to
  // the binary allow/deny prompt.
  const writePrompt = (event) => {
    try {
      if (!existsSync(fdDir)) return;
      const p = event.properties || {};
      const kind = event.type === "question.asked" ? "question" : "permission";
      const m = p.metadata || {};
      const text = p.question ?? p.title ?? p.text ?? m.title ?? m.text ?? "";
      const raw = Array.isArray(p.options)
        ? p.options
        : Array.isArray(m.options)
          ? m.options
          : [];
      const options = raw.map((o) =>
        o && typeof o === "object"
          ? {
              label: String(o.label ?? o.title ?? o.text ?? o.value ?? ""),
              description: o.description ?? o.hint ?? o.detail ?? undefined,
            }
          : { label: String(o) },
      );
      writeFileSync(
        join(fdDir, "agent-prompt.json"),
        JSON.stringify({ kind, text: String(text), options }),
      );
    } catch (_) {
      /* never let prompt capture break the session */
    }
  };

  return {
    event: async ({ event }) => {
      if (event.type === "session.status") {
        const type = event.properties?.status?.type;
        if (type === "idle") write("idle");
        if (type === "busy" || type === "retry") write("working");
        return;
      }
      if (event.type === "session.idle") write("idle");
      // Needs the user's attention (permission or AskUserQuestion prompt).
      if (event.type === "permission.asked" || event.type === "question.asked") {
        write("waiting");
        writePrompt(event);
        return;
      }
      if (
        event.type === "permission.replied" ||
        event.type === "question.replied" ||
        event.type === "question.rejected"
      ) {
        write("working");
      }
    },
  };
};
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeFs;
    use std::path::Path;

    const REPO: &str = "/repo";

    fn agent(key: &str, command: &str) -> AgentDef {
        AgentDef {
            key: key.to_string(),
            display_name: key.to_string(),
            command: command.to_string(),
            args: vec!["--existing".to_string()],
            status_patterns: Default::default(),
        }
    }

    #[test]
    fn detects_supported_backends_by_executable() {
        assert_eq!(
            status_backend(&agent("custom", "/usr/local/bin/claude")),
            Some(StatusBackend::Claude)
        );
        assert_eq!(status_backend(&agent("codex", "wrapper")), None);
        assert_eq!(
            status_backend(&agent("custom", "C:\\tools\\opencode.cmd")),
            if cfg!(windows) {
                Some(StatusBackend::OpenCode)
            } else {
                None
            }
        );
        assert_eq!(status_backend(&agent("custom", "other")), None);
    }

    #[test]
    fn prepares_claude_plugin_without_replacing_existing_args() {
        let fs = FakeFs::new();
        let launch =
            prepare_status_launch(&fs, &agent("claude", "claude"), Path::new(REPO), false).unwrap();
        assert!(launch.explicit);
        assert_eq!(launch.args[0], "--existing");
        assert!(launch.args.contains(&"--plugin-dir".to_string()));
        let hooks = fs
            .file_contents(Path::new(
                "/repo/.flightdeck/runtime/status/claude/hooks/hooks.json",
            ))
            .unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&hooks).expect("valid Claude hooks JSON");
        assert!(hooks.contains("UserPromptSubmit"));
        // AskUserQuestion must flip the agent to `waiting` (remote-control-z30):
        // Claude fires no PermissionRequest for a question, so a PreToolUse hook
        // matching the tool is what surfaces the wait to the phone.
        let pre = &parsed["hooks"]["PreToolUse"][0];
        assert_eq!(pre["matcher"], "AskUserQuestion");
        assert!(pre["hooks"][0]["command"]
            .as_str()
            .is_some_and(|c| c.contains("waiting")));
        let manifest = fs
            .file_contents(Path::new(
                "/repo/.flightdeck/runtime/status/claude/.claude-plugin/plugin.json",
            ))
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&manifest).expect("valid Claude plugin manifest");
        assert_eq!(
            fs.file_contents(Path::new("/repo/.flightdeck/agent-status")),
            Some("idle\n".to_string())
        );
    }

    #[test]
    fn prepares_codex_inline_hooks_as_valid_toml_overrides() {
        let fs = FakeFs::new();
        let launch =
            prepare_status_launch(&fs, &agent("codex", "codex"), Path::new(REPO), false).unwrap();
        assert!(launch.explicit);
        assert!(launch.args.windows(2).any(|w| w == ["--enable", "hooks"]));
        for pair in launch.args.windows(2).filter(|w| w[0] == "--config") {
            let (_, value) = pair[1].split_once('=').expect("dotted key=value");
            let document = format!("value = {value}");
            toml::from_str::<toml::Value>(&document)
                .unwrap_or_else(|e| panic!("invalid Codex hook override {:?}: {e}", pair[1]));
        }
        assert!(launch
            .args
            .iter()
            .any(|a| a.starts_with("hooks.UserPromptSubmit=")));
        assert!(launch.args.iter().any(|a| a.starts_with("hooks.Stop=")));
        assert!(
            launch
                .args
                .iter()
                .any(|a| a.starts_with("hooks.PermissionRequest=")),
            "Codex input prompts must report the waiting state"
        );
    }

    #[test]
    fn prepares_opencode_runtime_plugin_and_config_environment() {
        let fs = FakeFs::new();
        let launch =
            prepare_status_launch(&fs, &agent("opencode", "opencode"), Path::new(REPO), true)
                .unwrap();
        assert_eq!(
            launch.env,
            vec![(
                "OPENCODE_CONFIG_DIR".to_string(),
                "/workspace/.flightdeck/runtime/status/opencode".to_string()
            )]
        );
        let plugin = fs
            .file_contents(Path::new(
                "/repo/.flightdeck/runtime/status/opencode/plugins/flightdeck.js",
            ))
            .unwrap();
        assert!(plugin.contains("session.status"));
        assert!(plugin.contains("type === \"busy\""));
        assert!(plugin.contains("type === \"idle\""));
        for event in ["question.asked", "question.replied", "question.rejected"] {
            assert!(
                plugin.contains(event),
                "runtime plugin must handle the OpenCode {event} lifecycle event"
            );
        }
        // The runtime plugin must also serialize the structured prompt to the
        // sidecar the desktop bridge reads on the needs-input edge.
        assert!(
            plugin.contains("agent-prompt.json"),
            "runtime plugin must write the prompt sidecar"
        );
        assert!(
            plugin.contains("writeFileSync"),
            "prompt sidecar is overwritten, not appended"
        );
        assert!(
            plugin.contains("writePrompt(event)"),
            "runtime plugin must capture the prompt on question/permission asked"
        );
    }

    #[test]
    fn unknown_agent_fails_closed_without_generating_runtime_files() {
        let fs = FakeFs::new();
        let launch =
            prepare_status_launch(&fs, &agent("custom", "other"), Path::new(REPO), false).unwrap();
        assert!(!launch.explicit);
        assert!(launch.env.is_empty());
        assert_eq!(launch.args, vec!["--existing"]);
        assert!(!fs.exists(Path::new("/repo/.flightdeck/agent-status")));
    }

    #[test]
    fn writes_all_artifacts_and_gitignore_entry() {
        let fs = FakeFs::new();
        let report = write_status_integrations(&fs, Path::new(REPO)).unwrap();

        assert_eq!(report.written.len(), 4);
        assert!(report.gitignore_added);

        for name in [
            "README.md",
            "claude-code.settings.json",
            "codex-config.toml",
            "opencode-flightdeck.js",
        ] {
            let p = Path::new(REPO).join(INTEGRATIONS_DIR).join(name);
            assert!(fs.file_contents(&p).is_some(), "missing artifact {name}");
        }

        let gi = fs
            .file_contents(Path::new("/repo/.gitignore"))
            .unwrap_or_default();
        assert!(gi.contains(STATUS_IGNORE_ENTRY));
    }

    #[test]
    fn artifacts_only_write_status_keywords_flightdeck_understands() {
        // Guard: every keyword written by the templates must be one the poller
        // maps to a status, or the integration is silently broken.
        use crate::app::state::status_keyword_to_interpreted;
        for kw in ["working", "idle", "waiting"] {
            assert!(
                status_keyword_to_interpreted(kw).is_some(),
                "template keyword '{kw}' not understood by the poller"
            );
        }
        // And the templates reference the path FlightDeck polls.
        assert!(CLAUDE_SETTINGS.contains(".flightdeck/agent-status"));
        assert!(CODEX_CONFIG.contains(".flightdeck/agent-status"));
        assert!(OPENCODE_PLUGIN.contains(".flightdeck/agent-status"));
        for event in ["question.asked", "question.replied", "question.rejected"] {
            assert!(
                OPENCODE_RUNTIME_PLUGIN.contains(event) && OPENCODE_PLUGIN.contains(event),
                "all OpenCode bridges must handle {event}"
            );
        }
        // Both OpenCode plugins must serialize the structured prompt (question
        // text + options) to the sidecar the desktop reads, overwriting it each
        // time (writeFileSync, not appendFileSync) so a stale prompt never
        // lingers, and must invoke that capture on the asked events.
        for plugin in [OPENCODE_RUNTIME_PLUGIN, OPENCODE_PLUGIN] {
            assert!(
                plugin.contains("agent-prompt.json"),
                "OpenCode plugin must write the prompt sidecar"
            );
            assert!(
                plugin.contains("writeFileSync"),
                "prompt sidecar is overwritten, not appended"
            );
            assert!(
                plugin.contains("\"question\"") && plugin.contains("options"),
                "OpenCode plugin must derive question.asked options"
            );
            assert!(
                plugin.contains("writePrompt(event)"),
                "OpenCode plugin must capture the prompt on the asked events"
            );
        }
    }

    #[test]
    fn gitignore_entry_is_idempotent() {
        let fs = FakeFs::new();
        write_status_integrations(&fs, Path::new(REPO)).unwrap();
        let second = write_status_integrations(&fs, Path::new(REPO)).unwrap();
        assert!(!second.gitignore_added, "should not re-add the entry");
        let gi = fs
            .file_contents(Path::new("/repo/.gitignore"))
            .unwrap_or_default();
        assert_eq!(
            gi.lines()
                .filter(|l| l.trim() == STATUS_IGNORE_ENTRY)
                .count(),
            1
        );
    }
}
