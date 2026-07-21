//! Shared value types used by both protocol planes: version constants, roles,
//! agent identity/status, and git status detail.

use serde::{Deserialize, Serialize};

use crate::ids::SessionId;

/// Protocol version this build speaks and prefers.
pub const PROTOCOL_VERSION: u16 = 2;
/// Oldest protocol version this build can still interoperate with.
pub const MIN_SUPPORTED_VERSION: u16 = 1;
/// Newest protocol version this build can interoperate with.
pub const MAX_SUPPORTED_VERSION: u16 = 2;

/// The two roles that connect to the relay for a given pairing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The FlightDeck desktop app (holds the agents and worktrees).
    Desktop,
    /// The FlightDeck Remote iOS app.
    Phone,
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
