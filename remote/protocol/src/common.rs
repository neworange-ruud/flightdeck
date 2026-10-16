//! Shared value types used by both protocol planes: version constants, roles,
//! agent identity/status, and git status detail.

use serde::{Deserialize, Serialize};

use crate::ids::SessionId;

/// Protocol version this build speaks and prefers.
///
/// v3 adds multi-select (checklist) questions: `PermissionPrompt.multi_select`
/// and `PermissionDecision.option_indices`. Both are `#[serde(default)]`, so v2
/// peers still parse v3 frames (missing fields → single-select).
///
/// v3 also carries multi-QUESTION forms additively: `PermissionPrompt.questions`
/// (a list of `PromptQuestion`, each with its own options + `multi_select`) and
/// `PermissionDecision.answers` (a per-question `QuestionAnswer` list). Both are
/// `#[serde(default)]` and the flat single-question fields keep mirroring the
/// first question, so this needed no version bump of its own
/// (remote-control-ssw).
///
/// **v4 (epic remote-control-rjrz): desktop-decided encrypted push, the direct
/// link, and relay modes.** v4 adds a frame an older relay cannot parse —
/// [`crate::relay::RelayFrame::PushNotify`] — which is exactly the
/// non-backward-compatible case the v3 note above reserved a bump for. A
/// desktop sends `push_notify` only on a connection negotiated at
/// [`PUSH_NOTIFY_MIN_VERSION`] or above (an older relay still drives its own
/// per-envelope wake pushes). Every other v4 addition is an optional,
/// `skip_serializing_if = "Option::is_none"` field
/// (`hello_ok.relay_mode`, `pairing_offer.direct_endpoint`,
/// `pairing_claimed.direct_endpoint`, `pairing_claimed.peer_device_public_key`),
/// so v1–v3 JSON still parses and [`MIN_SUPPORTED_VERSION`] stays 1.
pub const PROTOCOL_VERSION: u16 = 4;
/// Oldest protocol version this build can still interoperate with.
pub const MIN_SUPPORTED_VERSION: u16 = 1;
/// Newest protocol version this build can interoperate with.
pub const MAX_SUPPORTED_VERSION: u16 = 4;
/// The first negotiated version at which the relay understands
/// [`crate::relay::RelayFrame::PushNotify`]. A desktop whose relay connection
/// negotiated below this must not send `push_notify` (the relay would answer
/// `error { code: bad_frame }`); such a relay still sends its own legacy wake
/// pushes.
pub const PUSH_NOTIFY_MIN_VERSION: u16 = 4;

/// The two roles that connect to the relay for a given pairing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The FlightDeck desktop app (holds the agents and worktrees).
    Desktop,
    /// The FlightDeck Remote iOS app.
    Phone,
}

/// The kind of agent event a push notification announces (spec §5.5, §7.2).
///
/// It is the **only** plaintext hint a push carries past the relay: the relay
/// uses it to pick the APNs interruption level and the generic fallback alert
/// text, and it is repeated inside the sealed [`crate::e2e::PushPayload`].
/// Shared by [`crate::relay::PushNotifyBody`] and the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushClass {
    /// An agent stopped to ask the human (permission prompt or question).
    /// Wire value: `needs_input`.
    NeedsInput,
    /// An agent hit an error. Wire value: `error`.
    Error,
    /// An agent settled idle after its turn (desktop-side debounce). Wire
    /// value: `done`.
    Done,
}

impl PushClass {
    /// The `snake_case` wire spelling — identical to the serde form. Used where
    /// the class is embedded in a canonical string (the HTTPS push signing
    /// message, spec §5.5) rather than serialized as JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            PushClass::NeedsInput => "needs_input",
            PushClass::Error => "error",
            PushClass::Done => "done",
        }
    }
}

impl std::fmt::Display for PushClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which agent CLI backs a session. Matches FlightDeck's supported agents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentType {
    /// Anthropic Claude Code. Wire value: `claude_code`.
    ClaudeCode,
    /// OpenCode. Wire value: `opencode`.
    Opencode,
    /// Codex CLI. Wire value: `codex`.
    Codex,
    /// Cursor CLI (`cursor-agent`). Wire value: `cursor`.
    Cursor,
}

/// FlightDeck's four agent states. `Manual` carries the user-set label and is
/// the cyan override that clears on the next real state change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AgentStatus {
    /// Red spinner: the agent is actively running a turn.
    Working,
    /// Green: the turn is done; waiting for a prompt.
    Idle,
    /// Orange glow: stopped, asking the human (permission / question).
    NeedsInput,
    /// Cyan: user-flagged manual override with a short label.
    Manual {
        /// The label the user set, shown verbatim.
        label: String,
    },
}

/// Which status dominates a project's roll-up dot. Precedence, high to low:
/// needs-input > working > manual > idle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollupDot {
    /// At least one agent needs input (orange).
    NeedsInput,
    /// At least one agent is working, none need input (red).
    Working,
    /// A manual override is the most notable state (cyan).
    Manual,
    /// Everything is idle/done (green/dim).
    Idle,
}

/// Compact git indicators shown on a session row (design: `~3 drift:2`,
/// `+12 ~4`, `clean`, `no-upstream`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitIndicators {
    /// Branch name, if the worktree has one checked out.
    pub branch: Option<String>,
    /// Count of added (new) files (`+`).
    pub added: u32,
    /// Count of modified files (`~`).
    pub modified: u32,
    /// Count of removed files (`-`).
    pub removed: u32,
    /// Commits ahead of upstream.
    pub ahead: u32,
    /// Commits behind upstream.
    pub behind: u32,
    /// Commits of drift from the base branch.
    pub drift: u32,
    /// Whether the branch has an upstream (`false` renders `no-upstream`).
    pub has_upstream: bool,
}

impl GitIndicators {
    /// True when there are no uncommitted file changes (renders `clean`).
    pub fn is_clean(&self) -> bool {
        self.added == 0 && self.modified == 0 && self.removed == 0
    }
}

/// Per-file change kind in a full git status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitFileStatus {
    /// Staged/tracked new file.
    Added,
    /// Tracked, modified.
    Modified,
    /// Tracked, deleted.
    Deleted,
    /// Renamed (path is the new path).
    Renamed,
    /// Present but not tracked.
    Untracked,
}

/// One changed file in a git status detail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFileChange {
    /// Repo-relative path (new path for renames).
    pub path: String,
    /// The change kind.
    pub status: GitFileStatus,
    /// Lines added in this file.
    pub added_lines: u32,
    /// Lines removed in this file.
    pub removed_lines: u32,
}

/// Full, read-only git status for a session's worktree (design §5.5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatusDetail {
    /// The session this status describes.
    pub session_id: SessionId,
    /// Current branch.
    pub branch: Option<String>,
    /// Base branch the worktree was created from.
    pub base_branch: Option<String>,
    /// Whether the branch has an upstream.
    pub has_upstream: bool,
    /// Commits ahead of upstream.
    pub ahead: u32,
    /// Commits behind upstream.
    pub behind: u32,
    /// Commits of drift from base.
    pub drift: u32,
    /// The changed files.
    pub files: Vec<GitFileChange>,
}

#[cfg(test)]
mod tests {
    use super::{AgentType, PushClass};

    #[test]
    fn push_class_as_str_matches_serde() {
        for (class, wire) in [
            (PushClass::NeedsInput, "needs_input"),
            (PushClass::Error, "error"),
            (PushClass::Done, "done"),
        ] {
            assert_eq!(class.as_str(), wire);
            assert_eq!(class.to_string(), wire);
            assert_eq!(serde_json::to_value(class).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<PushClass>(serde_json::json!(wire)).unwrap(),
                class
            );
        }
    }

    #[test]
    fn agent_type_wire_values_are_stable() {
        // These strings are the wire contract with the iOS app, which mirrors
        // the enum in Swift. Renaming one silently breaks every paired phone.
        for (ty, wire) in [
            (AgentType::ClaudeCode, "\"claude_code\""),
            (AgentType::Opencode, "\"opencode\""),
            (AgentType::Codex, "\"codex\""),
            (AgentType::Cursor, "\"cursor\""),
        ] {
            assert_eq!(serde_json::to_string(&ty).unwrap(), wire);
            assert_eq!(serde_json::from_str::<AgentType>(wire).unwrap(), ty);
        }
    }
}
