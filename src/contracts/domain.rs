//! Shared domain types (SPECS §8, §9, §24).
//!
//! These are the contract every service module is written against. They are
//! main-agent-owned: subagents must not change their shapes (report friction
//! instead). State-persisted types store **relative** paths (SPECS §9).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Stable identifier for an Agent Tab.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TabId(pub String);

// ---------------------------------------------------------------------------
// Status model (SPECS §24)
// ---------------------------------------------------------------------------

/// Lifecycle of the OS process backing a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// Not yet launched.
    NotStarted,
    /// Launch requested, not yet confirmed running.
    Starting,
    /// Running.
    Running,
    /// Stopped (e.g. via Ctrl-C) but the tab still exists.
    Stopped,
    /// Exited with the given status code.
    Exited(i32),
    /// Failed to start.
    Failed,
    /// The live session was lost (e.g. after restart, before relaunch).
    Lost,
}

impl ProcessState {
    /// Short human-readable label, e.g. `"running"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            ProcessState::NotStarted => "not started",
            ProcessState::Starting => "starting",
            ProcessState::Running => "running",
            ProcessState::Stopped => "stopped",
            ProcessState::Exited(_) => "exited",
            ProcessState::Failed => "failed",
            ProcessState::Lost => "lost",
        }
    }
}

/// Status interpreted from process state + output pattern matching (SPECS §24).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpretedStatus {
    Starting,
    Running,
    /// Actively producing output — the agent is working on something.
    Working,
    /// Process is up but quiet — finished a turn / waiting on the user.
    Idle,
    WaitingForInput,
    NeedsAttention,
    Completed,
    Failed,
    Stopped,
    SessionLost,
    Recovered,
    Unknown,
}

impl InterpretedStatus {
    /// Short human-readable label.
    pub fn as_str(&self) -> &'static str {
        match self {
            InterpretedStatus::Starting => "starting",
            InterpretedStatus::Running => "running",
            InterpretedStatus::Working => "working",
            InterpretedStatus::Idle => "idle",
            InterpretedStatus::WaitingForInput => "waiting",
            InterpretedStatus::NeedsAttention => "needs attention",
            InterpretedStatus::Completed => "completed",
            InterpretedStatus::Failed => "failed",
            InterpretedStatus::Stopped => "stopped",
            InterpretedStatus::SessionLost => "session lost",
            InterpretedStatus::Recovered => "recovered",
            InterpretedStatus::Unknown => "unknown",
        }
    }

    /// Parse the label produced by [`InterpretedStatus::as_str`]. Unknown labels
    /// map to [`InterpretedStatus::Unknown`].
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "starting" => InterpretedStatus::Starting,
            "running" => InterpretedStatus::Running,
            "working" => InterpretedStatus::Working,
            "idle" => InterpretedStatus::Idle,
            "waiting" => InterpretedStatus::WaitingForInput,
            "needs attention" => InterpretedStatus::NeedsAttention,
            "completed" => InterpretedStatus::Completed,
            "failed" => InterpretedStatus::Failed,
            "stopped" => InterpretedStatus::Stopped,
            "session lost" => InterpretedStatus::SessionLost,
            "recovered" => InterpretedStatus::Recovered,
            _ => InterpretedStatus::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------
// OS notifications (SPECS §24) — alert the user when an agent finishes a task.
// ---------------------------------------------------------------------------

/// A pending OS notification produced when an agent finishes a running task.
///
/// Built by [`crate::app::state::AppState::take_finish_notifications`] and posted
/// by a [`crate::contracts::Notifier`] (the macOS implementation lives in
/// [`crate::notify`]). Decoupling production from delivery keeps the
/// transition-detection logic pure and unit-testable (SPECS §27).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// Short title (the Agent Tab name).
    pub title: String,
    /// Body line (e.g. `"Claude Code finished"`).
    pub body: String,
}

/// Manual status override set by the user (SPECS §24). `None` = cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualStatus {
    InProgress,
    Waiting,
    Blocked,
    Done,
}

impl ManualStatus {
    /// Short human-readable label.
    pub fn as_str(&self) -> &'static str {
        match self {
            ManualStatus::InProgress => "in progress",
            ManualStatus::Waiting => "waiting",
            ManualStatus::Blocked => "blocked",
            ManualStatus::Done => "done",
        }
    }

    /// Parse the label produced by [`ManualStatus::as_str`].
    pub fn from_str_lossy(s: &str) -> Option<Self> {
        match s {
            "in progress" => Some(ManualStatus::InProgress),
            "waiting" => Some(ManualStatus::Waiting),
            "blocked" => Some(ManualStatus::Blocked),
            "done" => Some(ManualStatus::Done),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Config model (SPECS §8) — committed, human-editable `config.toml`.
// ---------------------------------------------------------------------------

/// Output→status substring patterns for an agent (SPECS §8, §24).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusPatterns {
    #[serde(default)]
    pub waiting: Vec<String>,
    #[serde(default)]
    pub completed: Vec<String>,
    #[serde(default)]
    pub error: Vec<String>,
}

/// A configured agent (a `[agents.<key>]` table in `config.toml`).
///
/// `key` is the TOML table key; it is not stored in the table body, so it is
/// `#[serde(skip)]` and populated by the config loader after parsing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDef {
    #[serde(skip)]
    pub key: String,
    pub display_name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub status_patterns: StatusPatterns,
}

/// `[project]` config section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    pub default_base_branch: String,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        ProjectConfig {
            name: "project".to_string(),
            default_base_branch: "main".to_string(),
        }
    }
}

/// `[worktrees]` config section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreesConfig {
    pub root: String,
}

impl Default for WorktreesConfig {
    fn default() -> Self {
        WorktreesConfig {
            root: ".flightdeck/worktrees".to_string(),
        }
    }
}

/// `[git]` config section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitConfig {
    pub default_remote: String,
    pub primary_host: String,
    pub branch_prefix: String,
}

impl Default for GitConfig {
    fn default() -> Self {
        GitConfig {
            default_remote: "origin".to_string(),
            primary_host: "github".to_string(),
            branch_prefix: "flightdeck/".to_string(),
        }
    }
}

/// `[ui]` config section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiConfig {
    pub agent_tab_position: String,
    pub default_agent: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            agent_tab_position: "left".to_string(),
            default_agent: "opencode".to_string(),
        }
    }
}

/// Default for a boolean config field that should be `true` when omitted.
fn default_true() -> bool {
    true
}

/// `[notifications]` config section (SPECS §24): OS notifications fired when an
/// agent transitions out of an active state (working/starting) into a settled
/// one. Each category can be toggled independently; `enabled` is the master
/// switch and is **off by default** (opt-in) — enable it with `flightdeck
/// setup-notifications` or by editing the config. The per-category toggles
/// default to `true`, so once enabled all three categories fire unless one is
/// turned off; a partial `[notifications]` table fills the rest in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationsConfig {
    /// Master switch for all OS notifications. Off by default (opt-in).
    #[serde(default)]
    pub enabled: bool,
    /// Notify when an agent finishes its turn (idle / completed).
    #[serde(default = "default_true")]
    pub on_finish: bool,
    /// Notify when an agent is waiting for input / needs attention.
    #[serde(default = "default_true")]
    pub on_waiting: bool,
    /// Notify when an agent errors out (failed).
    #[serde(default = "default_true")]
    pub on_failed: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        NotificationsConfig {
            // Opt-in: off until the user enables it (config or setup command).
            enabled: false,
            on_finish: true,
            on_waiting: true,
            on_failed: true,
        }
    }
}

/// `[update]` config section (SPECS §30): the opt-in update notice. When
/// `check` is true, FlightDeck makes a once-a-day background check against
/// GitHub Releases on startup and shows a status-bar hint when a newer version
/// exists. It never auto-updates and never blocks startup; the check is **off by
/// default** (opt-in) because it makes a network request on launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// Master switch for the background update check. **Off by default**
    /// (opt-in) — `false` is the `Default`, so no network call happens on launch
    /// until the user turns it on.
    #[serde(default)]
    pub check: bool,
}

/// The full parsed `config.toml` (SPECS §8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub project: ProjectConfig,
    #[serde(default)]
    pub worktrees: WorktreesConfig,
    #[serde(default)]
    pub git: GitConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub notifications: NotificationsConfig,
    #[serde(default)]
    pub update: UpdateConfig,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentDef>,
}

// ---------------------------------------------------------------------------
// Runtime state model (SPECS §9) — ignored, not committed `state.json`.
// ---------------------------------------------------------------------------

/// Persisted state for a single Agent Tab (SPECS §9). Stores **relative** paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabState {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub agent: String,
    pub branch: String,
    pub worktree_path_relative: String,
    pub base_branch: String,
    pub base_commit_sha: String,
    pub created_at: String,
    #[serde(default)]
    pub attached_existing_branch: bool,
    #[serde(default)]
    pub recovered: bool,
    #[serde(default = "default_last_known_status")]
    pub last_known_status: String,
    #[serde(default)]
    pub manual_status: Option<String>,
}

fn default_last_known_status() -> String {
    "unknown".to_string()
}

/// Top-level persisted project state (SPECS §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectState {
    pub version: u32,
    pub project_root_relative: String,
    pub base_branch: String,
    #[serde(default)]
    pub tabs: Vec<TabState>,
}

/// The current state-file schema version.
pub const STATE_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Git value types used by the `GitExecutor` trait.
// ---------------------------------------------------------------------------

/// A git worktree as reported by `git worktree list` (SPECS §10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    /// Absolute path to the worktree.
    pub path: PathBuf,
    /// Checked-out branch, if any (detached HEAD → `None`).
    pub branch: Option<String>,
    /// HEAD commit SHA, if known.
    pub head: Option<String>,
}

/// Outcome of a guarded local merge-back (SPECS §15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOutcome {
    /// True if the merge completed successfully.
    pub merged: bool,
    /// True if the merge stopped on conflicts (manual intervention needed).
    pub conflicted: bool,
    /// Human-readable detail.
    pub message: String,
}

/// Outcome of a guarded worktree rebase onto the base branch (SPECS §5
/// carve-out). On conflict the rebase is aborted so the worktree is left
/// exactly as it was — FlightDeck never resolves conflicts or leaves a
/// half-finished rebase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseOutcome {
    /// True if the rebase completed successfully (HEAD now sits on the base).
    pub rebased: bool,
    /// True if the rebase hit conflicts and was aborted (no changes applied).
    pub conflicted: bool,
    /// Human-readable detail.
    pub message: String,
}

/// Terminal dimensions for a PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for PtySize {
    fn default() -> Self {
        PtySize { rows: 24, cols: 80 }
    }
}
