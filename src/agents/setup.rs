//! Explicit lifecycle integrations for built-in agent backends, plus the
//! `flightdeck setup-status` command's reusable global artifacts (SPECS §24).
//!
//! FlightDeck injects launch-scoped Claude Code/Codex/Cursor hooks or an
//! OpenCode plugin automatically. They append explicit lifecycle events to each
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
    Cursor,
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
            // Only the agent binary, never the bare `cursor` editor launcher:
            // `cursor` opens the Cursor IDE and would silently receive flags
            // meant for the CLI.
            "cursor-agent" => Some(StatusBackend::Cursor),
            _ => None,
        }
    }

    classify(&agent.command)
}

/// The agent-visible (container-internal) path to the status file, when
/// `containerized`. The worktree is bind-mounted at
/// [`crate::runtime::container::WORKSPACE`] with that as `WORKDIR`, so this is
/// a fixed Linux path regardless of the host OS — built as a string constant,
/// never via `Path::join`, which would emit backslashes on a Windows host.
const CONTAINER_STATUS_FILE: &str = "/workspace/.flightdeck/agent-status";

/// The agent-visible (container-internal) path to the question sidecar. See
/// [`CONTAINER_STATUS_FILE`].
const CONTAINER_QUESTION_FILE: &str = "/workspace/.flightdeck/agent-question.json";

/// Materialize and attach a launch-scoped lifecycle integration for a built-in
/// backend. Hooks/plugins write `working`, `idle`, or `waiting` to the status
/// file that [`crate::app::state::AppState`] polls. OpenCode questions and
/// permission prompts both mean the agent is waiting for user input.
///
/// `containerized` changes the paths passed to the agent (both the plugin-dir
/// flag and the path templated into the hook bodies): the generated files
/// still live on the host, under `status_root`, but the agent inside the
/// container sees them at their bind-mounted `/workspace/...` location, since
/// a host path (or, in isolated mode, a host temp directory entirely outside
/// the bind mount) does not exist inside the container.
///
/// `status_root` is where the plugin dir, the seeded status file, and
/// `agent-question.json` live **on the host** — this is also what
/// [`crate::app::state::agent_status_file`] and the desktop bridge poll, so it
/// must always be the host truth even when the hook body itself has to carry
/// a different, container-internal path. Passing `worktree` for it reproduces
/// today's behavior exactly. When it differs from `worktree` (an isolated,
/// non-containerized run), the hook bodies carry `status_root`'s *absolute*
/// host path instead of a path relative to the agent's working directory, so
/// lifecycle events still land somewhere FlightDeck polls even though the
/// agent's cwd is the project.
///
/// A `status_root` that differs from `worktree` is treated as exclusively
/// FlightDeck-owned scratch space: any pre-existing `.flightdeck` contents
/// under it are wiped before writing, so a stale directory left behind by a
/// crashed earlier run (its pid-based name can be recycled — see
/// [`crate::isolated_status_dir`]) never bleeds into a fresh run.
/// `status_root == worktree` is never wiped.
pub fn prepare_status_launch(
    fs: &dyn FileSystem,
    agent: &AgentDef,
    worktree: &Path,
    status_root: &Path,
    containerized: bool,
) -> Result<StatusLaunch> {
    let Some(backend) = status_backend(agent) else {
        return Ok(StatusLaunch {
            args: agent.args.clone(),
            env: Vec::new(),
            explicit: false,
        });
    };

    // `status_root` is FlightDeck-owned scratch only when it differs from the
    // worktree (an isolated run's redirect target, never a user's project).
    // Narrowed to `.flightdeck` (not the whole root) so this can never nuke
    // something outside what this function itself owns, if a future caller
    // ever passes a status_root that isn't purely scratch.
    let flightdeck_dir = status_root.join(".flightdeck");
    if status_root != worktree && fs.exists(&flightdeck_dir) {
        fs.remove_dir_all(&flightdeck_dir)?;
    }

    let runtime = status_root.join(STATUS_RUNTIME_DIR);
    fs.create_dir_all(&runtime)?;
    // Host truth: what FlightDeck itself writes to and polls (via
    // `agent_status_file`) regardless of containerization.
    let status_file = status_root.join(".flightdeck").join("agent-status");
    // A freshly launched interactive agent starts at its prompt. Writing this
    // before spawn gives the UI a deterministic initial state even if a backend
    // does not emit a session-start event.
    fs.write(&status_file, "idle\n")?;
    let question_file = status_root.join(".flightdeck").join("agent-question.json");

    let agent_runtime = if containerized {
        format!("/workspace/{STATUS_RUNTIME_DIR}")
    } else {
        runtime.to_string_lossy().to_string()
    };
    // Agent-visible: what gets templated into the hook bodies. Containerized
    // agents only ever see `/workspace/...`; a host path (or an isolated run's
    // host temp directory, which is never bind-mounted) would resolve to
    // nothing inside the container, silently swallowed by each body's
    // trailing `exit 0`.
    let (agent_status_file, agent_question_file) = if containerized {
        (
            CONTAINER_STATUS_FILE.to_string(),
            CONTAINER_QUESTION_FILE.to_string(),
        )
    } else {
        (
            status_file.to_string_lossy().to_string(),
            question_file.to_string_lossy().to_string(),
        )
    };
    let mut args = agent.args.clone();
    let mut env = Vec::new();
    // Every backend below installs a working integration; only Cursor can fail
    // to (see `install_cursor_hooks`), and it lowers this itself.
    let mut explicit = true;

    match backend {
        StatusBackend::Claude => {
            let root = runtime.join("claude");
            fs.create_dir_all(&root.join(".claude-plugin"))?;
            fs.create_dir_all(&root.join("hooks"))?;
            fs.write(
                &root.join(".claude-plugin/plugin.json"),
                CLAUDE_PLUGIN_MANIFEST,
            )?;
            fs.write(
                &root.join("hooks/hooks.json"),
                &claude_plugin_hooks(&agent_status_file, &agent_question_file),
            )?;
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
                args.push(codex_hook_override(event, state, &agent_status_file));
            }
        }
        StatusBackend::OpenCode => {
            let root = runtime.join("opencode");
            fs.create_dir_all(&root.join("plugins"))?;
            fs.write(
                &root.join("plugins/flightdeck.js"),
                &opencode_runtime_plugin(&agent_status_file),
            )?;
            env.push((
                "OPENCODE_CONFIG_DIR".to_string(),
                format!("{agent_runtime}/opencode"),
            ));
        }
        StatusBackend::Cursor => {
            explicit = install_cursor_hooks(fs, worktree, status_root, &agent_status_file)?;
        }
    }

    Ok(StatusLaunch {
        args,
        env,
        explicit,
    })
}

/// Install Cursor CLI's launch-scoped lifecycle hooks into
/// `<worktree>/.cursor/hooks.json`, returning whether the tab now has explicit
/// status.
///
/// **Why a file in the worktree and not a `--plugin-dir` like Claude's.**
/// Cursor does load hooks from a plugin directory, but it gates the two events
/// FlightDeck actually needs — `beforeSubmitPrompt` (turn start) and `stop`
/// (turn end) — on the *user* (`~/.cursor/hooks.json`) or *project*
/// (`<workspace>/.cursor/hooks.json`) config declaring them: the plugin's own
/// entries are merged into the executed set but are not consulted by the
/// "should we run this step at all" check (verified against cursor-agent
/// 2026.08.31 — a plugin-only install fires `sessionStart`/`preToolUse`/
/// `postToolUse`/`beforeShellExecution` and nothing else, so a tab would go to
/// `working` and never come back). Cursor offers no environment variable that
/// relocates the user-level file, so the project-level file inside the
/// FlightDeck-managed worktree is the only launch-scoped place left.
///
/// Nothing the user owns is ever overwritten:
///
/// * A pre-existing `.cursor/hooks.json` that is not FlightDeck's (no
///   [`CURSOR_HOOKS_MARKER`]) is left exactly as it is, and the tab falls back
///   to neutral status rather than half-working status.
/// * When FlightDeck does write the file, it also drops a self-ignoring
///   `.cursor/.gitignore` (if there is none) so neither generated file shows up
///   in `git status` — the worktree diff stays the agent's work alone.
/// * `status_root != worktree` means an isolated, non-containerized run, whose
///   contract is that FlightDeck writes nothing under the project
///   (`specs/ISOLATED_MODE.md` §2). Cursor's hooks have to live under the
///   agent's own workspace, so there is nowhere legal to put them: the run
///   simply gets neutral status.
fn install_cursor_hooks(
    fs: &dyn FileSystem,
    worktree: &Path,
    status_root: &Path,
    agent_status_file: &str,
) -> Result<bool> {
    if status_root != worktree {
        return Ok(false);
    }
    let dir = worktree.join(".cursor");
    let hooks = dir.join("hooks.json");
    let ours = match fs.read_to_string(&hooks) {
        Ok(existing) => existing.contains(CURSOR_HOOKS_MARKER),
        // Unreadable also covers "absent", which is the common case.
        Err(_) => !fs.exists(&hooks),
    };
    if !ours {
        return Ok(false);
    }
    fs.create_dir_all(&dir)?;
    // Rewritten every launch rather than only when absent: the status path
    // baked into the bodies differs between a local run (an absolute host
    // path) and a containerized one (`/workspace/...`), so a file left behind
    // by the other mode would point somewhere that does not exist.
    fs.write(&hooks, &cursor_hooks(agent_status_file))?;
    let ignore = dir.join(".gitignore");
    if !fs.exists(&ignore) {
        fs.write(&ignore, CURSOR_GITIGNORE)?;
    }
    Ok(true)
}

/// Single-quote a path for a POSIX shell, escaping embedded single quotes by
/// closing the quoted string, emitting an escaped literal quote, and
/// reopening it (the standard `'\''` trick). Safe for any byte sequence,
/// including a Windows path's backslashes (backslash has no special meaning
/// inside single quotes).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Build one Codex hook override CLI arg (`--config hooks.<event>=...`) that
/// appends `state` to the absolute `status_file`. The command string is a
/// POSIX-shell one-liner, single-quoted via [`shell_quote`]; the whole TOML
/// value is then serialized through `toml::Value::String`, never hand-quoted,
/// so embedded quotes/backslashes from either layer are escaped correctly.
fn codex_hook_override(event: &str, state: &str, status_file: &str) -> String {
    let command = format!(
        "printf '{state}\\n' >> {}; exit 0",
        shell_quote(status_file)
    );
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

/// The Claude plugin's hook bodies, targeting the absolute `status_file` (and,
/// for the `AskUserQuestion` matcher, the absolute `question_file`).
///
/// Each body is a POSIX-shell one-liner embedded in JSON, so the whole
/// document is built with `serde_json::json!` and rendered with
/// `to_string()` — every string (including the shell-quoted path) is escaped
/// by the serializer, never by hand-writing `format!("\"{path}\"")`. That
/// also makes a Windows path's backslashes safe: `serde_json` escapes them as
/// `\\` in the JSON string, and the JSON parser on the Claude side hands the
/// shell the literal single backslash back, which POSIX single-quoting (see
/// [`shell_quote`]) then passes through unchanged.
///
/// The original bodies guarded on `[ -d .flightdeck ]` because the path was
/// relative to the agent's (arbitrary) working directory, and the hook could
/// otherwise fire in an unrelated project. With an absolute `status_file` that
/// FlightDeck itself creates via `create_dir_all` plus the seeded status file,
/// the directory exists by construction, so the guard is dropped; `>>` failing
/// on a since-vanished directory is already harmless because every body ends
/// `exit 0`.
///
/// Every event transcribed here matches the original `CLAUDE_PLUGIN_HOOKS`
/// constant exactly (SessionStart, UserPromptSubmit, Stop, StopFailure,
/// PermissionRequest, PreToolUse/AskUserQuestion, PostToolUse, and both
/// Notification matchers) — dropping one would silently lose a status
/// transition.
fn claude_plugin_hooks(status_file: &str, question_file: &str) -> String {
    let sf = shell_quote(status_file);
    let qf = shell_quote(question_file);
    let write = |state: &str| -> String { format!("printf '{state}\\n' >> {sf}; exit 0") };
    let hook = |command: String| -> serde_json::Value {
        serde_json::json!([{"hooks": [{"type": "command", "command": command}]}])
    };
    let matched_hook = |matcher: &str, command: String| -> serde_json::Value {
        serde_json::json!([{"matcher": matcher, "hooks": [{"type": "command", "command": command}]}])
    };
    serde_json::json!({
        "description": "FlightDeck agent lifecycle status",
        "hooks": {
            "SessionStart": hook(write("idle")),
            "UserPromptSubmit": hook(write("working")),
            "Stop": hook(write("idle")),
            "StopFailure": hook(write("idle")),
            "PermissionRequest": hook(write("waiting")),
            "PreToolUse": matched_hook(
                "AskUserQuestion",
                format!("cat > {qf}; printf 'waiting\\n' >> {sf}; exit 0"),
            ),
            "PostToolUse": hook(write("working")),
            "Notification": [
                serde_json::json!({"matcher": "elicitation_dialog", "hooks": [{"type": "command", "command": write("waiting")}]}),
                serde_json::json!({"matcher": "idle_prompt", "hooks": [{"type": "command", "command": write("idle")}]}),
            ],
        }
    })
    .to_string()
}

/// Marker embedded in every generated Cursor hook body (as a trailing shell
/// comment, so it never affects execution). It is how
/// [`install_cursor_hooks`] recognises a `.cursor/hooks.json` as FlightDeck's
/// own — and therefore safe to rewrite — versus one the user or their repo
/// owns, which is never touched.
const CURSOR_HOOKS_MARKER: &str = "flightdeck-agent-status";

/// The self-ignoring `.gitignore` FlightDeck drops next to a generated
/// `.cursor/hooks.json`. Ignoring itself as well as the hooks file means the
/// whole generated directory is invisible to `git status`, so the worktree
/// diff stays the agent's work alone. Only ever written when it is absent, so
/// a repo that ships its own `.cursor/.gitignore` keeps it.
const CURSOR_GITIGNORE: &str = "\
# Written by FlightDeck: the launch-scoped Cursor CLI lifecycle hooks that
# report this agent's status. Both entries (this file included) are ignored so
# the generated files never show up in `git status`. Safe to delete —
# FlightDeck writes it again on the next launch.
/hooks.json
/.gitignore
";

/// Cursor CLI's `hooks.json`, targeting the absolute `status_file`.
///
/// Built with `serde_json::json!` and rendered with `to_string()` for the same
/// reason [`claude_plugin_hooks`] is: every string — the shell-quoted path
/// included — is escaped by the serializer rather than by hand, which also
/// makes a Windows path's backslashes safe.
///
/// Three events, chosen deliberately from Cursor's larger set:
///
/// * `beforeSubmitPrompt` → `working` (the turn starts),
/// * `postToolUse` → `working` (still the agent's turn after a tool call),
/// * `stop` → `idle` (the turn ended).
///
/// Three near-neighbours are deliberately **not** wired:
///
/// * `sessionStart` (→ `idle` for the other backends) races
///   `beforeSubmitPrompt` when a prompt is supplied at launch and can land
///   after it, parking a busy agent on `idle`. It is also redundant:
///   [`prepare_status_launch`] seeds `idle` into the status file before spawn.
/// * `afterAgentResponse` fires *after* `stop`, so wiring it to anything would
///   overwrite the `idle` that just landed.
/// * There is no `waiting` state. Cursor exposes no approval-request event;
///   `beforeShellExecution` fires before the approval *decision*, so it fires
///   just the same for a command that is auto-approved and runs for ten
///   minutes. Reporting `waiting` from it would mislabel every long allowed
///   command as blocked-on-the-human, which is worse than not reporting it —
///   so a Cursor tab shows `working` while Cursor asks to run something.
fn cursor_hooks(status_file: &str) -> String {
    let sf = shell_quote(status_file);
    let write = |state: &str| -> serde_json::Value {
        serde_json::json!([{
            "type": "command",
            "command": format!("printf '{state}\\n' >> {sf}; exit 0 # {CURSOR_HOOKS_MARKER}"),
        }])
    };
    serde_json::json!({
        "version": 1,
        "hooks": {
            "beforeSubmitPrompt": write("working"),
            "postToolUse": write("working"),
            "stop": write("idle"),
        }
    })
    .to_string()
}

/// The OpenCode runtime plugin, targeting the absolute `status_file`.
///
/// The path is serialized once through `serde_json::to_string` (never
/// hand-quoted) and substituted for the `__FLIGHTDECK_STATUS_FILE_JSON__`
/// placeholder in [`OPENCODE_RUNTIME_PLUGIN_TEMPLATE`] with a plain string
/// replace — so the rest of the generated source is untouched, and only the
/// one substitution point needs to be reasoned about for escaping. A JSON
/// string is also valid JS string-literal syntax (both use `\"`/`\\`
/// escaping and Unicode `\uXXXX`), so the same serialization is safe to embed
/// directly as a JS literal, including a Windows path's backslashes.
fn opencode_runtime_plugin(status_file: &str) -> String {
    let status_file_js = serde_json::to_string(status_file).expect("string always serializes");
    OPENCODE_RUNTIME_PLUGIN_TEMPLATE.replace("__FLIGHTDECK_STATUS_FILE_JSON__", &status_file_js)
}

const OPENCODE_RUNTIME_PLUGIN_TEMPLATE: &str = r#"// Generated by FlightDeck. Explicit lifecycle state only; no terminal heuristics.
import { appendFileSync, existsSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

const FLIGHTDECK_STATUS_FILE = __FLIGHTDECK_STATUS_FILE_JSON__;

export const FlightDeckStatus = async () => {
  const fdDir = dirname(FLIGHTDECK_STATUS_FILE);
  const write = (state) => {
    try {
      if (existsSync(fdDir)) appendFileSync(FLIGHTDECK_STATUS_FILE, state + "\n");
    } catch (_) {}
  };
  // Serialize the structured prompt so FlightDeck can offer real options on the
  // phone. OpenCode's event.properties schema is not formally documented, so
  // probe the likely field names defensively; an empty options array makes the
  // desktop reader fall back to the binary allow/deny prompt.
  const writePrompt = (event) => {
    try {
      if (!existsSync(fdDir)) return;
      // Capture the raw event so the exact schema can be inspected/parsed even
      // if the normalization below misses a field (remote-control-qa1). Not
      // consumed by the desktop; overwritten each prompt.
      try {
        writeFileSync(
          join(fdDir, "agent-prompt.debug.json"),
          JSON.stringify({ type: event.type, properties: event.properties }, null, 2),
        );
      } catch (_) {}
      const p = event.properties || {};
      const kind = event.type === "question.asked" ? "question" : "permission";
      // OpenCode's question payload uses `questions[]` (each with `options`/
      // `choices` of `{label,value,hint}`) and a `multiple` flag; a permission
      // uses top-level `title`/`options`. Probe both, and the nested `question`
      // object, so real options reach the phone instead of an empty card.
      const q = Array.isArray(p.questions)
        ? p.questions[0] || {}
        : p.question && typeof p.question === "object"
          ? p.question
          : p;
      const text =
        q.question ?? q.title ?? q.header ?? p.title ?? p.text ?? p.prompt ?? "";
      const rawOpts = Array.isArray(q.options)
        ? q.options
        : Array.isArray(q.choices)
          ? q.choices
          : Array.isArray(p.options)
            ? p.options
            : Array.isArray(p.choices)
              ? p.choices
              : [];
      const options = rawOpts.map((o) =>
        o && typeof o === "object"
          ? {
              label: String(o.label ?? o.title ?? o.value ?? o.text ?? ""),
              description: o.hint ?? o.description ?? o.detail ?? undefined,
            }
          : { label: String(o) },
      );
      const multiple = Boolean(q.multiple ?? q.multiSelect ?? p.multiple ?? false);
      writeFileSync(
        join(fdDir, "agent-prompt.json"),
        JSON.stringify({ kind, text: String(text), options, multiple }),
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
        ("cursor-hooks.json", CURSOR_HOOKS),
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

## Cursor CLI

Merge `cursor-hooks.json` into your **user** hooks at `~/.cursor/hooks.json`
(or this project's `.cursor/hooks.json`). It wires `beforeSubmitPrompt` and
`postToolUse` → `working` and `stop` → `idle`.

Cursor reports no `waiting`: it has no approval-request event, and the shell
hook that fires before an approval also fires before every auto-approved
command, so using it would mark long allowed commands as blocked on you. A
Cursor tab therefore stays `working` while Cursor asks to run something.

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
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && { cat > \"$r/.flightdeck/agent-question.json\" 2>/dev/null; printf 'waiting\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; }; exit 0"
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

/// Cursor CLI `~/.cursor/hooks.json` lifecycle hooks. Cursor exports
/// `CURSOR_PROJECT_DIR` (the workspace root) to every hook command, with
/// `$PWD` as the fallback for an older build that does not.
const CURSOR_HOOKS: &str = r##"{
  "version": 1,
  "hooks": {
    "beforeSubmitPrompt": [
      {
        "type": "command",
        "command": "r=\"${CURSOR_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'working\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
      }
    ],
    "postToolUse": [
      {
        "type": "command",
        "command": "r=\"${CURSOR_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'working\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
      }
    ],
    "stop": [
      {
        "type": "command",
        "command": "r=\"${CURSOR_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'idle\\n' >> \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
      }
    ]
  }
}
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
    fn status_launch_writes_only_under_the_status_root() {
        let fs = FakeFs::new();
        let agent = agent("claude", "claude");

        let launch = prepare_status_launch(
            &fs,
            &agent,
            Path::new("/repo"),
            Path::new("/tmp/fd-isolated-1"),
            false,
        )
        .unwrap();

        assert!(
            fs.writes_under(Path::new("/repo")).is_empty(),
            "nothing may land in the project: {:?}",
            fs.writes_under(Path::new("/repo"))
        );
        assert!(
            !fs.writes_under(Path::new("/tmp/fd-isolated-1")).is_empty(),
            "the plumbing goes to the status root instead"
        );
        assert!(
            launch.explicit,
            "a known backend still reports explicit status"
        );
        assert!(
            launch.args.iter().any(|a| a.contains("/tmp/fd-isolated-1")),
            "the plugin dir handed to the agent points at the status root: {:?}",
            launch.args
        );
    }

    #[test]
    fn claude_hooks_target_the_absolute_status_file() {
        let fs = FakeFs::new();
        let agent = agent("claude", "claude");
        prepare_status_launch(
            &fs,
            &agent,
            Path::new("/repo"),
            Path::new("/tmp/root"),
            false,
        )
        .unwrap();

        let hooks = fs
            .file_contents(Path::new(
                "/tmp/root/.flightdeck/runtime/status/claude/hooks/hooks.json",
            ))
            .expect("hooks.json written under the status root");
        // Compare against the DECODED command, not the raw file text, and
        // derive the expected path with the same `Path::join` the code uses.
        // Both matter for cross-platform parity: on Windows `join` yields
        // backslashes, and JSON escapes each one as `\\`, so a raw
        // `contains` of either a forward-slash literal or a native path string
        // fails there while the hook is perfectly correct.
        let doc: serde_json::Value = serde_json::from_str(&hooks)
            .unwrap_or_else(|e| panic!("the templated hooks must be valid JSON: {e}: {hooks}"));
        let expected = Path::new("/tmp/root")
            .join(".flightdeck")
            .join("agent-status")
            .to_string_lossy()
            .to_string();
        let stop = doc["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap_or_else(|| panic!("Stop hook command missing: {hooks}"));
        assert!(
            stop.contains(&expected),
            "hook bodies must carry the absolute status path, not a cwd-relative one.\n\
             expected to find: {expected}\n\
             in command: {stop}"
        );
    }

    #[test]
    fn codex_hook_override_quotes_the_absolute_path_as_toml() {
        let ov = codex_hook_override("Stop", "idle", "/tmp/root/.flightdeck/agent-status");
        assert!(ov.contains("/tmp/root/.flightdeck/agent-status"));
        assert!(
            toml::from_str::<toml::Value>(&ov.replace("hooks.Stop=", "x=")).is_ok(),
            "the override must be parseable TOML: {ov}"
        );
    }

    #[test]
    fn status_root_equal_to_the_worktree_is_todays_behavior() {
        // Regression guard for the normal path.
        let fs = FakeFs::new();
        let agent = agent("claude", "claude");
        prepare_status_launch(&fs, &agent, Path::new("/repo"), Path::new("/repo"), false).unwrap();

        assert!(
            fs.exists(Path::new("/repo/.flightdeck/agent-status")),
            "the seeded status file still lands in the worktree"
        );
        assert!(
            fs.exists(Path::new(
                "/repo/.flightdeck/runtime/status/claude/hooks/hooks.json"
            )),
            "so does the plugin"
        );
    }

    #[test]
    fn stale_status_root_contents_are_cleaned_before_a_fresh_run() {
        // A pid-based isolated temp dir can be recycled from a crashed earlier
        // run. Leftover files this run does not itself rewrite (e.g. a
        // different backend's plugin dir) must not survive into the fresh run.
        let fs = FakeFs::new().with_file(
            "/tmp/root/.flightdeck/runtime/status/opencode/plugins/flightdeck.js",
            "stale leftover from a crashed earlier run",
        );
        let agent = agent("codex", "codex");
        prepare_status_launch(
            &fs,
            &agent,
            Path::new("/repo"),
            Path::new("/tmp/root"),
            false,
        )
        .unwrap();

        assert!(
            fs.file_contents(Path::new(
                "/tmp/root/.flightdeck/runtime/status/opencode/plugins/flightdeck.js"
            ))
            .is_none(),
            "a stale directory left by a crashed earlier run must be cleaned, not merged into"
        );
    }

    #[test]
    fn status_root_equal_to_the_worktree_is_never_wiped() {
        // Guard against the cleanup step ever touching the worktree: a normal
        // (non-isolated) run must never have its status_root nuked just because
        // it happens to already contain a stale status file from a prior spawn.
        let fs = FakeFs::new().with_file(
            "/repo/some-unrelated-project-file.txt",
            "must survive prepare_status_launch",
        );
        let agent = agent("claude", "claude");
        prepare_status_launch(&fs, &agent, Path::new("/repo"), Path::new("/repo"), false).unwrap();

        assert_eq!(
            fs.file_contents(Path::new("/repo/some-unrelated-project-file.txt")),
            Some("must survive prepare_status_launch".to_string()),
            "status_root == worktree must never be wiped"
        );
    }

    #[test]
    fn prepares_claude_plugin_without_replacing_existing_args() {
        let fs = FakeFs::new();
        let launch = prepare_status_launch(
            &fs,
            &agent("claude", "claude"),
            Path::new(REPO),
            Path::new(REPO),
            false,
        )
        .unwrap();
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
        let pre_cmd = pre["hooks"][0]["command"].as_str().unwrap_or_default();
        assert!(pre_cmd.contains("waiting"), "must flip status to waiting");
        // Must also capture the hook's stdin (the AskUserQuestion tool_input) to
        // the question sidecar the bridge reads, so the phone gets the real
        // question deterministically instead of a racing binary card (qa1).
        assert!(
            pre_cmd.contains("agent-question.json") && pre_cmd.contains("cat >"),
            "PreToolUse must pipe stdin to the question sidecar"
        );
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
        let launch = prepare_status_launch(
            &fs,
            &agent("codex", "codex"),
            Path::new(REPO),
            Path::new(REPO),
            false,
        )
        .unwrap();
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
        let launch = prepare_status_launch(
            &fs,
            &agent("opencode", "opencode"),
            Path::new(REPO),
            Path::new(REPO),
            true,
        )
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
        // Regression guard: the hook body must carry the *container-internal*
        // path, not a host path the agent cannot see once inside the
        // bind-mounted container (`/workspace` is `WORKDIR`).
        assert!(
            plugin.contains("/workspace/.flightdeck/agent-status"),
            "containerized OpenCode plugin must target the bind-mounted path: {plugin}"
        );
        assert!(
            !plugin.contains(REPO),
            "containerized OpenCode plugin must never carry the host status root: {plugin}"
        );
    }

    #[test]
    fn claude_hooks_use_the_container_path_when_containerized() {
        let fs = FakeFs::new();
        prepare_status_launch(
            &fs,
            &agent("claude", "claude"),
            Path::new(REPO),
            Path::new(REPO),
            true,
        )
        .unwrap();
        let hooks = fs
            .file_contents(Path::new(
                "/repo/.flightdeck/runtime/status/claude/hooks/hooks.json",
            ))
            .unwrap();
        assert!(
            hooks.contains("/workspace/.flightdeck/agent-status"),
            "containerized Claude hooks must target the bind-mounted path: {hooks}"
        );
        assert!(
            hooks.contains("/workspace/.flightdeck/agent-question.json"),
            "the AskUserQuestion sidecar path must also be the bind-mounted one: {hooks}"
        );
        assert!(
            !hooks.contains(REPO),
            "containerized Claude hooks must never carry the host status root: {hooks}"
        );
    }

    #[test]
    fn codex_hook_override_uses_the_container_path_when_containerized() {
        let fs = FakeFs::new();
        let launch = prepare_status_launch(
            &fs,
            &agent("codex", "codex"),
            Path::new(REPO),
            Path::new(REPO),
            true,
        )
        .unwrap();
        let overrides: Vec<&String> = launch
            .args
            .iter()
            .filter(|a| a.starts_with("hooks.Stop="))
            .collect();
        assert_eq!(overrides.len(), 1);
        assert!(
            overrides[0].contains("/workspace/.flightdeck/agent-status"),
            "containerized Codex override must target the bind-mounted path: {:?}",
            overrides[0]
        );
        assert!(
            !overrides[0].contains(REPO),
            "containerized Codex override must never carry the host status root: {:?}",
            overrides[0]
        );
    }

    #[test]
    fn unknown_agent_fails_closed_without_generating_runtime_files() {
        let fs = FakeFs::new();
        let launch = prepare_status_launch(
            &fs,
            &agent("custom", "other"),
            Path::new(REPO),
            Path::new(REPO),
            false,
        )
        .unwrap();
        assert!(!launch.explicit);
        assert!(launch.env.is_empty());
        assert_eq!(launch.args, vec!["--existing"]);
        assert!(!fs.exists(Path::new("/repo/.flightdeck/agent-status")));
    }

    // -------------------------------------------------------------------------
    // Cursor CLI
    // -------------------------------------------------------------------------

    /// Parse a generated `.cursor/hooks.json` and return each event's single
    /// command body, keyed by event name.
    fn cursor_commands(json: &str) -> std::collections::BTreeMap<String, String> {
        let value: serde_json::Value = serde_json::from_str(json)
            .unwrap_or_else(|e| panic!("invalid hooks.json: {e}\n{json}"));
        assert_eq!(value["version"], 1, "Cursor's hooks file is schema v1");
        value["hooks"]
            .as_object()
            .expect("hooks object")
            .iter()
            .map(|(event, entries)| {
                let entries = entries.as_array().expect("hook entries are an array");
                assert_eq!(entries.len(), 1, "one command per event");
                assert_eq!(entries[0]["type"], "command");
                (
                    event.clone(),
                    entries[0]["command"].as_str().expect("command").to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn status_backend_recognises_the_cursor_cli_but_not_the_editor_launcher() {
        assert_eq!(
            status_backend(&agent("cursor", "cursor-agent")),
            Some(StatusBackend::Cursor)
        );
        assert_eq!(
            status_backend(&agent("cursor", "/opt/cursor/bin/cursor-agent")),
            Some(StatusBackend::Cursor)
        );
        assert_eq!(
            status_backend(&agent("cursor", "C:\\tools\\cursor-agent.cmd")),
            if cfg!(windows) {
                Some(StatusBackend::Cursor)
            } else {
                None
            }
        );
        // `cursor` launches the Cursor editor, not the CLI agent: passing it
        // agent flags would be wrong, so it stays unrecognised.
        assert_eq!(status_backend(&agent("cursor", "cursor")), None);
    }

    #[test]
    fn prepares_cursor_project_hooks_and_hides_them_from_git() {
        let fs = FakeFs::new();
        let launch = prepare_status_launch(
            &fs,
            &agent("cursor", "cursor-agent"),
            Path::new(REPO),
            Path::new(REPO),
            false,
        )
        .unwrap();
        assert!(launch.explicit);
        // Cursor needs no extra flags or environment — the hooks file is the
        // whole integration.
        assert_eq!(launch.args, vec!["--existing".to_string()]);
        assert!(launch.env.is_empty());

        let hooks = fs
            .file_contents(Path::new("/repo/.cursor/hooks.json"))
            .expect("cursor hooks written into the worktree");
        let commands = cursor_commands(&hooks);
        assert_eq!(
            commands.keys().cloned().collect::<Vec<_>>(),
            vec![
                "beforeSubmitPrompt".to_string(),
                "postToolUse".to_string(),
                "stop".to_string()
            ]
        );
        assert!(commands["beforeSubmitPrompt"].contains("printf 'working"));
        assert!(commands["postToolUse"].contains("printf 'working"));
        assert!(commands["stop"].contains("printf 'idle"));
        for command in commands.values() {
            assert!(
                command.contains("/repo/.flightdeck/agent-status"),
                "hook must target the seeded status file: {command}"
            );
            assert!(
                command.contains(CURSOR_HOOKS_MARKER),
                "hook must carry the ownership marker: {command}"
            );
        }
        // `sessionStart` races `beforeSubmitPrompt` and `afterAgentResponse`
        // fires after `stop`; wiring either would park a busy agent on the
        // wrong state.
        assert!(!hooks.contains("sessionStart"));
        assert!(!hooks.contains("afterAgentResponse"));

        let ignore = fs
            .file_contents(Path::new("/repo/.cursor/.gitignore"))
            .expect("self-ignoring .gitignore written alongside");
        assert!(ignore.lines().any(|l| l.trim() == "/hooks.json"));
        assert!(
            ignore.lines().any(|l| l.trim() == "/.gitignore"),
            "the ignore file must ignore itself, or it shows up in git status"
        );
    }

    #[test]
    fn cursor_hooks_use_the_container_path_when_containerized() {
        let fs = FakeFs::new();
        prepare_status_launch(
            &fs,
            &agent("cursor", "cursor-agent"),
            Path::new(REPO),
            Path::new(REPO),
            true,
        )
        .unwrap();
        let hooks = fs
            .file_contents(Path::new("/repo/.cursor/hooks.json"))
            .unwrap();
        assert!(
            hooks.contains("/workspace/.flightdeck/agent-status"),
            "containerized Cursor hooks must target the bind-mounted path: {hooks}"
        );
        assert!(
            !hooks.contains(&format!("{REPO}/.flightdeck")),
            "containerized Cursor hooks must never carry the host status root: {hooks}"
        );
    }

    #[test]
    fn cursor_hooks_are_rewritten_when_the_status_path_changes() {
        // Switching a worktree between local and containerized execution moves
        // the status file the bodies must target. FlightDeck owns the file (it
        // carries the marker), so it is rewritten rather than left stale.
        let fs = FakeFs::new();
        let a = agent("cursor", "cursor-agent");
        prepare_status_launch(&fs, &a, Path::new(REPO), Path::new(REPO), false).unwrap();
        let launch =
            prepare_status_launch(&fs, &a, Path::new(REPO), Path::new(REPO), true).unwrap();
        assert!(launch.explicit);
        let hooks = fs
            .file_contents(Path::new("/repo/.cursor/hooks.json"))
            .unwrap();
        assert!(hooks.contains("/workspace/.flightdeck/agent-status"));
        assert!(!hooks.contains(&format!("{REPO}/.flightdeck")));
    }

    #[test]
    fn cursor_never_overwrites_a_hooks_file_it_does_not_own() {
        let fs = FakeFs::new();
        let theirs = r#"{"version":1,"hooks":{"stop":[{"type":"command","command":"make lint"}]}}"#;
        fs.write(Path::new("/repo/.cursor/hooks.json"), theirs)
            .unwrap();

        let launch = prepare_status_launch(
            &fs,
            &agent("cursor", "cursor-agent"),
            Path::new(REPO),
            Path::new(REPO),
            false,
        )
        .unwrap();

        assert_eq!(
            fs.file_contents(Path::new("/repo/.cursor/hooks.json"))
                .unwrap(),
            theirs,
            "a repo's own Cursor hooks are never rewritten"
        );
        assert!(
            fs.file_contents(Path::new("/repo/.cursor/.gitignore"))
                .is_none(),
            "no .gitignore is planted next to a file FlightDeck did not write — \
             it would quietly hide the user's own tracked hooks"
        );
        assert!(
            !launch.explicit,
            "without its own hooks Cursor never reports the end of a turn, so \
             the tab must fall back to neutral rather than stick on 'working'"
        );
    }

    #[test]
    fn cursor_writes_nothing_under_the_project_on_an_isolated_run() {
        // An isolated run redirects the status root out of the project and
        // promises no FlightDeck-initiated writes under it. Cursor's hooks can
        // only live in the agent's own workspace, so there is nowhere legal to
        // put them: the run gets neutral status instead.
        let fs = FakeFs::new();
        let launch = prepare_status_launch(
            &fs,
            &agent("cursor", "cursor-agent"),
            Path::new(REPO),
            Path::new("/tmp/flightdeck-isolated-1"),
            false,
        )
        .unwrap();
        assert!(!launch.explicit);
        assert!(fs
            .file_contents(Path::new("/repo/.cursor/hooks.json"))
            .is_none());
        assert!(fs
            .file_contents(Path::new("/repo/.cursor/.gitignore"))
            .is_none());
    }

    #[test]
    fn writes_all_artifacts_and_gitignore_entry() {
        let fs = FakeFs::new();
        let report = write_status_integrations(&fs, Path::new(REPO)).unwrap();

        assert_eq!(report.written.len(), 5);
        assert!(report.gitignore_added);

        for name in [
            "README.md",
            "claude-code.settings.json",
            "codex-config.toml",
            "opencode-flightdeck.js",
            "cursor-hooks.json",
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
                OPENCODE_RUNTIME_PLUGIN_TEMPLATE.contains(event) && OPENCODE_PLUGIN.contains(event),
                "all OpenCode bridges must handle {event}"
            );
        }
        // Both OpenCode plugins must serialize the structured prompt (question
        // text + options) to the sidecar the desktop reads, overwriting it each
        // time (writeFileSync, not appendFileSync) so a stale prompt never
        // lingers, and must invoke that capture on the asked events.
        for plugin in [OPENCODE_RUNTIME_PLUGIN_TEMPLATE, OPENCODE_PLUGIN] {
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
