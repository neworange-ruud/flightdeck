//! `flightdeck setup-status` — install opt-in precise agent status hooks/plugin
//! (SPECS §24, Layer 2).
//!
//! FlightDeck's universal idle/working detection works with **no** agent
//! configuration: it watches PTY output activity. This module installs the
//! *optional* precise layer — per-tool hooks/plugins that write an explicit
//! status keyword to each worktree's [`agent_status_file`] so FlightDeck can show
//! "waiting for input" / "needs attention" / "completed" instead of inferring
//! only idle vs working.
//!
//! [`agent_status_file`]: crate::app::state::agent_status_file
//!
//! ## Design
//!
//! Every artifact writes one keyword (`working` / `idle` / `waiting`) to
//! `<worktree>/.flightdeck/agent-status`, which is exactly the path FlightDeck
//! polls (it derives the same path from the worktree, so no value needs to be
//! injected into the agent). The shell hooks are **self-contained one-liners**
//! (no external script file) so they work inside every Git worktree without
//! needing a committed helper, and they are **gated on `.flightdeck/` existing**
//! so they only write inside FlightDeck-managed worktrees — running the same
//! agent in an unrelated project writes nothing.
//!
//! The artifacts are written into `<repo>/.flightdeck/integrations/` for the user
//! to wire into each tool's (user-global) config; see the generated README.

use crate::contracts::{FileSystem, Result};
use crate::fs::ignore::{ensure_gitignore_entry, STATUS_IGNORE_ENTRY};
use std::path::{Path, PathBuf};

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
sidebar. The **baseline** detection needs no setup — FlightDeck watches each
agent's terminal output and marks a tab `working` while output is flowing and
`idle` once it falls quiet.

These optional integrations make the status **precise**: each agent writes an
explicit keyword to `<worktree>/.flightdeck/agent-status` when it starts a turn,
finishes, or needs your input, so FlightDeck can show `waiting` / `completed`
exactly rather than inferring from silence.

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
- `Notification` → `waiting`

The hooks write nothing to stdout, so they never disturb the session or get
injected into Claude's context.

## Codex CLI

Append the contents of `codex-config.toml` to your **user** config at
`~/.codex/config.toml` (Codex only honours hooks/notify in the user-level file).
It wires `UserPromptSubmit` → `working` and `Stop` → `idle`. A `notify`
fallback (idle-only, for older Codex) is included as a comment.

## OpenCode

Copy `opencode-flightdeck.js` to `~/.config/opencode/plugin/flightdeck.js`
(global) — or to `.opencode/plugin/` in your project. It maps `session.idle` →
`idle`, the first message activity → `working`, and permission prompts →
`waiting`.

---

After wiring, restart the agent in a tab (Ctrl-r). The tab status should switch
to `waiting` the moment the agent asks for confirmation, and to `idle` the
moment it finishes — without waiting for the output-silence timeout.
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
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'working\\n' > \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'idle\\n' > \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
          }
        ]
      }
    ],
    "StopFailure": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'idle\\n' > \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
          }
        ]
      }
    ],
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'idle\\n' > \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
          }
        ]
      }
    ],
    "Notification": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "r=\"${CLAUDE_PROJECT_DIR:-$PWD}\"; [ -d \"$r/.flightdeck\" ] && printf 'waiting\\n' > \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"
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
command = "r=\"$PWD\"; [ -d \"$r/.flightdeck\" ] && printf 'working\\n' > \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"

[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "r=\"$PWD\"; [ -d \"$r/.flightdeck\" ] && printf 'idle\\n' > \"$r/.flightdeck/agent-status\" 2>/dev/null; exit 0"

# Fallback for older Codex without lifecycle hooks (idle only, fires on
# agent-turn-complete). `notify` is honoured ONLY in the user-level config.
# notify = ["sh", "-c", "r=\"$PWD\"; [ -d \"$r/.flightdeck\" ] && printf 'idle\\n' > \"$r/.flightdeck/agent-status\"; exit 0", "flightdeck-notify"]
"##;

/// OpenCode plugin (plain JS, no type imports so it works as a global plugin).
const OPENCODE_PLUGIN: &str = r#"// FlightDeck agent status plugin for OpenCode.
// Install globally: copy to ~/.config/opencode/plugin/flightdeck.js
// or per-project:    copy to .opencode/plugin/flightdeck.js
//
// Writes one of working/idle/waiting to <worktree>/.flightdeck/agent-status,
// which FlightDeck polls. Gated on .flightdeck/ existing, so it is a no-op
// outside FlightDeck worktrees.
import { writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";

export const FlightDeck = async ({ directory, worktree }) => {
  const root = worktree || directory;
  const fdDir = join(root, ".flightdeck");
  const write = (state) => {
    try {
      if (existsSync(fdDir)) writeFileSync(join(fdDir, "agent-status"), state + "\n");
    } catch (_) {
      /* never let status writing break the session */
    }
  };

  let working = false;
  return {
    event: async ({ event }) => {
      // Agent finished its turn.
      if (event.type === "session.idle") {
        working = false;
        write("idle");
        return;
      }
      // Needs the user's attention (permission / confirmation prompt).
      if (event.type === "permission.asked" || event.type === "permission.updated") {
        write("waiting");
        return;
      }
      // First message activity after idle => started working.
      if (
        !working &&
        (event.type === "message.updated" ||
          event.type === "message.part.updated" ||
          event.type === "session.created")
      ) {
        working = true;
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
