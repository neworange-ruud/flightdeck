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

/// Status interpreted from process state + explicit lifecycle events (SPECS §24).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpretedStatus {
    Starting,
    Running,
    /// The backend reports an active agent turn.
    Working,
    /// The backend reports that the agent is waiting for a prompt.
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

/// A pending OS notification produced when an agent needs the user's attention.
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
    /// The optional alert sound that accompanies this notification. Completion
    /// and input-required events deliberately use different sounds so they can
    /// be distinguished without looking at the screen.
    pub sound: NotificationSound,
}

/// The sound to play alongside an OS notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationSound {
    /// Do not play an alert sound.
    None,
    /// The two-note chime for a completed agent turn.
    Completion,
    /// The three-pulse alert for an agent waiting on user input.
    InputRequired,
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

/// Deprecated output→status substring patterns retained so existing project
/// configs still deserialize and round-trip. Runtime status comes exclusively
/// from explicit backend lifecycle integrations (SPECS §8, §24).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusPatterns {
    #[serde(default)]
    pub waiting: Vec<String>,
    #[serde(default)]
    pub completed: Vec<String>,
    #[serde(default)]
    pub error: Vec<String>,
}

impl StatusPatterns {
    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty() && self.completed.is_empty() && self.error.is_empty()
    }
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
    #[serde(default, skip_serializing_if = "StatusPatterns::is_empty")]
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
    /// Use F2 instead of the platform-default modified-Esc shortcut to leave
    /// terminal focus. Off by default.
    #[serde(default)]
    pub use_f2_to_leave_terminal_focus: bool,
    /// Auto-continuation: capture an agent's on-exit resume command and replay
    /// it on restart/recovery so a tab continues its previous session. On by
    /// default. Turn it off (`auto_continue = false`) to disable both halves —
    /// nothing is captured from output and nothing is replayed on start; tabs
    /// simply start fresh. Agent termination on shutdown is unaffected.
    #[serde(default = "default_true")]
    pub auto_continue: bool,
    /// Color of the TERMINAL-mode cue (chip + live-pane border). One of:
    /// green, cyan, blue, magenta, yellow, red, white.
    #[serde(default = "default_terminal_mode_color")]
    pub terminal_mode_color: String,
    /// Color of the APP-mode cue (chip + live-pane border). Same value set.
    #[serde(default = "default_app_mode_color")]
    pub app_mode_color: String,
    /// Live-pane border brightness: off, dim, normal, bright.
    #[serde(default = "default_mode_border")]
    pub mode_border: String,
    /// Dim the terminal viewport while in APP mode (it is not receiving keys).
    #[serde(default = "default_true")]
    pub dim_terminal_in_app_mode: bool,
}

fn default_terminal_mode_color() -> String {
    "green".to_string()
}
fn default_app_mode_color() -> String {
    "cyan".to_string()
}
fn default_mode_border() -> String {
    "off".to_string()
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            agent_tab_position: "left".to_string(),
            default_agent: "opencode".to_string(),
            use_f2_to_leave_terminal_focus: false,
            auto_continue: true,
            terminal_mode_color: default_terminal_mode_color(),
            app_mode_color: default_app_mode_color(),
            mode_border: default_mode_border(),
            dim_terminal_in_app_mode: true,
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
/// switch and is **on by default** — turn it off with `enabled = false` (in the
/// global or a project `config.toml`) or from the configuration manager. The
/// per-category toggles also default to `true`, so out of the box all three
/// categories fire unless one is turned off; a partial `[notifications]` table
/// fills the rest in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationsConfig {
    /// Master switch for all OS notifications. On by default.
    #[serde(default = "default_true")]
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
    /// Play distinctive alert sounds when an agent finishes its turn or needs
    /// input. On by default; independent of the visual/OS notification
    /// categories.
    #[serde(default = "default_true")]
    pub sound: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        NotificationsConfig {
            // On by default: notify when agents finish/wait/fail out of the box.
            enabled: true,
            on_finish: true,
            on_waiting: true,
            on_failed: true,
            sound: true,
        }
    }
}

/// `[update]` config section (SPECS §30): the update notice. When
/// `check` is true, FlightDeck makes a once-a-day background check against
/// GitHub Releases on startup and shows a status-bar hint when a newer version
/// exists. It never auto-updates and never blocks startup; the check is **on by
/// default** and can be disabled with `check = false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// Master switch for the background update check. On by default.
    #[serde(default = "default_true")]
    pub check: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        UpdateConfig { check: true }
    }
}

/// Default relay endpoint for FlightDeck Remote: the stable custom domain
/// (`relay.flightdeckai.app`, remote-control-edn) fronting the hosted relay on
/// Azure Container Apps, so the URL survives any rename/recreate of the
/// underlying Azure resources. Overridable in `config.toml` (or per-device in
/// `~/.flightdeck/remote.json`). An empty `relay_url` is treated as "no relay
/// configured" and disables the client even when `enabled = true`.
fn default_relay_url() -> String {
    "wss://relay.flightdeckai.app/ws".to_string()
}

/// `[remote]` config section: FlightDeck Remote, the phone <-> desktop link over
/// a hosted relay. **Off by default** — the desktop opens no outbound connection
/// and behaves bit-for-bit as before until `enabled = true`. When enabled, a
/// background thread (see `src/remote/`) maintains one WebSocket to `relay_url`,
/// authenticates with the per-device key, and reports link state to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfig {
    /// Master switch for the relay client. Off by default.
    #[serde(default)]
    pub enabled: bool,
    /// Relay WebSocket URL (`wss://…`, or `ws://…` for local dev). Empty means
    /// "not configured" and keeps the client dormant even if `enabled`.
    #[serde(default = "default_relay_url")]
    pub relay_url: String,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        RemoteConfig {
            enabled: false,
            relay_url: default_relay_url(),
        }
    }
}

// ---------------------------------------------------------------------------
// Container execution model (SPECS §31) — the optional `[containers]` section.
// ---------------------------------------------------------------------------

fn default_runtime() -> String {
    "podman".to_string()
}
fn default_cpu() -> String {
    "4".to_string()
}
fn default_memory() -> String {
    "8g".to_string()
}
fn default_pids() -> u32 {
    512
}

/// Container resource limits (SPECS §31). Stored as strings/ints (never `f32`)
/// so the whole [`Config`] can keep `Eq`. `cpu` maps to `--cpus` (e.g. `"4"` or
/// `"1.5"`), `memory` to `--memory` (e.g. `"8g"`), `pids` to `--pids-limit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    #[serde(default = "default_cpu")]
    pub cpu: String,
    #[serde(default = "default_memory")]
    pub memory: String,
    #[serde(default = "default_pids")]
    pub pids: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            cpu: default_cpu(),
            memory: default_memory(),
            pids: default_pids(),
        }
    }
}

/// A host credential bind-mounted read-only (or writable) into the agent
/// container (SPECS §31). `host_path` may start with `~`. Mounting `$HOME`
/// itself is rejected by the runtime guardrails regardless of config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthMount {
    pub host_path: String,
    pub container_path: String,
    #[serde(default)]
    pub writable: bool,
}

/// How agent credentials reach the container (SPECS §31): bind-mounted files
/// and/or a host-env allowlist injected as `--env KEY=value`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub mounts: Vec<AuthMount>,
    #[serde(default)]
    pub env_allow: Vec<String>,
}

/// `[containers]` config section (SPECS §31). When the table is absent or
/// `enabled = false`, FlightDeck's behaviour is bit-for-bit the local model;
/// every field is `#[serde(default)]` so existing `config.toml` files keep
/// working untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainersConfig {
    /// Master switch. **Off by default** — all agents run locally until enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Container runtime. Only `"podman"` is supported in v1.
    #[serde(default = "default_runtime")]
    pub runtime: String,
    /// Fully-resolved image to run. When `None`, FlightDeck builds one from the
    /// base + customization (SPECS §31 image strategy).
    #[serde(default)]
    pub image: Option<String>,
    /// Override the FlightDeck base image the generated Containerfile builds on.
    #[serde(default)]
    pub base_image: Option<String>,
    /// Declarative customization: OS packages to install on top of the base.
    #[serde(default)]
    pub packages: Vec<String>,
    /// Declarative customization: a repo-relative script run during the build.
    #[serde(default)]
    pub setup_script: Option<String>,
    /// Advanced escape hatch: a repo-relative Containerfile to build verbatim
    /// (expected to `FROM` a FlightDeck base). Mutually exclusive with
    /// `packages`/`setup_script`.
    #[serde(default)]
    pub containerfile: Option<String>,
    /// Ports published to `127.0.0.1` at container start.
    #[serde(default)]
    pub forward_ports: Vec<u16>,
    /// Resource limits.
    #[serde(default)]
    pub limits: Limits,
    /// Credential delivery.
    #[serde(default)]
    pub auth: AuthConfig,
}

impl Default for ContainersConfig {
    fn default() -> Self {
        ContainersConfig {
            enabled: false,
            runtime: default_runtime(),
            image: None,
            base_image: None,
            packages: Vec::new(),
            setup_script: None,
            containerfile: None,
            forward_ports: Vec::new(),
            limits: Limits::default(),
            auth: AuthConfig::default(),
        }
    }
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
    /// FlightDeck Remote (phone link). Absent table → disabled → no relay.
    #[serde(default)]
    pub remote: RemoteConfig,
    /// Container execution (SPECS §31). Absent table → disabled → local model.
    /// Accepts the legacy `[execution]` section name as a deprecated alias.
    #[serde(default, alias = "execution")]
    pub containers: ContainersConfig,
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
    /// Whether this tab's agent was launched inside a container (SPECS §31).
    /// Recorded per-tab so that toggling `[containers] enabled` off later does
    /// not mislead reattach/teardown of tabs that *were* containerized.
    #[serde(default)]
    pub containerized: bool,
    /// The image the container was launched from, for provenance (SPECS §31).
    #[serde(default)]
    pub container_image: Option<String>,
    /// Whether this tab runs directly on the base branch in the project root
    /// (no dedicated worktree). Such a tab shares the repo root with FlightDeck
    /// itself, so worktree-destructive ops (abandon/merge/rebase) are refused
    /// and no `git worktree add`/`remove` is ever run for it.
    #[serde(default)]
    pub runs_on_base: bool,
    /// Args to relaunch the agent so it resumes its previous session, captured
    /// from the agent's on-exit resume hint (e.g. `["--resume", "<uuid>"]` for
    /// Claude, `["resume", "<uuid>"]` for Codex). Empty = start fresh. Replayed
    /// in place of the configured base args on resume/restart.
    #[serde(default)]
    pub resume_args: Vec<String>,
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

// ---------------------------------------------------------------------------
// Container runtime value types (SPECS §31), used by the `ContainerRuntime` trait.
// ---------------------------------------------------------------------------

/// Liveness of a named container as reported by the runtime (SPECS §31).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerState {
    /// The container exists and its main process is running.
    Running,
    /// The container existed and has exited (not yet removed).
    Exited,
    /// No container with that name exists (never created, or already removed).
    Absent,
}
