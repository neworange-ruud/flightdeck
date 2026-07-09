//! Headless application state and the command dispatcher (T7, SPECS §3, §24, §25).
//!
//! Holds the Agent Tabs (the §3 invariant: 1 tab = 1 worktree = 1 branch =
//! 1 primary agent), the selected tab and child terminal, the current input
//! mode, and persistent warnings (e.g. dirty-base → merge disabled, SPECS §13).
//!
//! This layer performs **no** terminal I/O and never executes git/fs/pty
//! directly: every side effect goes through the [`Services`] trait objects so it
//! is fully unit-testable with the fakes in [`crate::testing`] (SPECS §27).

use std::path::{Path, PathBuf};

use crate::agents::adapter::{build_launch, validate_agent};
use crate::agents::registry::AgentRegistry;
use crate::agents::status::{classify_output, combine_status, running_status, DisplayStatus};
use crate::app::commands::{CloseAction, CloseTabOptions, Command, Effect, PushConfirm, Selector};
use crate::app::modes::InputMode;
use crate::contracts::{
    AgentDef, Clock, Config, ContainerRuntime, ContainerState, ContainersConfig, FileSystem,
    FlightDeckError, GitExecutor, InterpretedStatus, ManualStatus, Notification,
    NotificationsConfig, ProcessState, ProjectState, PtyBackend, PtySize, Result, TabId, TabState,
    STATE_VERSION,
};
use crate::fs::paths::{to_absolute, to_relative, worktree_path};
use crate::git::branch::{branch_name, decide_branch, slugify, BranchDecision};
use crate::git::remote::{github_pr_url, plan_push, push_branch, PushPlan};
use crate::git::status::{
    base_drift, check_merge_preconditions, check_rebase_preconditions, collect_status, merge_back,
    rebase_onto_base, MergeDecision, MergeRequest, RebaseDecision, RebaseRequest,
};
use crate::git::worktree::{create_worktree, plan_worktree, remove_worktree_if_safe, WorktreePlan};
use crate::persistence::project_state::save_state;
use crate::runtime::container::{
    build_attach_args, build_exec_args, build_run_args, standard_labels,
};
use crate::runtime::guards::enforce_guardrails;
use crate::runtime::image;
use crate::runtime::name::{container_name, repo_hash};
use crate::runtime::spec::{ContainerSpec, ResolvedAuthMount};
use crate::terminal::session::Session;
use crate::terminal::shell::{container_shell, shell_launch};

/// The services the app core dispatches into (SPECS §27). Passing these as a
/// bundle keeps the core headless: the wiring layer (T9) constructs real
/// implementations, tests construct fakes.
pub struct Services<'a> {
    /// Git access (never history-rewriting, SPECS §5).
    pub git: &'a dyn GitExecutor,
    /// Filesystem access.
    pub fs: &'a dyn FileSystem,
    /// PTY backend for spawning terminals.
    pub pty: &'a dyn PtyBackend,
    /// Clock for deterministic timestamps.
    pub clock: &'a dyn Clock,
    /// Container runtime control plane (SPECS §31).
    pub container: &'a dyn ContainerRuntime,
}

/// Lifecycle phase of an Agent Tab (SPECS §16/§17).
///
/// A tab is `Creating` while its worktree is materialized on a background
/// worker (so the UI never blocks on `git worktree add`); it flips to `Ready`
/// once the primary agent is spawned. Creation failures remove the placeholder
/// tab entirely rather than leaving a dead state, so there is no `Failed`
/// variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabPhase {
    /// Worktree is being materialized on a background worker; no session yet.
    Creating,
    /// Worktree exists and the primary agent has been spawned.
    Ready,
}

/// A unit of slow worktree-materialization work handed to a background worker
/// after [`AppState::begin_new_agent_tab`] has reserved a placeholder tab.
///
/// Carries only owned, `Send` data so it can cross the thread boundary; the
/// worker runs [`materialize_worktree`] and the main thread then calls
/// [`AppState::finalize_new_tab`] (or [`AppState::fail_new_tab`]).
#[derive(Debug, Clone)]
pub struct WorktreeJob {
    /// Id of the placeholder ([`TabPhase::Creating`]) tab to finalize.
    pub tab_id: String,
    /// Branch the worktree is for.
    pub branch: String,
    /// Base branch to create from (when `create_branch`).
    pub base_branch: String,
    /// Absolute path where the worktree lives (to create, or to reuse).
    pub worktree_abs: PathBuf,
    /// Whether the worker must run `git worktree add` (false = reuse existing).
    pub needs_create: bool,
    /// Whether to create the branch before adding the worktree.
    pub create_branch: bool,
}

/// Run the slow part of new-tab creation off the UI thread: materialize the
/// worktree described by `job`. A no-op when the worktree is being reused.
///
/// Free function (no `&self`) so a background worker can call it with a cloned
/// [`GitExecutor`] without borrowing [`AppState`].
pub fn materialize_worktree(git: &dyn GitExecutor, job: &WorktreeJob) -> Result<()> {
    if job.needs_create {
        create_worktree(
            git,
            &job.branch,
            &job.base_branch,
            &job.worktree_abs,
            job.create_branch,
        )?;
    }
    Ok(())
}

/// How to launch (or reattach) a tab's primary terminal (SPECS §31).
///
/// `command`/`args` are handed to the [`PtyBackend`] (for a container that is
/// `podman attach <name>`). When `start_args` is `Some`, the caller must first
/// start the detached container with those `podman run -d …` args — that is the
/// fresh-launch case; reattach leaves it `None`.
struct PrimarySpawn {
    /// `podman run -d …` args to start the container before attaching (fresh
    /// launch only); `None` when reattaching or running locally.
    start_args: Option<Vec<String>>,
    command: String,
    args: Vec<String>,
    containerized: bool,
    image: Option<String>,
}

/// SELinux relabel suffix for bind mounts: `Some("z")` on Linux, `None` on
/// macOS (the Podman-machine virtiofs share takes no relabel). SPECS §31/§11.
fn platform_mount_flags() -> Option<String> {
    if cfg!(target_os = "linux") {
        Some("z".to_string())
    } else {
        None
    }
}

/// Home directory of the non-root `agent` user in the default base image
/// (SPECS §31). Default credential mounts target subpaths of this.
const AGENT_HOME: &str = "/home/agent";

/// Host→container credential mounts applied by default (no `[containers.auth]`
/// configured, default base image) so the host agent's login carries into the
/// container (SPECS §31). Host paths may use `~`; absent ones are skipped.
fn default_auth_mounts(agent: &str) -> Vec<(&'static str, &'static str)> {
    match agent {
        "claude" => vec![
            ("~/.claude", "/home/agent/.claude"),
            ("~/.claude.json", "/home/agent/.claude.json"),
        ],
        "codex" => vec![("~/.codex", "/home/agent/.codex")],
        "opencode" => vec![
            (
                "~/.local/share/opencode",
                "/home/agent/.local/share/opencode",
            ),
            ("~/.config/opencode", "/home/agent/.config/opencode"),
        ],
        _ => vec![],
    }
}

/// Host env vars injected by default (no `[containers.auth]` configured) when
/// present, so API-key auth works without configuration (SPECS §31).
fn default_env_allow(agent: &str) -> Vec<&'static str> {
    match agent {
        "claude" => vec!["ANTHROPIC_API_KEY"],
        "codex" => vec!["OPENAI_API_KEY"],
        "opencode" => vec!["ANTHROPIC_API_KEY", "OPENAI_API_KEY"],
        _ => vec![],
    }
}

/// Expand a leading `~/` in a config path against `$HOME`.
fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(p)
}

/// Validate an agent is launchable before any git mutation (SPECS §16/§31).
/// In container mode the agent binary lives *inside* the image, so instead of
/// the agent command we require the container runtime to be ready
/// ([`ContainerRuntime::available`]).
fn validate_launchable(
    agent: &AgentDef,
    exec: &ContainersConfig,
    container: &dyn ContainerRuntime,
    base: &Path,
) -> Result<()> {
    if exec.enabled {
        container.available()
    } else {
        validate_agent(agent, base)
    }
}

/// Whether `cmd` acts on the selected tab's live worktree/session and therefore
/// must be refused while that tab is still in [`TabPhase::Creating`]. Creating a
/// new tab, switching/closing tabs, and global commands stay allowed.
fn requires_ready_tab(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::PushBranch { .. }
            | Command::FinishLocalMerge { .. }
            | Command::RebaseWorktree { .. }
            | Command::CopyEnvFile
            | Command::AbandonWorktree { .. }
            | Command::NewChildTerminal
            | Command::NewAgentTerminal { .. }
            | Command::CloseAgentTerminal
            | Command::OpenShell
            | Command::CloseChildTerminal
            | Command::SwitchChildTerminal(_)
            | Command::RestartAgent
            | Command::RenameAgentTab { .. }
            | Command::ShowGitStatus
    )
}

/// A live Agent Tab: persisted metadata + a live terminal session + the cached
/// interpreted status from output pattern matching (SPECS §3, §24).
///
/// The `meta` field is exactly what is serialized to `state.json`; the rest is
/// runtime-only and not persisted.
pub struct RuntimeTab {
    /// The persisted tab metadata (serialized verbatim to `state.json`).
    pub meta: TabState,
    /// Lifecycle phase: `Creating` until the background worker materializes the
    /// worktree and the agent is spawned, then `Ready`.
    pub phase: TabPhase,
    /// The live terminal session (one primary + N children). Not persisted.
    pub session: Session,
    /// Cached interpreted status from the latest output classification or hook
    /// signal, if any. Combined with process state + activity + manual override
    /// for display (SPECS §24).
    pub interpreted: Option<InterpretedStatus>,
    /// Clock-millis when `interpreted` was last set (for sticky-signal logic).
    pub interpreted_at_ms: Option<u64>,
    /// Clock-millis of the most recent PTY output from the primary agent. Drives
    /// the universal idle/working heuristic (SPECS §24).
    pub last_activity_ms: Option<u64>,
    /// Absolute path to this tab's agent status file, written by the agent's
    /// status hook/plugin (Layer 2, opt-in) and polled by FlightDeck. `None`
    /// until the agent is spawned.
    pub status_file: Option<PathBuf>,
    /// Last raw status-file content applied, used to ignore unchanged re-reads so
    /// each hook event registers as a single fresh signal.
    pub status_file_seen: Option<String>,
    /// Whether this tab was last observed in an *active* state (working /
    /// starting). When it next settles (idle / waiting / completed / failed) a
    /// single OS notification fires and this is cleared, so a quiet agent never
    /// re-notifies until it resumes working (SPECS §24).
    pub notify_armed: bool,
}

impl RuntimeTab {
    /// Build a runtime tab from persisted metadata, with an empty session and no
    /// cached interpreted status. Does **not** spawn anything (SPECS §10).
    fn from_meta(meta: TabState) -> Self {
        RuntimeTab {
            meta,
            // Recovered tabs already have a worktree on disk (SPECS §10).
            phase: TabPhase::Ready,
            session: Session::new(),
            interpreted: None,
            interpreted_at_ms: None,
            last_activity_ms: None,
            status_file: None,
            status_file_seen: None,
            notify_armed: false,
        }
    }

    /// Stable id of this tab.
    pub fn id(&self) -> TabId {
        TabId(self.meta.id.clone())
    }

    /// The combined, display-ready status (SPECS §24): live process state +
    /// activity-derived idle/working + cached signal + manual override. Manual
    /// takes visual priority but never hides process state.
    ///
    /// `now_ms` is the current clock-millis ([`Clock::now_millis`]); only running
    /// agents consult it (to decide idle vs working).
    pub fn display_status(&self, now_ms: u64) -> DisplayStatus {
        let manual = self
            .meta
            .manual_status
            .as_deref()
            .and_then(ManualStatus::from_str_lossy);
        let process = self.session.primary_state();
        let interpreted = if process == ProcessState::Running {
            Some(running_status(
                self.interpreted,
                self.interpreted_at_ms,
                self.last_activity_ms,
                now_ms,
            ))
        } else {
            // Not running: let the process state drive (exited → completed/failed,
            // lost → session lost, …) rather than a stale activity reading.
            None
        };
        combine_status(process, interpreted, manual)
    }
}

/// Path to a tab's agent status file inside its worktree (Layer 2, SPECS §24).
///
/// The agent's status hook/plugin writes one of the keywords understood by
/// [`status_keyword_to_interpreted`] here; FlightDeck polls it. The path is
/// derived purely from the worktree so the hook can compute the same path from
/// its own working directory without any injected configuration. It is covered
/// by the `.flightdeck/agent-status` `.gitignore` entry added at init.
pub fn agent_status_file(worktree_abs: &Path) -> PathBuf {
    worktree_abs.join(".flightdeck").join("agent-status")
}

/// Map a status-file keyword (written by an agent hook/plugin) to an
/// [`InterpretedStatus`] (SPECS §24). Unknown keywords yield `None`.
pub fn status_keyword_to_interpreted(keyword: &str) -> Option<InterpretedStatus> {
    match keyword.trim().to_ascii_lowercase().as_str() {
        "working" | "busy" | "in_progress" | "in-progress" | "thinking" => {
            Some(InterpretedStatus::Working)
        }
        "idle" => Some(InterpretedStatus::Idle),
        "waiting" | "waiting_for_input" | "input" => Some(InterpretedStatus::WaitingForInput),
        "attention" | "needs_attention" | "needs-attention" | "notification" => {
            Some(InterpretedStatus::NeedsAttention)
        }
        "done" | "completed" | "complete" | "finished" => Some(InterpretedStatus::Completed),
        "error" | "failed" | "failure" => Some(InterpretedStatus::Failed),
        _ => None,
    }
}

/// How long after the event loop starts finish-notifications are suppressed
/// (SPECS §24). Long enough for resumed/just-launched agents to settle to idle
/// without firing a "finished" alert.
pub const NOTIFY_STARTUP_GRACE_MS: u64 = 4000;

/// Which OS-notification category a settled status belongs to (SPECS §24), used
/// to gate it against the per-category config toggles and to phrase the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotifyKind {
    /// Agent finished its turn (idle / completed).
    Finish,
    /// Agent is waiting for input / needs attention.
    Waiting,
    /// Agent errored out.
    Failed,
}

impl NotifyKind {
    /// Whether this category is enabled in the config.
    fn enabled(self, cfg: &NotificationsConfig) -> bool {
        match self {
            NotifyKind::Finish => cfg.on_finish,
            NotifyKind::Waiting => cfg.on_waiting,
            NotifyKind::Failed => cfg.on_failed,
        }
    }

    /// The verb used in the notification body, e.g. `"finished"`.
    fn verb(self) -> &'static str {
        match self {
            NotifyKind::Finish => "finished",
            NotifyKind::Waiting => "is waiting for input",
            NotifyKind::Failed => "failed",
        }
    }
}

/// Classification of an [`InterpretedStatus`] for notification edge-detection
/// (SPECS §24): an agent is *active*, has just *finished* (a notifiable
/// category), or is in a *neutral* state that neither arms nor notifies.
enum NotifyPhase {
    Active,
    Finished(NotifyKind),
    Neutral,
}

/// Map an interpreted status to its notification phase (SPECS §24).
fn notify_phase(status: InterpretedStatus) -> NotifyPhase {
    match status {
        InterpretedStatus::Starting | InterpretedStatus::Running | InterpretedStatus::Working => {
            NotifyPhase::Active
        }
        InterpretedStatus::Idle | InterpretedStatus::Completed => {
            NotifyPhase::Finished(NotifyKind::Finish)
        }
        InterpretedStatus::WaitingForInput | InterpretedStatus::NeedsAttention => {
            NotifyPhase::Finished(NotifyKind::Waiting)
        }
        InterpretedStatus::Failed => NotifyPhase::Finished(NotifyKind::Failed),
        InterpretedStatus::Stopped
        | InterpretedStatus::SessionLost
        | InterpretedStatus::Recovered
        | InterpretedStatus::Unknown => NotifyPhase::Neutral,
    }
}

/// The headless application state (SPECS §3).
pub struct AppState {
    /// The parsed, committed configuration (SPECS §8).
    pub config: Config,
    /// The agent registry built from config (SPECS §8).
    pub registry: AgentRegistry,
    /// The runtime Agent Tabs, in display order.
    pub tabs: Vec<RuntimeTab>,
    /// Index of the selected tab, if any.
    pub selected_tab: Option<usize>,
    /// The current input mode (SPECS §23).
    pub mode: InputMode,
    /// Persistent warnings to keep on screen (e.g. dirty base → merge disabled,
    /// SPECS §13). Deduplicated.
    pub warnings: Vec<String>,
    /// The base branch (SPECS §12).
    pub base_branch: String,
    /// Absolute repository root.
    pub repo_root: PathBuf,
    /// Absolute path to `state.json`.
    pub state_path: PathBuf,
    /// PTY size used when spawning terminals; updated on resize.
    pub pty_size: PtySize,
    /// Clock-millis before which finish-notifications are suppressed. Set at
    /// event-loop start to a short window after launch so resumed/just-spawned
    /// agents settling to idle don't fire a burst of alerts (SPECS §24).
    pub notify_grace_until_ms: u64,
    /// When `true`, the selected tab's terminals (primary agent + child shells)
    /// are laid out side by side in equal-width columns instead of as a single
    /// active terminal behind a horizontal tab bar. Runtime-only (not persisted).
    pub split_view: bool,
    /// Latest published version when a newer release than this binary exists,
    /// set by the opt-in once-a-day update check (SPECS §30). `None` until the
    /// check completes (or when up to date / the check is disabled). Drives the
    /// status-bar update hint. Runtime-only.
    pub update_available: Option<String>,
}

impl AppState {
    /// Build the app state from config, a recovered [`ProjectState`], the repo
    /// root, and the `state.json` path. Recovered tabs are reconstructed as
    /// runtime tabs **without** spawning agents (SPECS §10).
    pub fn new(
        config: Config,
        state: ProjectState,
        repo_root: impl Into<PathBuf>,
        state_path: impl Into<PathBuf>,
    ) -> Self {
        let registry = AgentRegistry::from_config(&config);
        let tabs: Vec<RuntimeTab> = state.tabs.into_iter().map(RuntimeTab::from_meta).collect();
        let selected_tab = if tabs.is_empty() { None } else { Some(0) };
        AppState {
            config,
            registry,
            tabs,
            selected_tab,
            mode: InputMode::default(),
            warnings: Vec::new(),
            base_branch: state.base_branch,
            repo_root: repo_root.into(),
            state_path: state_path.into(),
            pty_size: PtySize::default(),
            notify_grace_until_ms: 0,
            split_view: false,
            update_available: None,
        }
    }

    // -----------------------------------------------------------------------
    // Mode handling (SPECS §23)
    // -----------------------------------------------------------------------

    /// The current input mode.
    pub fn mode(&self) -> InputMode {
        self.mode
    }

    /// Enter terminal-focus mode (keystrokes go to the active terminal).
    pub fn focus_terminal(&mut self) {
        self.mode = InputMode::Terminal;
    }

    /// Enter app-command mode (keystrokes control FlightDeck).
    pub fn focus_app(&mut self) {
        self.mode = InputMode::App;
    }

    /// Toggle between the two modes.
    pub fn toggle_mode(&mut self) {
        self.mode = match self.mode {
            InputMode::Terminal => InputMode::App,
            InputMode::App => InputMode::Terminal,
        };
    }

    /// Toggle split view (side-by-side terminals vs. horizontal tabs).
    pub fn toggle_split_view(&mut self) {
        self.split_view = !self.split_view;
    }

    // -----------------------------------------------------------------------
    // Tab/selection helpers
    // -----------------------------------------------------------------------

    /// The selected runtime tab, if any.
    pub fn selected(&self) -> Option<&RuntimeTab> {
        self.selected_tab.and_then(|i| self.tabs.get(i))
    }

    /// Whether the selected tab's worktree is still being created.
    fn selected_is_creating(&self) -> bool {
        self.selected()
            .map(|t| t.phase == TabPhase::Creating)
            .unwrap_or(false)
    }

    /// Mutable access to the selected runtime tab, if any.
    pub fn selected_mut(&mut self) -> Option<&mut RuntimeTab> {
        match self.selected_tab {
            Some(i) => self.tabs.get_mut(i),
            None => None,
        }
    }

    /// Index of the tab with the given id, if present.
    pub fn tab_index(&self, id: &TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.meta.id == id.0)
    }

    /// Resolve a [`Selector`] against `len` and the current index.
    fn resolve_selector(sel: Selector, current: Option<usize>, len: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        let cur = current.unwrap_or(0);
        let idx = match sel {
            Selector::Index(i) => i,
            Selector::Next => (cur + 1) % len,
            Selector::Prev => (cur + len - 1) % len,
        };
        if idx < len {
            Some(idx)
        } else {
            None
        }
    }

    // -----------------------------------------------------------------------
    // Status ingestion (SPECS §24)
    // -----------------------------------------------------------------------

    /// Ingest PTY output for a tab: record output activity (for the idle/working
    /// heuristic) and update the cached interpreted status by running the agent's
    /// patterns through [`classify_output`] (SPECS §24). `now_ms` is the current
    /// [`Clock::now_millis`]. Output that matches no pattern leaves the cached
    /// signal unchanged but still counts as activity.
    pub fn ingest_output(&mut self, tab: &TabId, bytes: &[u8], now_ms: u64) {
        let Some(idx) = self.tab_index(tab) else {
            return;
        };
        // Any output is activity → the agent is doing something right now.
        self.tabs[idx].last_activity_ms = Some(now_ms);
        let text = String::from_utf8_lossy(bytes);
        let agent_key = self.tabs[idx].meta.agent.clone();
        let patterns = self
            .registry
            .get(&agent_key)
            .map(|a| a.status_patterns.clone());
        if let Some(patterns) = patterns {
            if let Some(status) = classify_output(&patterns, &text) {
                self.tabs[idx].interpreted = Some(status);
                self.tabs[idx].interpreted_at_ms = Some(now_ms);
            }
        }
    }

    /// Poll each tab's agent status file (Layer 2, opt-in precise signals).
    ///
    /// A configured agent hook/plugin writes a keyword (see
    /// [`status_keyword_to_interpreted`]) to the tab's [`agent_status_file`].
    /// Each *new* content registers as a fresh signal stamped `now_ms`, so it is
    /// shown immediately yet can still be superseded by later output activity
    /// (important for agents like Codex that only signal turn completion).
    /// Missing/unreadable files are ignored, so this is safe before any hook is
    /// installed. Call on each tick (SPECS §24).
    pub fn poll_status_files(&mut self, services: &Services, now_ms: u64) {
        for tab in self.tabs.iter_mut() {
            let Some(path) = tab.status_file.as_ref() else {
                continue;
            };
            let Ok(content) = services.fs.read_to_string(path) else {
                continue;
            };
            if tab.status_file_seen.as_deref() == Some(content.as_str()) {
                continue; // unchanged since last poll
            }
            if let Some(status) = status_keyword_to_interpreted(&content) {
                tab.interpreted = Some(status);
                tab.interpreted_at_ms = Some(now_ms);
            }
            tab.status_file_seen = Some(content);
        }
    }

    /// Open the startup grace window: suppress finish-notifications until
    /// `now_ms + NOTIFY_STARTUP_GRACE_MS` (SPECS §24). Called once when the event
    /// loop starts so agents resumed/spawned at launch can settle to idle without
    /// each firing a "finished" alert.
    pub fn begin_notification_grace(&mut self, now_ms: u64) {
        self.notify_grace_until_ms = now_ms + NOTIFY_STARTUP_GRACE_MS;
    }

    /// Detect agents that just finished a running task and return the OS
    /// notifications to post, updating each tab's per-tab arming so a quiet agent
    /// never re-notifies (SPECS §24).
    ///
    /// A notification fires on the **edge** from an active state (working /
    /// starting / running) to a settled one (idle/completed → "finished",
    /// waiting/needs-attention → "waiting", failed → "failed"). Pure of I/O: the
    /// caller (event loop) hands the results to a [`crate::contracts::Notifier`].
    /// Returns nothing while notifications are disabled or during the startup
    /// grace window — but arming is still tracked, so only *new* finishes after
    /// the window alert.
    pub fn take_finish_notifications(&mut self, now_ms: u64) -> Vec<Notification> {
        let cfg = self.config.notifications;
        let suppressed = !cfg.enabled || now_ms < self.notify_grace_until_ms;
        let registry = &self.registry;
        let mut out = Vec::new();
        for tab in self.tabs.iter_mut() {
            let interpreted = tab.display_status(now_ms).interpreted;
            match notify_phase(interpreted) {
                NotifyPhase::Active => tab.notify_armed = true,
                NotifyPhase::Neutral => tab.notify_armed = false,
                NotifyPhase::Finished(kind) => {
                    let was_armed = tab.notify_armed;
                    tab.notify_armed = false;
                    if was_armed && !suppressed && kind.enabled(&cfg) {
                        let agent = registry
                            .get(&tab.meta.agent)
                            .map(|a| a.display_name.clone())
                            .unwrap_or_else(|| tab.meta.agent.clone());
                        out.push(Notification {
                            title: tab.meta.name.clone(),
                            body: format!("{agent} {}", kind.verb()),
                        });
                    }
                }
            }
        }
        out
    }

    /// Update the persisted PTY size used for future spawns (SPECS §23 resize).
    pub fn set_pty_size(&mut self, size: PtySize) {
        self.pty_size = size;
    }

    // -----------------------------------------------------------------------
    // Persistence (SPECS §9)
    // -----------------------------------------------------------------------

    /// Build a [`ProjectState`] from the runtime tabs for persistence (SPECS §9).
    /// Snapshots the last-known status from each tab's combined display status.
    /// `now_ms` is the current [`Clock::now_millis`], used to resolve idle/working.
    pub fn to_project_state(&self, now_ms: u64) -> ProjectState {
        let project_root_relative = match to_relative(&self.repo_root, &self.repo_root) {
            Ok(p) if !p.as_os_str().is_empty() => p.to_string_lossy().to_string(),
            _ => ".".to_string(),
        };
        let tabs = self
            .tabs
            .iter()
            .map(|t| {
                let mut meta = t.meta.clone();
                meta.last_known_status = t.display_status(now_ms).interpreted.as_str().to_string();
                meta
            })
            .collect();
        ProjectState {
            version: STATE_VERSION,
            project_root_relative,
            base_branch: self.base_branch.clone(),
            tabs,
        }
    }

    /// Persist `state.json` via the filesystem service (SPECS §9). Called after
    /// mutations that change tab metadata.
    fn persist(&self, services: &Services) -> Result<()> {
        let state = self.to_project_state(services.clock.now_millis());
        save_state(services.fs, &self.state_path, &state)
    }

    /// Record a persistent warning if not already present (SPECS §13).
    fn add_warning(&mut self, warning: impl Into<String>) {
        let warning = warning.into();
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }

    // -----------------------------------------------------------------------
    // Dispatch (SPECS §22)
    // -----------------------------------------------------------------------

    /// The command reducer (SPECS §22). Calls the services through the trait
    /// objects and returns an [`Effect`] describing what the UI should surface.
    pub fn dispatch(&mut self, cmd: Command, services: &Services) -> Result<Effect> {
        // Commands that act on the selected tab's live worktree/session are
        // refused while that tab's worktree is still being created on a
        // background worker (SPECS §16/§17). Closing/switching stays allowed so
        // the user can always cancel — important if creation itself hangs.
        if self.selected_is_creating() && requires_ready_tab(&cmd) {
            return Ok(Effect::Refused(
                "This tab is still being created — please wait.".to_string(),
            ));
        }
        match cmd {
            Command::NewAgentTab { name, agent_key } => {
                self.cmd_new_agent_tab(&name, agent_key.as_deref(), services)
            }
            Command::RenameAgentTab { new_name } => self.cmd_rename(&new_name, services),
            Command::CloseAgentTab { action } => self.cmd_close_tab(action, services),
            Command::PushBranch { confirm } => self.cmd_push(confirm, services),
            Command::FinishLocalMerge { confirm } => self.cmd_finish_merge(confirm, services),
            Command::RebaseWorktree { confirm } => self.cmd_rebase(confirm, services),
            Command::PullBase => self.cmd_pull_base(services),
            Command::CopyEnvFile => self.cmd_copy_env_file(services),
            Command::AbandonWorktree { confirm } => self.cmd_abandon(confirm, services),
            Command::NewChildTerminal | Command::OpenShell => self.cmd_new_child(services),
            Command::NewAgentTerminal { agent_key } => {
                self.cmd_new_agent_child(agent_key.as_deref(), services)
            }
            Command::CloseChildTerminal => self.cmd_close_child(),
            Command::CloseAgentTerminal => self.cmd_close_agent_child(),
            Command::SwitchAgentTab(sel) => self.cmd_switch_tab(sel),
            Command::SwitchChildTerminal(sel) => self.cmd_switch_child(sel),
            Command::SetManualStatus(status) => self.cmd_set_manual_status(status, services),
            Command::RestartAgent => self.cmd_restart_agent(services),
            Command::ShowGitStatus => self.cmd_show_git_status(services),
            Command::ShowHelp => Ok(Effect::ShowHelp),
            Command::ToggleSplitView => {
                self.toggle_split_view();
                let label = if self.split_view { "on" } else { "off" };
                Ok(Effect::Message(format!("Split view {label}.")))
            }
            Command::Quit => Ok(Effect::Quit),
        }
    }

    /// NEW-TAB FLOW (SPECS §4, §16, §17), synchronous all-in-one used by the
    /// command dispatcher and tests. Validation precedes ALL git mutation
    /// (SPECS §16). The interactive event loop instead drives the async pair
    /// [`AppState::begin_new_agent_tab`] + [`AppState::finalize_new_tab`] (with
    /// [`materialize_worktree`] on a background worker) so the UI never blocks
    /// on `git worktree add`.
    fn cmd_new_agent_tab(
        &mut self,
        name: &str,
        agent_key: Option<&str>,
        services: &Services,
    ) -> Result<Effect> {
        let job = self.begin_new_agent_tab(name, agent_key, services)?;
        let outcome = match materialize_worktree(services.git, &job) {
            Ok(()) => self.finalize_new_tab(&job.tab_id, services),
            Err(e) => Err(e),
        };
        // Either step failing must leave no dead placeholder (the TabPhase
        // contract: creation failures remove the tab entirely). A spawn failure
        // in `finalize` (e.g. a missing container image) is no exception.
        if outcome.is_err() {
            self.fail_new_tab(&job.tab_id);
        }
        outcome
    }

    /// Begin the new-tab flow: validate, plan, and reserve a placeholder
    /// ([`TabPhase::Creating`]) tab, returning the [`WorktreeJob`] the caller
    /// must run. Performs only cheap, lock-free git reads — never the slow
    /// `git worktree add` — so it is safe to call on the UI thread (SPECS §16:
    /// validation precedes mutation; the placeholder is pushed only after the
    /// agent command validates).
    pub fn begin_new_agent_tab(
        &mut self,
        name: &str,
        agent_key: Option<&str>,
        services: &Services,
    ) -> Result<WorktreeJob> {
        // (a) look up the agent in the registry.
        let key = agent_key
            .map(|k| k.to_string())
            .unwrap_or_else(|| self.registry.default_key.clone());
        let agent = self
            .registry
            .get(&key)
            .cloned()
            .ok_or_else(|| FlightDeckError::Config(format!("unknown agent '{key}'")))?;

        // (b) validate the command (or container runtime) BEFORE any git
        //     mutation (SPECS §16/§31).
        validate_launchable(
            &agent,
            &self.config.containers,
            services.container,
            &self.repo_root,
        )?;

        // (c) slug + branch name with the configured prefix.
        let slug = slugify(name);
        if slug.is_empty() {
            return Err(FlightDeckError::Config(
                "tab name produced an empty slug".to_string(),
            ));
        }
        let prefix = self.config.git.branch_prefix.clone();
        let branch = branch_name(&prefix, &slug);

        // Refuse a second placeholder for the same branch/slug: the tab `id`
        // is only unique to the second (`{slug}-{created_at}`), so two rapid
        // creates for the same name could otherwise collide and let
        // `finalize_new_tab`/`fail_new_tab` target the wrong tab.
        if self.tabs.iter().any(|t| t.meta.branch == branch) {
            return Err(FlightDeckError::Refused(format!(
                "an Agent Tab for branch '{branch}' already exists"
            )));
        }

        // (d) decide create vs attach (surface attach, never silent, SPECS §11).
        let decision = decide_branch(services.git, &branch)?;
        let attached = matches!(decision, BranchDecision::AttachExisting);

        // (e) plan the worktree.
        let worktrees_root_rel = self.config.worktrees.root.clone();
        let target = worktree_path(&self.repo_root, &worktrees_root_rel, &slug);
        let worktrees_root_abs = to_absolute(&self.repo_root, Path::new(&worktrees_root_rel));
        let plan = plan_worktree(services.git, &branch, &target, &worktrees_root_abs)?;

        // (f) resolve the worktree location + whether the worker must create it.
        //     The slow `git worktree add` is deferred to `materialize_worktree`.
        let (worktree_abs, needs_create, create_branch) = match plan {
            // Create the branch from base only when it does not already exist.
            WorktreePlan::Create => (target, true, matches!(decision, BranchDecision::Create)),
            WorktreePlan::ReuseManaged { path } => (path, false, false),
            WorktreePlan::RefuseCheckedOutElsewhere { path } => {
                return Err(FlightDeckError::Refused(format!(
                    "branch '{branch}' is already checked out at {}",
                    path.display()
                )));
            }
        };

        // (g) record the placeholder TabState. The base SHA is a cheap, lock-free
        //     read so we resolve it now; the session is spawned in `finalize`.
        let base_commit_sha = services.git.rev_parse(&self.base_branch)?;
        let worktree_rel = to_relative(&self.repo_root, &worktree_abs)
            .unwrap_or_else(|_| worktree_abs.clone())
            .to_string_lossy()
            .to_string();
        let created_at = services.clock.now_iso8601();
        let id = format!("{slug}-{created_at}");
        let meta = TabState {
            id: id.clone(),
            name: name.to_string(),
            slug,
            agent: key,
            branch: branch.clone(),
            worktree_path_relative: worktree_rel,
            base_branch: self.base_branch.clone(),
            base_commit_sha,
            created_at,
            attached_existing_branch: attached,
            recovered: false,
            last_known_status: InterpretedStatus::Starting.as_str().to_string(),
            manual_status: None,
            containerized: false,
            container_image: None,
        };
        self.tabs.push(RuntimeTab {
            meta,
            phase: TabPhase::Creating,
            session: Session::new(),
            interpreted: None,
            interpreted_at_ms: None,
            last_activity_ms: None,
            status_file: None,
            status_file_seen: None,
            notify_armed: false,
        });
        // Focus the new (placeholder) tab so the user sees the progress.
        self.selected_tab = Some(self.tabs.len() - 1);

        Ok(WorktreeJob {
            tab_id: id,
            branch,
            base_branch: self.base_branch.clone(),
            worktree_abs,
            needs_create,
            create_branch,
        })
    }

    /// Finalize a placeholder tab once its worktree has been materialized: spawn
    /// the primary agent (NO initial prompt, SPECS §17), flip the tab to
    /// [`TabPhase::Ready`], and persist. Returns [`Effect::None`] if the
    /// placeholder is gone (e.g. the user closed it while it was creating).
    pub fn finalize_new_tab(&mut self, tab_id: &str, services: &Services) -> Result<Effect> {
        let Some(idx) = self.tabs.iter().position(|t| t.meta.id == tab_id) else {
            return Ok(Effect::None);
        };

        // Re-resolve everything the spawn needs from the recorded metadata.
        let agent_key = self.tabs[idx].meta.agent.clone();
        let agent = self
            .registry
            .get(&agent_key)
            .cloned()
            .ok_or_else(|| FlightDeckError::Config(format!("unknown agent '{agent_key}'")))?;
        let worktree_abs = to_absolute(
            &self.repo_root,
            Path::new(&self.tabs[idx].meta.worktree_path_relative),
        );
        let branch = self.tabs[idx].meta.branch.clone();
        let attached = self.tabs[idx].meta.attached_existing_branch;
        let tab_id = self.tabs[idx].meta.id.clone();

        // Share the base folder's `.env`/`.env.local` into the worktree before
        // the agent starts, so it inherits the developer's secrets. Best-effort:
        // never fails the session (SPECS §17 keeps creation robust).
        self.link_env_files(&worktree_abs, services);

        // Resolve the launch (local command, or a fresh `podman run`) then spawn
        // the primary terminal (fast; stays on the UI thread — SPECS §31 keeps
        // the spawn path synchronous and never builds an image here).
        let spawn = self.build_primary_spawn(&tab_id, &agent, &worktree_abs, services, false)?;
        // Fresh container: start it detached before attaching its PTY.
        if let Some(start_args) = &spawn.start_args {
            services.container.start_detached(start_args)?;
        }
        let mut session = Session::new();
        if let Err(e) = session.spawn_primary(
            services.pty,
            &spawn.command,
            &spawn.args,
            &worktree_abs,
            self.pty_size,
        ) {
            // The container (if any) was already started; a spawn failure here
            // must not leak it, since the tab is about to be removed by
            // `fail_new_tab` with no record of the container left behind.
            if spawn.start_args.is_some() {
                let _ = services
                    .container
                    .remove_container(&container_name(&tab_id), true);
            }
            return Err(e);
        }

        let tab = &mut self.tabs[idx];
        tab.session = session;
        tab.phase = TabPhase::Ready;
        tab.interpreted = Some(InterpretedStatus::Starting);
        tab.status_file = Some(agent_status_file(&worktree_abs));
        tab.meta.containerized = spawn.containerized;
        tab.meta.container_image = spawn.image;

        self.persist(services)?;

        if attached {
            Ok(Effect::AttachedExisting { branch })
        } else {
            Ok(Effect::Message(format!("Created Agent Tab on {branch}")))
        }
    }

    /// Symlink the base folder's `.env` / `.env.local` into a new worktree so
    /// the agent shares the developer's secrets without a copy that could drift.
    /// Best-effort: any file that is absent in the base, already present in the
    /// worktree, or fails to link is silently skipped — a missing `.env` must
    /// never bother the user or fail session creation.
    fn link_env_files(&self, worktree: &Path, services: &Services) {
        for name in [".env", ".env.local"] {
            let source = self.repo_root.join(name);
            let destination = worktree.join(name);
            if services.fs.exists(&source) && !services.fs.exists(&destination) {
                let _ = services.fs.symlink(&source, &destination);
            }
        }
    }

    /// Remove a placeholder tab whose worktree creation failed, restoring a
    /// sensible selection. The error itself is surfaced by the caller.
    pub fn fail_new_tab(&mut self, tab_id: &str) {
        if let Some(idx) = self.tabs.iter().position(|t| t.meta.id == tab_id) {
            self.tabs.remove(idx);
            self.fix_selection_after_removal(idx);
        }
    }

    /// Rename: change only the tab `name`; never touch branch/slug/worktree/base
    /// metadata (SPECS §18).
    fn cmd_rename(&mut self, new_name: &str, services: &Services) -> Result<Effect> {
        let Some(tab) = self.selected_mut() else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        tab.meta.name = new_name.to_string();
        self.persist(services)?;
        Ok(Effect::Message(format!("Renamed tab to '{new_name}'")))
    }

    /// Close Agent Tab (SPECS §25). When `action` is `None`, return the option
    /// set; never auto-escalate to force-kill.
    fn cmd_close_tab(
        &mut self,
        action: Option<CloseAction>,
        services: &Services,
    ) -> Result<Effect> {
        let Some(idx) = self.selected_tab else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };

        let Some(action) = action else {
            return Ok(Effect::CloseTabOptions(CloseTabOptions::standard()));
        };

        let tab = &mut self.tabs[idx];
        match action {
            CloseAction::CtrlCPrimary => {
                tab.session.ctrl_c_primary()?;
                // Per SPECS §25 we ask again if it stays alive: do not remove yet.
                return Ok(Effect::Message("Sent Ctrl-C to primary agent.".to_string()));
            }
            CloseAction::CtrlCAll => {
                tab.session.ctrl_c_all()?;
                return Ok(Effect::Message(
                    "Sent Ctrl-C to all terminals in this tab.".to_string(),
                ));
            }
            CloseAction::IfAllStopped => {
                if !tab.session.all_stopped() {
                    return Ok(Effect::Refused(
                        "Processes are still running; tab not closed.".to_string(),
                    ));
                }
                // fall through to removal
            }
            CloseAction::ForceTerminate => {
                tab.session.terminate_all()?;
                // fall through to removal
            }
        }

        // Remove the backing container (if any), then the tab from runtime state.
        let container_result = self.destroy_container_if_any(idx, services);
        self.tabs.remove(idx);
        self.fix_selection_after_removal(idx);
        self.persist(services)?;
        match container_result {
            Ok(()) => Ok(Effect::Message("Closed Agent Tab.".to_string())),
            Err(e) => Ok(Effect::Warning(format!(
                "Closed Agent Tab, but removing its container failed: {e}. It may still be running."
            ))),
        }
    }

    /// Push (SPECS §14): plan; warn on uncommitted; on confirm push; then PR URL.
    fn cmd_push(&mut self, confirm: Option<PushConfirm>, services: &Services) -> Result<Effect> {
        let Some(tab) = self.selected() else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let worktree = to_absolute(&self.repo_root, Path::new(&tab.meta.worktree_path_relative));
        let branch = tab.meta.branch.clone();
        let base = tab.meta.base_branch.clone();

        let plan = plan_push(services.git, &worktree)?;
        if matches!(plan, PushPlan::UncommittedChanges) {
            match confirm {
                None => return Ok(Effect::PushWarning(plan)),
                Some(PushConfirm::Cancel) => {
                    return Ok(Effect::Message("Push cancelled.".to_string()))
                }
                Some(PushConfirm::PushCommitted) => {}
            }
        }

        let remote = self.config.git.default_remote.clone();
        push_branch(services.git, &remote, &branch, &worktree)?;

        match github_pr_url(services.git, &remote, &base, &branch)? {
            Some(url) => Ok(Effect::PrUrl(url)),
            None => Ok(Effect::Message(format!("Pushed {branch} to {remote}."))),
        }
    }

    /// Finish / Local Merge (SPECS §13/§15). Dirty base → disabled with the
    /// persistent warning; otherwise check preconditions and merge only when
    /// allowed; never auto-resolve conflicts.
    fn cmd_finish_merge(&mut self, confirm: bool, services: &Services) -> Result<Effect> {
        let Some(idx) = self.selected_tab else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let tab = &self.tabs[idx];
        let agent_branch = tab.meta.branch.clone();
        let base_branch = tab.meta.base_branch.clone();
        let agent_worktree =
            to_absolute(&self.repo_root, Path::new(&tab.meta.worktree_path_relative));
        let base_worktree = self.repo_root.clone();
        // A running primary agent does NOT block the merge — "Finish" stops it and
        // removes the worktree afterwards. We only track it to inform the prompt.
        let primary_running = tab.session.primary_state() == ProcessState::Running;

        // SPECS §13: if base is dirty, local merge is disabled — record the
        // persistent warning and refuse.
        if services.git.is_dirty(&base_worktree)? {
            self.add_warning("Base repo dirty: local merge disabled");
            return Ok(Effect::Warning(
                "Base worktree has uncommitted changes. Local merge is disabled.\nRecommended action: push this branch and create a PR instead.".to_string(),
            ));
        }

        let req = MergeRequest {
            base_branch: &base_branch,
            agent_branch: &agent_branch,
            base_worktree: &base_worktree,
            agent_worktree: &agent_worktree,
        };

        // Technical preconditions (both worktrees clean, both branches exist).
        match check_merge_preconditions(services.git, &req)? {
            MergeDecision::Refused(reason) => return Ok(Effect::Refused(reason)),
            MergeDecision::Allowed => {}
        }

        // SPECS §15: the user must explicitly confirm. The first dispatch asks;
        // the UI re-dispatches with `confirm: true`.
        if !confirm {
            return Ok(Effect::MergeConfirm {
                agent_branch,
                base_branch,
                primary_running,
            });
        }

        let outcome = merge_back(services.git, &req)?;
        if outcome.conflicted {
            return Ok(Effect::Refused(outcome.message));
        }
        if !outcome.merged {
            return Ok(Effect::Refused(outcome.message));
        }

        // Merge succeeded: stop the agent's session and remove its worktree, then
        // drop the tab (the work now lives on the base branch). Force removal — the
        // merge is already committed onto base, so nothing is lost.
        if let Err(e) = self.tabs[idx].session.terminate_all() {
            // Mirror the force-close path: a termination failure must not be
            // silently swallowed right before an irreversible teardown, or the
            // still-running process(es) are orphaned once the worktree is gone.
            return Ok(Effect::Warning(format!(
                "Merged {agent_branch} into {base_branch}, but stopping the session failed: {e}. The worktree was not removed; retry closing it manually."
            )));
        }
        let _ = self.destroy_container_if_any(idx, services);
        match remove_worktree_if_safe(services.git, services.fs, &agent_worktree, true) {
            Ok(()) => {
                self.tabs.remove(idx);
                self.fix_selection_after_removal(idx);
                self.persist(services)?;
                Ok(Effect::Message(format!(
                    "Merged {agent_branch} into {base_branch} and removed the worktree."
                )))
            }
            Err(e) => {
                // The merge landed; only cleanup failed. Surface it but keep the tab.
                self.persist(services)?;
                Ok(Effect::Warning(format!(
                    "Merged {agent_branch} into {base_branch}, but removing the worktree failed: {e}"
                )))
            }
        }
    }

    /// Rebase Worktree onto the base branch (SPECS §5 carve-out). Checks
    /// preconditions, requires explicit confirmation, then rebases; aborts and
    /// reports on conflict, never resolving them. On success the worktree branch
    /// sits on top of the current base, so the stored base SHA is advanced to the
    /// base tip — drift (SPECS §12) then reflects that the base has been
    /// incorporated. The worktree branch's history is rewritten, so a previously
    /// pushed branch will need a force-push to update the remote / PR.
    fn cmd_rebase(&mut self, confirm: bool, services: &Services) -> Result<Effect> {
        let Some(idx) = self.selected_tab else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let tab = &self.tabs[idx];
        let agent_branch = tab.meta.branch.clone();
        let base_branch = tab.meta.base_branch.clone();
        let base_commit_sha = tab.meta.base_commit_sha.clone();
        let agent_worktree =
            to_absolute(&self.repo_root, Path::new(&tab.meta.worktree_path_relative));
        let primary_running = tab.session.primary_state() == ProcessState::Running;

        let req = RebaseRequest {
            base_branch: &base_branch,
            agent_branch: &agent_branch,
            agent_worktree: &agent_worktree,
        };

        // Technical preconditions (agent worktree clean, both branches exist).
        match check_rebase_preconditions(services.git, &req)? {
            RebaseDecision::Refused(reason) => return Ok(Effect::Refused(reason)),
            RebaseDecision::Allowed => {}
        }

        // History rewrite — always confirm first. The UI re-dispatches with
        // `confirm: true`.
        if !confirm {
            let drift = base_drift(services.git, &base_branch, &base_commit_sha).unwrap_or(0);
            return Ok(Effect::RebaseConfirm {
                agent_branch,
                base_branch,
                drift,
                primary_running,
            });
        }

        let outcome = rebase_onto_base(services.git, &req)?;
        if outcome.conflicted || !outcome.rebased {
            return Ok(Effect::Refused(outcome.message));
        }

        // Rebase landed: the branch now sits on the current base, so advance the
        // stored base SHA (best effort) and persist so §12 drift reflects it.
        if let Ok(sha) = services.git.rev_parse(&base_branch) {
            self.tabs[idx].meta.base_commit_sha = sha;
        }
        self.persist(services)?;
        Ok(Effect::Message(format!(
            "Rebased {agent_branch} onto {base_branch}. If the branch was pushed, force-push to update the remote / PR."
        )))
    }

    /// Pull base (SPECS §5.2). Runs `git pull --rebase` in the base folder (the
    /// repo root) so merged PRs land on the local base branch without leaving
    /// FlightDeck. A global action — it never touches an Agent Tab's worktree.
    /// Refuses up front if the base folder is dirty (pull --rebase would refuse
    /// anyway, but we surface a clear reason); aborts on conflict, leaving the
    /// base folder exactly as it was. The base branch must be the one checked out
    /// in the root.
    fn cmd_pull_base(&mut self, services: &Services) -> Result<Effect> {
        let base = self.base_branch.clone();
        let root = self.repo_root.clone();

        // The root folder must have the base branch checked out — Pull base is
        // defined as updating the base, not whatever else might be checked out.
        let current = services.git.current_branch(&root)?;
        if current != base {
            return Ok(Effect::Refused(format!(
                "Base folder is on '{current}', not the base branch '{base}'."
            )));
        }

        // `git pull --rebase` refuses on a dirty tree; FlightDeck never stashes
        // or discards (SPECS §5), so surface a clear refusal instead.
        if services.git.is_dirty(&root)? {
            return Ok(Effect::Refused(format!(
                "Base folder has uncommitted changes; commit or stash before pulling {base}."
            )));
        }

        let outcome = services.git.pull_base(&root)?;
        if outcome.conflicted || !outcome.rebased {
            return Ok(Effect::Refused(outcome.message));
        }

        Ok(Effect::Message(format!(
            "Pulled {base} (git pull --rebase)."
        )))
    }

    /// Abandon Worktree (SPECS §5/§15). Always returns [`Effect::AbandonWarning`]
    /// first so the UI confirms, even for a clean worktree (`dirty` tells the
    /// prompt whether uncommitted changes would be lost); once the user confirms
    /// (`confirm` true) it is force-removed regardless of uncommitted changes.
    /// The tab is dropped after a successful removal.
    fn cmd_abandon(&mut self, confirm: bool, services: &Services) -> Result<Effect> {
        let Some(idx) = self.selected_tab else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let worktree = to_absolute(
            &self.repo_root,
            Path::new(&self.tabs[idx].meta.worktree_path_relative),
        );

        // Abandoning always asks first (SPECS §5/§15); a clean worktree is also
        // confirmed, just without the dirty-changes warning.
        if !confirm {
            let dirty = services.git.is_dirty(&worktree)?;
            return Ok(Effect::AbandonWarning { dirty });
        }

        // Tear down any live session + container BEFORE removing the worktree.
        // On Windows a directory cannot be deleted while a process still holds it
        // open (the agent/shell keeps its cwd inside the worktree, and a container
        // may bind-mount it), so `git worktree remove` would fail with a
        // permission-denied error. Mirrors the merge path's ordering (SPECS §5/§15).
        if let Err(e) = self.tabs[idx].session.terminate_all() {
            // Don't remove the worktree out from under a session we failed to
            // stop — that would orphan the still-running process(es) with a
            // deleted cwd. Leave the tab in place so the user can retry.
            return Ok(Effect::Warning(format!(
                "Could not stop the running session: {e}. The worktree was not removed; retry Abandon once the process has stopped."
            )));
        }
        let _ = self.destroy_container_if_any(idx, services);

        match remove_worktree_if_safe(services.git, services.fs, &worktree, confirm) {
            Ok(()) => {
                self.tabs.remove(idx);
                self.fix_selection_after_removal(idx);
                self.persist(services)?;
                Ok(Effect::Message("Abandoned worktree.".to_string()))
            }
            Err(FlightDeckError::Refused(reason)) => Ok(Effect::Refused(reason)),
            Err(e) => Err(e),
        }
    }

    /// Copy the first available base env file into the selected worktree.
    fn cmd_copy_env_file(&mut self, services: &Services) -> Result<Effect> {
        let Some(tab) = self.selected() else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let worktree = to_absolute(&self.repo_root, Path::new(&tab.meta.worktree_path_relative));
        let Some(name) = [".env.local", ".env"]
            .into_iter()
            .find(|name| services.fs.exists(&self.repo_root.join(name)))
        else {
            return Ok(Effect::Refused(
                "No .env.local or .env found in base folder.".to_string(),
            ));
        };

        let source = self.repo_root.join(name);
        let destination = worktree.join(name);
        let contents = services.fs.read_to_string(&source)?;
        services.fs.write(&destination, &contents)?;
        Ok(Effect::Message(format!("Copied {name} to worktree.")))
    }

    /// New child shell terminal in the selected tab's worktree (SPECS §19). When
    /// the tab's agent runs in a container, the shell runs *inside* it via
    /// `podman exec` so it shares `/workspace` and the toolchain (SPECS §31).
    fn cmd_new_child(&mut self, services: &Services) -> Result<Effect> {
        let size = self.pty_size;
        let repo_root = self.repo_root.clone();
        let Some(tab) = self.selected_mut() else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let cwd = to_absolute(&repo_root, Path::new(&tab.meta.worktree_path_relative));
        let (cmd, args) = if tab.meta.containerized {
            let name = container_name(&tab.meta.id);
            // The container is always Linux, so the child shell must be a
            // Linux-native shell run *inside* it — not the host's default
            // shell (which is PowerShell on Windows and absent from the
            // container). See `container_shell`.
            (
                "podman".to_string(),
                build_exec_args(&name, &container_shell(), &[]),
            )
        } else {
            shell_launch()
        };
        let _idx = tab
            .session
            .spawn_child(services.pty, &cmd, &args, &cwd, size)?;
        // The new shell tab appearing is its own confirmation; no toast needed.
        Ok(Effect::None)
    }

    /// Spawn an additional agent terminal in the selected tab's worktree/session,
    /// shown as another "agent" tab on the horizontal row. `agent_key` picks the
    /// backend (falling back to the session tab's own agent when `None`). Runs in
    /// the same worktree — a `podman exec` into the tab's container when
    /// containerized, otherwise a local launch.
    fn cmd_new_agent_child(
        &mut self,
        agent_key: Option<&str>,
        services: &Services,
    ) -> Result<Effect> {
        let size = self.pty_size;
        let repo_root = self.repo_root.clone();
        let Some(idx) = self.selected_tab else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let agent_key = agent_key
            .map(str::to_string)
            .unwrap_or_else(|| self.tabs[idx].meta.agent.clone());
        let agent = self
            .registry
            .get(&agent_key)
            .cloned()
            .ok_or_else(|| FlightDeckError::Config(format!("unknown agent '{agent_key}'")))?;
        validate_launchable(
            &agent,
            &self.config.containers,
            services.container,
            &repo_root,
        )?;

        let cwd = to_absolute(
            &repo_root,
            Path::new(&self.tabs[idx].meta.worktree_path_relative),
        );
        let (cmd, args) = if self.tabs[idx].meta.containerized {
            let name = container_name(&self.tabs[idx].meta.id);
            (
                "podman".to_string(),
                build_exec_args(&name, &agent.command, &agent.args),
            )
        } else {
            let launch = build_launch(&agent, &cwd);
            (launch.command, launch.args)
        };
        let tab = &mut self.tabs[idx];
        tab.session
            .spawn_agent_child(services.pty, &cmd, &args, &cwd, size)?;
        // The new agent tab appearing is its own confirmation; no toast needed.
        Ok(Effect::None)
    }

    /// Close the selected tab's currently-selected child terminal (SPECS §19).
    fn cmd_close_child(&mut self) -> Result<Effect> {
        let Some(tab) = self.selected_mut() else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let Some(child) = tab.session.selected_child() else {
            return Ok(Effect::Refused("No child terminal selected.".to_string()));
        };
        tab.session.close_child(child)?;
        // The tab disappearing is its own confirmation; no toast needed.
        Ok(Effect::None)
    }

    /// Close the selected tab's currently-selected child terminal, but only when
    /// it is an additional *agent* (not a shell). Refuses otherwise (SPECS §19).
    fn cmd_close_agent_child(&mut self) -> Result<Effect> {
        let Some(tab) = self.selected_mut() else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let Some(child) = tab.session.selected_child() else {
            return Ok(Effect::Refused("No agent tab selected.".to_string()));
        };
        let is_agent = tab.session.child(child).map(|t| t.kind)
            == Some(crate::terminal::session::TerminalKind::Agent);
        if !is_agent {
            return Ok(Effect::Refused(
                "The selected tab is not an agent.".to_string(),
            ));
        }
        tab.session.close_child(child)?;
        Ok(Effect::None)
    }

    /// Switch the selected Agent Tab (SPECS §22). Preserves each tab's own
    /// selected child (sessions are untouched).
    fn cmd_switch_tab(&mut self, sel: Selector) -> Result<Effect> {
        let len = self.tabs.len();
        match Self::resolve_selector(sel, self.selected_tab, len) {
            Some(idx) => {
                self.selected_tab = Some(idx);
                Ok(Effect::None)
            }
            None => Ok(Effect::Refused("No such Agent Tab.".to_string())),
        }
    }

    /// Switch the selected tab's active terminal (SPECS §22). `Next`/`Prev`
    /// cycle the full horizontal tab ring — the primary "agent" terminal plus
    /// every child shell — wrapping around. `Index(i)` selects child shell `i`.
    fn cmd_switch_child(&mut self, sel: Selector) -> Result<Effect> {
        let Some(tab) = self.selected_mut() else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let child_count = tab.session.child_count();
        match sel {
            Selector::Index(i) => {
                if i < child_count {
                    tab.session.switch_child(i)?;
                    Ok(Effect::None)
                } else {
                    Ok(Effect::Refused("No such child terminal.".to_string()))
                }
            }
            Selector::Next | Selector::Prev => {
                // Ring positions: 0 = primary (agent), 1..=child_count = children.
                let ring = child_count + 1;
                let cur = match tab.session.selected_child() {
                    None => 0,
                    Some(i) => i + 1,
                };
                let next = match sel {
                    Selector::Next => (cur + 1) % ring,
                    Selector::Prev => (cur + ring - 1) % ring,
                    Selector::Index(_) => unreachable!(),
                };
                if next == 0 {
                    tab.session.focus_primary();
                } else {
                    tab.session.switch_child(next - 1)?;
                }
                Ok(Effect::None)
            }
        }
    }

    /// Set/clear the manual status override (SPECS §24). The process state stays
    /// visible — only the manual field changes.
    fn cmd_set_manual_status(
        &mut self,
        status: Option<ManualStatus>,
        services: &Services,
    ) -> Result<Effect> {
        let Some(tab) = self.selected_mut() else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        tab.meta.manual_status = status.map(|s| s.as_str().to_string());
        self.persist(services)?;
        match status {
            Some(s) => Ok(Effect::Message(format!("Manual status: {}", s.as_str()))),
            None => Ok(Effect::Message("Cleared manual status.".to_string())),
        }
    }

    /// Resolve how to launch (or reattach) a primary terminal (SPECS §31).
    ///
    /// - Local mode (`containers.enabled == false`): the agent command directly.
    /// - Container mode, `allow_attach` + a running container: `podman attach`.
    /// - Container mode otherwise: a fresh `podman run` (guardrail-checked). The
    ///   image must already exist; a missing image is refused with guidance
    ///   (the fast launch path never builds — SPECS §31 keeps spawn synchronous).
    fn build_primary_spawn(
        &self,
        tab_id: &str,
        agent: &AgentDef,
        worktree_abs: &Path,
        services: &Services,
        allow_attach: bool,
    ) -> Result<PrimarySpawn> {
        if !self.config.containers.enabled {
            let launch = build_launch(agent, worktree_abs);
            return Ok(PrimarySpawn {
                start_args: None,
                command: launch.command,
                args: launch.args,
                containerized: false,
                image: None,
            });
        }

        let name = container_name(tab_id);
        // Reattach to a still-running container (the detached run survives a
        // FlightDeck restart) — no fresh start needed.
        if allow_attach && services.container.container_state(&name)? == ContainerState::Running {
            return Ok(PrimarySpawn {
                start_args: None,
                command: "podman".to_string(),
                args: build_attach_args(&name),
                containerized: true,
                image: None,
            });
        }

        // Fresh run: verify the image (no building here), then start the
        // container detached and attach to it. The caller runs `start_args`.
        let rhash = repo_hash(&self.repo_root);
        let image = image::resolve_image_tag(&rhash, &agent.key, &self.config.containers);
        if !services.container.image_exists(&image)? {
            return Err(image::missing_image_error(&image, &agent.key));
        }
        // Clear any stale exited container so `--name` does not clash.
        let _ = services.container.remove_container(&name, true);

        let spec =
            self.container_spec(tab_id, &name, &rhash, &image, agent, worktree_abs, services);
        let run_args = build_run_args(&spec);
        enforce_guardrails(&run_args)?;
        Ok(PrimarySpawn {
            start_args: Some(run_args),
            command: "podman".to_string(),
            args: build_attach_args(&name),
            containerized: true,
            image: Some(image),
        })
    }

    /// Assemble the [`ContainerSpec`] for a fresh run, resolving auth mounts,
    /// env-allowlist secrets, host UID, and platform mount flags (SPECS §31).
    #[allow(clippy::too_many_arguments)]
    fn container_spec(
        &self,
        tab_id: &str,
        name: &str,
        rhash: &str,
        image: &str,
        agent: &AgentDef,
        worktree_abs: &Path,
        services: &Services,
    ) -> ContainerSpec {
        let exec = &self.config.containers;
        let user_auth = !exec.auth.mounts.is_empty() || !exec.auth.env_allow.is_empty();
        let (auth_mounts, env) = if user_auth || exec.base_image.is_some() {
            // Explicit auth config (or a custom base image, whose paths we can't
            // assume) — use exactly what the project declared.
            let mounts = exec
                .auth
                .mounts
                .iter()
                .map(|m| ResolvedAuthMount {
                    host_path: expand_tilde(&m.host_path),
                    container_path: m.container_path.clone(),
                    writable: m.writable,
                })
                .collect();
            let mut env = Vec::new();
            for key in &exec.auth.env_allow {
                if let Ok(val) = std::env::var(key) {
                    env.push((key.clone(), val));
                }
            }
            (mounts, env)
        } else {
            // No auth configured + the default base image: pass the host agent's
            // credentials through automatically so the user need not re-login
            // (SPECS §31). Mounts are writable so a fresh login persists, and
            // skipped when the host path is absent (avoids creating empty dirs).
            let mut mounts: Vec<ResolvedAuthMount> = default_auth_mounts(&agent.key)
                .into_iter()
                .filter_map(|(host, ctr)| {
                    let host_path = expand_tilde(host);
                    host_path.exists().then_some(ResolvedAuthMount {
                        host_path,
                        container_path: ctr.to_string(),
                        writable: true,
                    })
                })
                .collect();
            mounts.shrink_to_fit();
            let mut env = Vec::new();
            for key in default_env_allow(&agent.key) {
                if let Ok(val) = std::env::var(key) {
                    env.push((key.to_string(), val));
                }
            }
            (mounts, env)
        };

        // On the default base image, pin HOME to the `agent` user's home: the
        // process runs as the host UID (no passwd entry for it), so without this
        // HOME would be unset/`/` and the agent could not write its config.
        let mut env = env;
        if exec.base_image.is_none() && !env.iter().any(|(k, _)| k == "HOME") {
            env.push(("HOME".to_string(), AGENT_HOME.to_string()));
        }

        ContainerSpec {
            name: name.to_string(),
            labels: standard_labels(tab_id, rhash),
            image: image.to_string(),
            workspace_host: worktree_abs.to_path_buf(),
            agent_cmd: agent.command.clone(),
            agent_args: agent.args.clone(),
            cpu: exec.limits.cpu.clone(),
            memory: exec.limits.memory.clone(),
            pids: exec.limits.pids,
            forward_ports: exec.forward_ports.clone(),
            auth_mounts,
            env,
            host_uid: services.container.host_uid(),
            mount_flags: platform_mount_flags(),
        }
    }

    /// Remove the container backing tab `idx`, if it is containerized. Called on
    /// the teardown paths (force-close, abandon, merge) so a stopped session
    /// leaves no container behind (SPECS §31).
    fn destroy_container_if_any(&self, idx: usize, services: &Services) -> Result<()> {
        if !self.tabs[idx].meta.containerized {
            return Ok(());
        }
        let name = container_name(&self.tabs[idx].meta.id);
        services.container.remove_container(&name, true)
    }

    /// Spawn (or re-spawn) the primary agent for tab `idx`. Re-validates the
    /// agent first (SPECS §16), launches locally or in a container per
    /// `containers.enabled` (SPECS §31), and marks the tab no longer recovered.
    /// `allow_attach` reconnects to a still-running container instead of running
    /// a fresh one (used on resume, not on an explicit restart).
    fn start_primary_for(
        &mut self,
        idx: usize,
        services: &Services,
        allow_attach: bool,
    ) -> Result<()> {
        let size = self.pty_size;
        let repo_root = self.repo_root.clone();
        let agent_key = self.tabs[idx].meta.agent.clone();
        let agent = self
            .registry
            .get(&agent_key)
            .cloned()
            .ok_or_else(|| FlightDeckError::Config(format!("unknown agent '{agent_key}'")))?;
        validate_launchable(
            &agent,
            &self.config.containers,
            services.container,
            &self.repo_root,
        )?;

        let cwd = to_absolute(
            &repo_root,
            Path::new(&self.tabs[idx].meta.worktree_path_relative),
        );
        let status_file = agent_status_file(&cwd);
        let tab_id = self.tabs[idx].meta.id.clone();
        let spawn = self.build_primary_spawn(&tab_id, &agent, &cwd, services, allow_attach)?;
        // Fresh container: start it detached before attaching its PTY.
        if let Some(start_args) = &spawn.start_args {
            services.container.start_detached(start_args)?;
        }

        let tab = &mut self.tabs[idx];
        // An explicit restart always starts fresh (never silently reattaches,
        // see `cmd_restart_agent`): terminate any still-running primary first,
        // or `spawn_primary` below would silently drop it without killing the
        // process, leaking it. `resume_agents` only calls this when there is
        // no running primary, so this is a no-op on that path.
        if tab.session.primary_state() == ProcessState::Running {
            if let Some(primary) = tab.session.primary_mut() {
                let _ = primary.session_mut().terminate_tree();
            }
        }
        if let Err(e) =
            tab.session
                .spawn_primary(services.pty, &spawn.command, &spawn.args, &cwd, size)
        {
            // The container (if any) was already started; don't leak it on a
            // spawn failure.
            if spawn.start_args.is_some() {
                let _ = services
                    .container
                    .remove_container(&container_name(&tab_id), true);
            }
            return Err(e);
        }
        tab.interpreted = Some(InterpretedStatus::Starting);
        tab.interpreted_at_ms = None;
        tab.last_activity_ms = None;
        tab.status_file = Some(status_file);
        tab.status_file_seen = None;
        tab.meta.containerized = spawn.containerized;
        if let Some(image) = spawn.image {
            tab.meta.container_image = Some(image);
        }
        tab.meta.recovered = false;
        Ok(())
    }

    /// Restart the primary agent of the selected (recovered/stopped) tab
    /// (SPECS §10, §23). Re-validates the agent before spawning.
    fn cmd_restart_agent(&mut self, services: &Services) -> Result<Effect> {
        let Some(idx) = self.selected_tab else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        // An explicit restart always starts fresh (never silently reattaches).
        self.start_primary_for(idx, services, false)?;
        self.persist(services)?;
        Ok(Effect::Message("Restarted primary agent.".to_string()))
    }

    /// Start the primary agent for every resumed tab that has no running primary
    /// and whose worktree still exists on disk (used when resuming a session).
    ///
    /// Best-effort: a tab whose agent command is missing or whose worktree is
    /// gone is skipped rather than aborting the whole resume. Returns the number
    /// of agents started. This is an explicit step the wiring layer invokes —
    /// `AppState::new` and `recover` themselves never spawn (SPECS §10).
    pub fn resume_agents(&mut self, services: &Services) -> usize {
        let mut started = 0usize;
        for idx in 0..self.tabs.len() {
            if self.tabs[idx].session.primary_state() != ProcessState::NotStarted {
                continue;
            }
            let cwd = to_absolute(
                &self.repo_root,
                Path::new(&self.tabs[idx].meta.worktree_path_relative),
            );
            if !services.fs.exists(&cwd) {
                continue;
            }
            // `allow_attach` reconnects to a still-running container (the
            // detached run survives a restart); if the container is gone it
            // starts a fresh one — matching how local agents resume, so a tab is
            // never left without an agent. Best-effort per tab.
            if self.start_primary_for(idx, services, true).is_ok() {
                started += 1;
            }
        }
        if started > 0 {
            let _ = self.persist(services);
        }
        started
    }

    /// Show the git status panel for the selected tab (SPECS §21).
    fn cmd_show_git_status(&mut self, services: &Services) -> Result<Effect> {
        let Some(tab) = self.selected() else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let branch = tab.meta.branch.clone();
        let base_branch = tab.meta.base_branch.clone();
        let base_commit_sha = tab.meta.base_commit_sha.clone();
        let worktree = to_absolute(&self.repo_root, Path::new(&tab.meta.worktree_path_relative));
        let status = collect_status(
            services.git,
            &branch,
            &base_branch,
            &base_commit_sha,
            &worktree,
        )?;
        // Surface the GitHub PR compare URL once the branch has an upstream
        // (i.e. it has been pushed) and the remote is a GitHub remote (SPECS
        // §21). Best-effort: any remote-lookup failure simply omits the URL.
        let pr_url = if status.upstream.is_some() {
            let remote = self.config.git.default_remote.clone();
            github_pr_url(services.git, &remote, &base_branch, &branch).unwrap_or(None)
        } else {
            None
        };
        Ok(Effect::GitStatus {
            status: Box::new(status),
            pr_url,
        })
    }

    /// After removing the tab at `removed`, clamp `selected_tab` so it stays
    /// valid and points at a sensible neighbour (SPECS §26 "maintains selected
    /// tab").
    fn fix_selection_after_removal(&mut self, removed: usize) {
        self.selected_tab = if self.tabs.is_empty() {
            None
        } else {
            let sel = self.selected_tab.unwrap_or(0);
            let new = if sel > removed || sel >= self.tabs.len() {
                sel.saturating_sub(1)
            } else {
                sel
            };
            Some(new.min(self.tabs.len() - 1))
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{AgentDef, ContainerState, StatusPatterns, UiConfig, WorktreesConfig};
    use crate::persistence::project_state::default_state;
    use crate::testing::{FakeClock, FakeContainerRuntime, FakeFs, FakeGit, FakePty};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    // --- Test scaffolding -------------------------------------------------

    /// An agent whose command is a real executable file (absolute path) so
    /// `validate_agent` passes via the `contains('/')` branch.
    fn make_real_agent(dir: &TempDir, key: &str) -> (AgentDef, String) {
        let path = dir.path().join(key);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        let command = path.to_str().unwrap().to_string();
        (
            AgentDef {
                key: key.to_string(),
                display_name: key.to_string(),
                command: command.clone(),
                args: vec![],
                status_patterns: StatusPatterns {
                    waiting: vec!["waiting for input".to_string()],
                    completed: vec!["Task complete".to_string()],
                    error: vec!["ERROR".to_string()],
                },
            },
            command,
        )
    }

    fn config_with_agent(agent: AgentDef) -> Config {
        let mut config = Config {
            ui: UiConfig {
                default_agent: agent.key.clone(),
                agent_tab_position: "left".to_string(),
            },
            worktrees: WorktreesConfig {
                root: ".flightdeck/worktrees".to_string(),
            },
            ..Config::default()
        };
        config.agents.insert(agent.key.clone(), agent);
        config
    }

    const REPO: &str = "/repo";
    const STATE: &str = "/repo/.flightdeck/state.json";

    fn fresh_state(config: Config) -> AppState {
        AppState::new(config, default_state("main"), REPO, STATE)
    }

    fn services<'a>(
        git: &'a FakeGit,
        fs: &'a FakeFs,
        pty: &'a FakePty,
        clock: &'a FakeClock,
    ) -> Services<'a> {
        // Local-mode tests don't exercise the container runtime; leak a default
        // fake so the (overwhelming) majority of call sites need no container
        // argument. Container-specific tests build `Services` explicitly with a
        // configured `FakeContainerRuntime`.
        let container: &'static FakeContainerRuntime =
            Box::leak(Box::new(FakeContainerRuntime::new()));
        Services {
            git,
            fs,
            pty,
            clock,
            container,
        }
    }

    // --- §26: create tab (happy path) -------------------------------------

    #[test]
    fn create_tab_happy_path() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();

        let mut app = fresh_state(config);
        let effect = app
            .dispatch(
                Command::NewAgentTab {
                    name: "Fix Login Bug".to_string(),
                    agent_key: None,
                },
                &services(&git, &fs, &pty, &clock),
            )
            .unwrap();

        assert!(matches!(effect, Effect::Message(_)));
        // Branch created from base.
        assert_eq!(
            git.created_branches(),
            vec![("flightdeck/fix-login-bug".to_string(), "main".to_string())]
        );
        // Worktree added.
        assert_eq!(git.added_worktrees().len(), 1);
        // Primary spawned.
        assert_eq!(pty.spawns().len(), 1);
        // Tab focused.
        assert_eq!(app.selected_tab, Some(0));
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].meta.branch, "flightdeck/fix-login-bug");
        assert_eq!(app.tabs[0].meta.slug, "fix-login-bug");
        assert!(!app.tabs[0].meta.recovered);
        // State persisted with the tab.
        let saved = fs
            .file_contents(Path::new(STATE))
            .expect("state.json written");
        assert!(saved.contains("flightdeck/fix-login-bug"));
        assert!(saved.contains("\"version\""));
    }

    // --- §26 / §16: validation ORDER — no git mutation on missing command -

    #[test]
    fn new_tab_validation_precedes_git_mutation() {
        // Agent command does not resolve anywhere.
        let agent = AgentDef {
            key: "ghost".to_string(),
            display_name: "Ghost".to_string(),
            command: "__definitely_missing_cmd_xyz__".to_string(),
            args: vec![],
            status_patterns: StatusPatterns::default(),
        };
        let config = config_with_agent(agent);

        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();

        let mut app = fresh_state(config);
        let err = app
            .dispatch(
                Command::NewAgentTab {
                    name: "Whatever".to_string(),
                    agent_key: None,
                },
                &services(&git, &fs, &pty, &clock),
            )
            .unwrap_err();

        assert!(matches!(err, FlightDeckError::AgentMissing(_)));
        // No git mutation, no spawn.
        assert!(git.created_branches().is_empty());
        assert!(git.added_worktrees().is_empty());
        assert!(pty.spawns().is_empty());
        assert!(app.tabs.is_empty());
    }

    // --- §16/§17: async new-tab flow (begin → materialize → finalize) -----

    #[test]
    fn begin_new_agent_tab_reserves_placeholder_without_spawning() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();

        let mut app = fresh_state(config);
        let job = app
            .begin_new_agent_tab("Fix Login Bug", None, &services(&git, &fs, &pty, &clock))
            .unwrap();

        // A placeholder tab exists and is focused, in the Creating phase.
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.selected_tab, Some(0));
        assert_eq!(app.tabs[0].phase, TabPhase::Creating);
        assert_eq!(job.tab_id, app.tabs[0].meta.id);
        assert!(job.needs_create);
        // Nothing slow happened yet: no worktree add, no spawn, no persist.
        assert!(git.added_worktrees().is_empty());
        assert!(pty.spawns().is_empty());
        assert!(fs.file_contents(Path::new(STATE)).is_none());

        // The worker runs the slow step, then finalize spawns + flips to Ready.
        materialize_worktree(&git, &job).unwrap();
        let effect = app
            .finalize_new_tab(&job.tab_id, &services(&git, &fs, &pty, &clock))
            .unwrap();

        assert!(matches!(effect, Effect::Message(_)));
        assert_eq!(app.tabs[0].phase, TabPhase::Ready);
        assert_eq!(git.added_worktrees().len(), 1);
        assert_eq!(pty.spawns().len(), 1);
        assert!(app.tabs[0].status_file.is_some());
        // Persisted only on finalize.
        assert!(fs
            .file_contents(Path::new(STATE))
            .expect("state.json written")
            .contains("flightdeck/fix-login-bug"));
    }

    #[test]
    fn fail_new_tab_removes_the_placeholder() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();

        let mut app = fresh_state(config);
        let job = app
            .begin_new_agent_tab("Some Task", None, &services(&git, &fs, &pty, &clock))
            .unwrap();
        assert_eq!(app.tabs.len(), 1);

        app.fail_new_tab(&job.tab_id);
        assert!(app.tabs.is_empty());
        assert_eq!(app.selected_tab, None);
    }

    #[test]
    fn destructive_commands_refused_while_tab_is_creating() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();

        let mut app = fresh_state(config);
        app.begin_new_agent_tab("WIP", None, &services(&git, &fs, &pty, &clock))
            .unwrap();
        assert_eq!(app.tabs[0].phase, TabPhase::Creating);

        let effect = app
            .dispatch(
                Command::PushBranch { confirm: None },
                &services(&git, &fs, &pty, &clock),
            )
            .unwrap();
        assert!(matches!(effect, Effect::Refused(_)));
        // The refusal short-circuits before any push attempt.
        assert!(git.pushes().is_empty());
    }

    #[test]
    fn copy_env_file_prefers_env_local() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new()
            .with_file("/repo/.env", "BASE=1\n")
            .with_file("/repo/.env.local", "LOCAL=1\n");
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Fix Login Bug".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        let effect = app.dispatch(Command::CopyEnvFile, &svc).unwrap();

        assert_eq!(
            effect,
            Effect::Message("Copied .env.local to worktree.".to_string())
        );
        assert_eq!(
            fs.file_contents(Path::new(
                "/repo/.flightdeck/worktrees/fix-login-bug/.env.local"
            )),
            Some("LOCAL=1\n".to_string())
        );
        assert_eq!(
            fs.file_contents(Path::new("/repo/.flightdeck/worktrees/fix-login-bug/.env")),
            None
        );
    }

    #[test]
    fn copy_env_file_falls_back_to_env_and_refuses_when_missing() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new().with_file("/repo/.env", "BASE=1\n");
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config.clone());
        app.dispatch(
            Command::NewAgentTab {
                name: "Fix Login Bug".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        let effect = app.dispatch(Command::CopyEnvFile, &svc).unwrap();

        assert_eq!(
            effect,
            Effect::Message("Copied .env to worktree.".to_string())
        );
        assert_eq!(
            fs.file_contents(Path::new("/repo/.flightdeck/worktrees/fix-login-bug/.env")),
            Some("BASE=1\n".to_string())
        );

        let empty_fs = FakeFs::new();
        let empty_pty = FakePty::new();
        empty_pty.queue_session();
        let empty_svc = services(&git, &empty_fs, &empty_pty, &clock);
        let mut empty_app = fresh_state(config);
        empty_app
            .dispatch(
                Command::NewAgentTab {
                    name: "No Env".to_string(),
                    agent_key: None,
                },
                &empty_svc,
            )
            .unwrap();

        let effect = empty_app
            .dispatch(Command::CopyEnvFile, &empty_svc)
            .unwrap();

        assert_eq!(
            effect,
            Effect::Refused("No .env.local or .env found in base folder.".to_string())
        );
    }

    #[test]
    fn new_agent_tab_symlinks_env_files_from_base() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new()
            .with_file("/repo/.env", "BASE=1\n")
            .with_file("/repo/.env.local", "LOCAL=1\n");
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Fix Login Bug".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        // Both `.env` and `.env.local` are symlinked into the worktree pointing
        // back at the base folder — no copy is made.
        assert_eq!(
            fs.symlink_target(Path::new("/repo/.flightdeck/worktrees/fix-login-bug/.env")),
            Some(PathBuf::from("/repo/.env"))
        );
        assert_eq!(
            fs.symlink_target(Path::new(
                "/repo/.flightdeck/worktrees/fix-login-bug/.env.local"
            )),
            Some(PathBuf::from("/repo/.env.local"))
        );
        assert_eq!(
            fs.file_contents(Path::new("/repo/.flightdeck/worktrees/fix-login-bug/.env")),
            None
        );
    }

    #[test]
    fn new_agent_tab_without_env_files_creates_no_symlink() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        // Absent `.env`/`.env.local` must not fail session creation.
        app.dispatch(
            Command::NewAgentTab {
                name: "No Env".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        assert_eq!(
            fs.symlink_target(Path::new("/repo/.flightdeck/worktrees/no-env/.env")),
            None
        );
        assert_eq!(
            fs.symlink_target(Path::new("/repo/.flightdeck/worktrees/no-env/.env.local")),
            None
        );
    }

    // --- §26 / §11: attach to existing branch, surfaced ------------------

    #[test]
    fn attach_to_existing_branch_is_surfaced() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let git = FakeGit::new()
            .with_root(REPO)
            .with_branches(["main", "flightdeck/existing-task"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();

        let mut app = fresh_state(config);
        let effect = app
            .dispatch(
                Command::NewAgentTab {
                    name: "Existing Task".to_string(),
                    agent_key: None,
                },
                &services(&git, &fs, &pty, &clock),
            )
            .unwrap();

        assert_eq!(
            effect,
            Effect::AttachedExisting {
                branch: "flightdeck/existing-task".to_string()
            }
        );
        assert!(app.tabs[0].meta.attached_existing_branch);
        // No branch created (it already existed); worktree still materialized.
        assert!(git.created_branches().is_empty());
        assert_eq!(git.added_worktrees().len(), 1);
    }

    // --- §26: rename tab -------------------------------------------------

    #[test]
    fn rename_tab_changes_name_only() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Original Name".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        let branch_before = app.tabs[0].meta.branch.clone();
        let slug_before = app.tabs[0].meta.slug.clone();
        let wt_before = app.tabs[0].meta.worktree_path_relative.clone();

        app.dispatch(
            Command::RenameAgentTab {
                new_name: "Totally Different".to_string(),
            },
            &svc,
        )
        .unwrap();

        assert_eq!(app.tabs[0].meta.name, "Totally Different");
        assert_eq!(app.tabs[0].meta.branch, branch_before);
        assert_eq!(app.tabs[0].meta.slug, slug_before);
        assert_eq!(app.tabs[0].meta.worktree_path_relative, wt_before);
    }

    // --- §26: switch tab / maintains selected tab and child --------------

    #[test]
    fn switch_tab_and_maintains_selection() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        for _ in 0..4 {
            pty.queue_session(); // 2 primaries + child spawns
        }
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Tab One".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        app.dispatch(
            Command::NewAgentTab {
                name: "Tab Two".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        assert_eq!(app.selected_tab, Some(1));

        // Open a child terminal in tab two and select it.
        app.dispatch(Command::NewChildTerminal, &svc).unwrap();
        assert_eq!(app.tabs[1].session.selected_child(), Some(0));

        // Switch to previous tab, then back; tab two's child selection persists.
        app.dispatch(Command::SwitchAgentTab(Selector::Prev), &svc)
            .unwrap();
        assert_eq!(app.selected_tab, Some(0));
        app.dispatch(Command::SwitchAgentTab(Selector::Next), &svc)
            .unwrap();
        assert_eq!(app.selected_tab, Some(1));
        assert_eq!(app.tabs[1].session.selected_child(), Some(0));

        // Index switch.
        app.dispatch(Command::SwitchAgentTab(Selector::Index(0)), &svc)
            .unwrap();
        assert_eq!(app.selected_tab, Some(0));
    }

    // --- §26 / §25: close tab option set; default is Ctrl-C primary ------

    #[test]
    fn close_tab_returns_option_set_with_safe_default() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        let effect = app
            .dispatch(Command::CloseAgentTab { action: None }, &svc)
            .unwrap();
        match effect {
            Effect::CloseTabOptions(opts) => {
                assert_eq!(opts.default_action(), CloseAction::CtrlCPrimary);
                // No auto-escalation: force is not the default.
                assert_ne!(opts.actions[0], CloseAction::ForceTerminate);
            }
            other => panic!("expected CloseTabOptions, got {other:?}"),
        }
        // Tab still present (not closed without an action).
        assert_eq!(app.tabs.len(), 1);

        // Default action just signals Ctrl-C; does not remove the tab.
        app.dispatch(
            Command::CloseAgentTab {
                action: Some(CloseAction::CtrlCPrimary),
            },
            &svc,
        )
        .unwrap();
        assert_eq!(app.tabs.len(), 1);
    }

    #[test]
    fn close_tab_force_terminate_removes_tab() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let handle = pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        app.dispatch(
            Command::CloseAgentTab {
                action: Some(CloseAction::ForceTerminate),
            },
            &svc,
        )
        .unwrap();
        assert!(handle.terminated());
        assert!(app.tabs.is_empty());
        assert_eq!(app.selected_tab, None);
    }

    // --- §26 / §24: manual status applied AND process state represented --

    #[test]
    fn manual_status_override_keeps_process_state_visible() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        app.dispatch(Command::SetManualStatus(Some(ManualStatus::Blocked)), &svc)
            .unwrap();

        let ds = app.tabs[0].display_status(0);
        assert_eq!(ds.manual, Some(ManualStatus::Blocked));
        // Process state still represented (the primary is running).
        assert_eq!(ds.process, ProcessState::Running);
        assert_eq!(app.tabs[0].meta.manual_status.as_deref(), Some("blocked"));

        // Clear it; process state remains.
        app.dispatch(Command::SetManualStatus(None), &svc).unwrap();
        let ds = app.tabs[0].display_status(0);
        assert_eq!(ds.manual, None);
        assert_eq!(ds.process, ProcessState::Running);
    }

    // --- §26: status ingestion via classify_output ----------------------

    #[test]
    fn ingest_output_updates_interpreted_status() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        let id = app.tabs[0].id();

        app.ingest_output(&id, b"the agent is waiting for input now", 0);
        assert_eq!(
            app.tabs[0].display_status(0).interpreted,
            InterpretedStatus::WaitingForInput
        );

        // Non-matching output leaves it unchanged.
        app.ingest_output(&id, b"some normal log line", 0);
        assert_eq!(
            app.tabs[0].display_status(0).interpreted,
            InterpretedStatus::WaitingForInput
        );
    }

    // --- §24: activity-based idle/working detection ---------------------

    #[test]
    fn activity_drives_working_then_idle() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        let id = app.tabs[0].id();

        // Output at t=1000 → working when observed shortly after.
        app.ingest_output(&id, b"...streaming tokens...", 1_000);
        assert_eq!(
            app.tabs[0].display_status(1_100).interpreted,
            InterpretedStatus::Working
        );

        // No further output; once past the idle threshold → idle.
        let later = 1_000 + crate::agents::status::IDLE_AFTER_MS + 1;
        assert_eq!(
            app.tabs[0].display_status(later).interpreted,
            InterpretedStatus::Idle
        );
    }

    // --- §24: OS notifications on task-finish edge ----------------------

    /// Build an app with one running tab and return it plus the tab id.
    fn app_with_running_tab(
        config: Config,
    ) -> (AppState, FakeGit, FakeFs, FakePty, FakeClock, TabId) {
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &services(&git, &fs, &pty, &clock),
        )
        .unwrap();
        let id = app.tabs[0].id();
        (app, git, fs, pty, clock, id)
    }

    const IDLE: u64 = crate::agents::status::IDLE_AFTER_MS;

    /// A config with one real agent and OS notifications enabled (off by
    /// default, so tests that expect alerts must opt in).
    fn config_notify_on(dir: &TempDir) -> Config {
        let (agent, _cmd) = make_real_agent(dir, "opencode");
        let mut config = config_with_agent(agent);
        config.notifications.enabled = true;
        config
    }

    #[test]
    fn notifies_when_agent_finishes_turn() {
        let dir = TempDir::new().unwrap();
        let (mut app, _git, _fs, _pty, _clock, id) = app_with_running_tab(config_notify_on(&dir));

        // Agent streams output (working) → a tick arms the tab, no alert yet.
        app.ingest_output(&id, b"...working...", 1_000);
        assert!(app.take_finish_notifications(1_100).is_empty());

        // Falls silent past the idle threshold → one "finished" notification.
        let idle_at = 1_000 + IDLE + 1;
        let notes = app.take_finish_notifications(idle_at);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "Task");
        // Body names the agent (the test agent's display name is "opencode").
        assert!(notes[0].body.contains("opencode"), "got: {}", notes[0].body);
        assert!(notes[0].body.contains("finished"), "got: {}", notes[0].body);

        // Staying idle must not re-notify.
        assert!(app.take_finish_notifications(idle_at + 1).is_empty());

        // Resuming work re-arms; finishing again fires a fresh notification.
        app.ingest_output(&id, b"more work", idle_at + 100);
        assert!(app.take_finish_notifications(idle_at + 150).is_empty());
        let idle_again = idle_at + 100 + IDLE + 1;
        assert_eq!(app.take_finish_notifications(idle_again).len(), 1);
    }

    #[test]
    fn notifies_when_agent_waits_for_input() {
        let dir = TempDir::new().unwrap();
        let (mut app, _git, _fs, _pty, _clock, id) = app_with_running_tab(config_notify_on(&dir));

        app.ingest_output(&id, b"...working...", 1_000);
        assert!(app.take_finish_notifications(1_100).is_empty());

        // A waiting pattern (the opencode test agent matches "waiting for input").
        app.ingest_output(&id, b"the agent is waiting for input now", 1_200);
        let notes = app.take_finish_notifications(1_250);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].body.contains("waiting"), "got: {}", notes[0].body);
    }

    #[test]
    fn notifies_when_agent_fails() {
        let dir = TempDir::new().unwrap();
        let (mut app, _git, _fs, _pty, _clock, id) = app_with_running_tab(config_notify_on(&dir));

        app.ingest_output(&id, b"...working...", 1_000);
        assert!(app.take_finish_notifications(1_100).is_empty());

        // The opencode test agent matches "ERROR" as an error pattern.
        app.ingest_output(&id, b"ERROR: boom", 1_200);
        let notes = app.take_finish_notifications(1_250);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].body.contains("failed"), "got: {}", notes[0].body);
    }

    #[test]
    fn startup_grace_suppresses_finish_notifications() {
        let dir = TempDir::new().unwrap();
        let (mut app, _git, _fs, _pty, _clock, id) = app_with_running_tab(config_notify_on(&dir));

        app.begin_notification_grace(1_000); // suppress until 1_000 + grace
        app.ingest_output(&id, b"...working...", 1_100);
        assert!(app.take_finish_notifications(1_200).is_empty()); // arms the tab

        // Settles to idle while still inside the grace window → suppressed.
        let idle_at = 1_100 + IDLE + 1;
        assert!(idle_at < 1_000 + NOTIFY_STARTUP_GRACE_MS, "test setup");
        assert!(app.take_finish_notifications(idle_at).is_empty());
    }

    #[test]
    fn disabled_config_emits_no_notifications() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let mut config = config_with_agent(agent);
        config.notifications.enabled = false;
        let (mut app, _git, _fs, _pty, _clock, id) = app_with_running_tab(config);

        app.ingest_output(&id, b"...working...", 1_000);
        let _ = app.take_finish_notifications(1_100);
        let idle_at = 1_000 + IDLE + 1;
        assert!(app.take_finish_notifications(idle_at).is_empty());
    }

    #[test]
    fn notifications_are_off_by_default() {
        // Opt-in: the master switch is off until the user enables it, but the
        // per-category toggles stay on so enabling is a single flip.
        let cfg = NotificationsConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.on_finish && cfg.on_waiting && cfg.on_failed);
        assert!(!Config::default().notifications.enabled);
    }

    #[test]
    fn per_category_toggle_suppresses_only_that_category() {
        let dir = TempDir::new().unwrap();
        let mut config = config_notify_on(&dir);
        config.notifications.on_finish = false; // mute turn-finished only
        let (mut app, _git, _fs, _pty, _clock, id) = app_with_running_tab(config);

        app.ingest_output(&id, b"...working...", 1_000);
        let _ = app.take_finish_notifications(1_100);
        let idle_at = 1_000 + IDLE + 1;
        // on_finish disabled → no "finished" alert.
        assert!(app.take_finish_notifications(idle_at).is_empty());

        // But a waiting signal (on_waiting still enabled) still fires.
        app.ingest_output(&id, b"...working again...", idle_at + 100);
        let _ = app.take_finish_notifications(idle_at + 150);
        app.ingest_output(&id, b"waiting for input", idle_at + 200);
        assert_eq!(app.take_finish_notifications(idle_at + 250).len(), 1);
    }

    // --- §24: opt-in precise status via status file ---------------------

    #[test]
    fn poll_status_file_applies_hook_signal() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        // The agent's hook writes "waiting" to the tab's status file.
        let path = app.tabs[0].status_file.clone().expect("status file path");
        fs.write(&path, "waiting\n").unwrap();

        app.poll_status_files(&svc, 5_000);
        // A waiting hook signal is sticky while the agent stays quiet.
        assert_eq!(
            app.tabs[0].display_status(9_999).interpreted,
            InterpretedStatus::WaitingForInput
        );

        // New output after the signal supersedes it → back to working.
        let id = app.tabs[0].id();
        app.ingest_output(&id, b"resuming", 6_000);
        assert_eq!(
            app.tabs[0].display_status(6_050).interpreted,
            InterpretedStatus::Working
        );
    }

    // --- §26 / §23: mode transitions ------------------------------------

    #[test]
    fn mode_transitions() {
        let config = Config::default();
        let mut app = fresh_state(config);
        // Default is App mode.
        assert_eq!(app.mode(), InputMode::App);
        app.focus_terminal();
        assert_eq!(app.mode(), InputMode::Terminal);
        app.focus_app();
        assert_eq!(app.mode(), InputMode::App);
        app.toggle_mode();
        assert_eq!(app.mode(), InputMode::Terminal);
        app.toggle_mode();
        assert_eq!(app.mode(), InputMode::App);
    }

    #[test]
    fn toggle_split_view_command_flips_flag() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let mut app = fresh_state(config);

        assert!(!app.split_view, "split view defaults off");
        let effect = app
            .dispatch(Command::ToggleSplitView, &services(&git, &fs, &pty, &clock))
            .unwrap();
        assert!(app.split_view);
        assert!(matches!(effect, Effect::Message(_)));
        // Toggling again turns it back off — no tab required (it is a global view
        // command, not a tab action).
        app.dispatch(Command::ToggleSplitView, &services(&git, &fs, &pty, &clock))
            .unwrap();
        assert!(!app.split_view);
    }

    // --- §26 / §13: dirty base → local merge disabled -------------------

    #[test]
    fn dirty_base_disables_local_merge() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        // Make the base worktree (repo root) dirty.
        git.set_dirty_at(Path::new(REPO), true);

        let effect = app
            .dispatch(Command::FinishLocalMerge { confirm: true }, &svc)
            .unwrap();
        match effect {
            Effect::Warning(msg) => assert!(msg.contains("Local merge is disabled")),
            other => panic!("expected Warning, got {other:?}"),
        }
        // Persistent warning recorded.
        assert!(app
            .warnings
            .iter()
            .any(|w| w.contains("local merge disabled")));
        // No merge performed.
        assert!(git.merges().is_empty());
    }

    #[test]
    fn local_merge_allowed_when_preconditions_pass() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let handle = pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        // Stop the primary so it is not "running".
        handle.set_state(ProcessState::Stopped);
        // Both worktrees clean (default). Confirm performs the merge and cleanup.
        let effect = app
            .dispatch(Command::FinishLocalMerge { confirm: true }, &svc)
            .unwrap();
        assert!(matches!(effect, Effect::Message(_)));
        assert_eq!(git.merges().len(), 1);
        // On success the worktree is removed and the tab dropped.
        assert_eq!(git.removed_worktrees().len(), 1);
        assert!(app.tabs.is_empty());
    }

    #[test]
    fn local_merge_works_while_primary_running() {
        // A running primary agent no longer blocks the merge — "Finish" stops it
        // and removes the worktree.
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let handle = pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        // Primary is running — must NOT block the merge.
        handle.set_state(ProcessState::Running);
        let effect = app
            .dispatch(Command::FinishLocalMerge { confirm: true }, &svc)
            .unwrap();
        assert!(matches!(effect, Effect::Message(_)));
        assert_eq!(git.merges().len(), 1);
        assert_eq!(git.removed_worktrees().len(), 1);
        assert!(app.tabs.is_empty());
    }

    #[test]
    fn finish_merge_without_confirm_asks_for_confirmation() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let handle = pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        handle.set_state(ProcessState::Running);
        // First dispatch (confirm: false) asks rather than merging.
        let effect = app
            .dispatch(Command::FinishLocalMerge { confirm: false }, &svc)
            .unwrap();
        match effect {
            Effect::MergeConfirm {
                primary_running, ..
            } => assert!(primary_running),
            other => panic!("expected MergeConfirm, got {other:?}"),
        }
        // Nothing was merged or removed yet.
        assert!(git.merges().is_empty());
        assert!(git.removed_worktrees().is_empty());
        assert_eq!(app.tabs.len(), 1);
    }

    // --- §5 carve-out: Rebase Worktree ----------------------------------

    #[test]
    fn rebase_without_confirm_asks_for_confirmation() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let handle = pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        // The rebase preconditions require the agent worktree to actually have
        // the agent branch checked out (FakeGit's current_branch is a single
        // global, so set it once the created branch name is known).
        git.set_current_branch(app.tabs[0].meta.branch.clone());
        handle.set_state(ProcessState::Running);

        // First dispatch (confirm: false) asks rather than rebasing.
        let effect = app
            .dispatch(Command::RebaseWorktree { confirm: false }, &svc)
            .unwrap();
        match effect {
            Effect::RebaseConfirm {
                base_branch,
                primary_running,
                ..
            } => {
                assert_eq!(base_branch, "main");
                assert!(primary_running, "running agent reported in the prompt");
            }
            other => panic!("expected RebaseConfirm, got {other:?}"),
        }
        // Nothing was rebased yet.
        assert!(git.rebases().is_empty());
    }

    #[test]
    fn rebase_on_confirm_rebases_onto_base_and_advances_stored_sha() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        // The rebase preconditions require the agent worktree to actually have
        // the agent branch checked out (FakeGit's current_branch is a single
        // global, so set it once the created branch name is known).
        git.set_current_branch(app.tabs[0].meta.branch.clone());
        // The base branch has advanced since tab creation.
        git.set_rev("main", "sha-main-new");

        let effect = app
            .dispatch(Command::RebaseWorktree { confirm: true }, &svc)
            .unwrap();
        match effect {
            Effect::Message(m) => assert!(m.contains("Rebased"), "got: {m}"),
            other => panic!("expected Message, got {other:?}"),
        }
        // Rebased the agent worktree onto the base branch.
        let wt = to_absolute(
            Path::new(REPO),
            Path::new(&app.tabs[0].meta.worktree_path_relative),
        );
        assert_eq!(git.rebases(), vec![("main".to_string(), wt)]);
        // Stored base SHA advanced to the current base tip so §12 drift resets.
        assert_eq!(app.tabs[0].meta.base_commit_sha, "sha-main-new");
    }

    #[test]
    fn rebase_aborts_and_refuses_on_conflict_without_advancing_sha() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        // The rebase preconditions require the agent worktree to actually have
        // the agent branch checked out (FakeGit's current_branch is a single
        // global, so set it once the created branch name is known).
        git.set_current_branch(app.tabs[0].meta.branch.clone());
        let sha_before = app.tabs[0].meta.base_commit_sha.clone();
        git.set_rebase_outcome(crate::contracts::RebaseOutcome {
            rebased: false,
            conflicted: true,
            message: "CONFLICT in file.rs".to_string(),
        });

        let effect = app
            .dispatch(Command::RebaseWorktree { confirm: true }, &svc)
            .unwrap();
        match effect {
            Effect::Refused(m) => assert!(m.contains("aborted"), "got: {m}"),
            other => panic!("expected Refused, got {other:?}"),
        }
        // The stored base SHA must NOT advance on a conflict.
        assert_eq!(app.tabs[0].meta.base_commit_sha, sha_before);
    }

    #[test]
    fn rebase_refused_when_worktree_dirty() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        let wt = to_absolute(
            Path::new(REPO),
            Path::new(&app.tabs[0].meta.worktree_path_relative),
        );
        git.set_dirty_at(&wt, true);

        let effect = app
            .dispatch(Command::RebaseWorktree { confirm: false }, &svc)
            .unwrap();
        assert!(matches!(effect, Effect::Refused(_)));
        assert!(git.rebases().is_empty());
    }

    // --- §5.2: pull base -------------------------------------------------

    #[test]
    fn pull_base_pulls_base_folder() {
        let config = Config::default();
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        let effect = app.dispatch(Command::PullBase, &svc).unwrap();
        match effect {
            Effect::Message(m) => assert!(m.contains("Pulled main"), "got: {m}"),
            other => panic!("expected Message, got {other:?}"),
        }
        // The pull ran in the base folder (the repo root), never a worktree.
        assert_eq!(git.pull_bases(), vec![PathBuf::from(REPO)]);
    }

    #[test]
    fn pull_base_refused_when_base_folder_dirty() {
        let config = Config::default();
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        git.set_dirty_at(Path::new(REPO), true);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        let effect = app.dispatch(Command::PullBase, &svc).unwrap();
        assert!(matches!(effect, Effect::Refused(_)));
        // A dirty base folder must not be pulled over.
        assert!(git.pull_bases().is_empty());
    }

    #[test]
    fn pull_base_refused_when_not_on_base_branch() {
        let config = Config::default();
        let git = FakeGit::new()
            .with_root(REPO)
            .with_branches(["main"])
            .with_current_branch("flightdeck/x");
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        let effect = app.dispatch(Command::PullBase, &svc).unwrap();
        assert!(matches!(effect, Effect::Refused(_)));
        assert!(git.pull_bases().is_empty());
    }

    #[test]
    fn pull_base_aborts_and_refuses_on_conflict() {
        let config = Config::default();
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        git.set_pull_base_outcome(crate::contracts::RebaseOutcome {
            rebased: false,
            conflicted: true,
            message: "CONFLICT in file.rs".to_string(),
        });
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        let effect = app.dispatch(Command::PullBase, &svc).unwrap();
        match effect {
            Effect::Refused(m) => assert!(m.contains("CONFLICT"), "got: {m}"),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    // --- §26: push warning + confirm ------------------------------------

    #[test]
    fn push_warns_on_dirty_then_pushes_on_confirm() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        let wt = to_absolute(
            Path::new(REPO),
            Path::new(&app.tabs[0].meta.worktree_path_relative),
        );
        git.set_dirty_at(&wt, true);

        // First push: warning, no push.
        let effect = app
            .dispatch(Command::PushBranch { confirm: None }, &svc)
            .unwrap();
        assert_eq!(effect, Effect::PushWarning(PushPlan::UncommittedChanges));
        assert!(git.pushes().is_empty());

        // Confirm push committed only.
        git.set_remote("origin", "git@github.com:owner/repo.git");
        let effect = app
            .dispatch(
                Command::PushBranch {
                    confirm: Some(PushConfirm::PushCommitted),
                },
                &svc,
            )
            .unwrap();
        match effect {
            Effect::PrUrl(url) => assert!(url.contains("/compare/main...flightdeck/task")),
            other => panic!("expected PrUrl, got {other:?}"),
        }
        assert_eq!(git.pushes().len(), 1);
    }

    // --- §21: git status overlay surfaces the PR compare URL after push ---

    #[test]
    fn git_status_overlay_includes_pr_url_when_pushed_to_github() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        let branch = app.tabs[0].meta.branch.clone();
        // The branch has been pushed (has an upstream) and origin is GitHub.
        git.set_upstream(&branch, Some(format!("origin/{branch}")));
        git.set_remote("origin", "git@github.com:owner/repo.git");

        let effect = app.dispatch(Command::ShowGitStatus, &svc).unwrap();
        match effect {
            Effect::GitStatus { pr_url, .. } => {
                let url = pr_url.expect("expected a PR compare URL after push");
                assert!(
                    url.contains("/compare/main...flightdeck/task"),
                    "got: {url}"
                );
            }
            other => panic!("expected GitStatus, got {other:?}"),
        }
    }

    #[test]
    fn git_status_overlay_has_no_pr_url_before_push() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        // A GitHub remote exists, but the branch has no upstream (not pushed).
        git.set_remote("origin", "git@github.com:owner/repo.git");

        let effect = app.dispatch(Command::ShowGitStatus, &svc).unwrap();
        match effect {
            Effect::GitStatus { pr_url, .. } => {
                assert!(pr_url.is_none(), "no PR URL should show before a push");
            }
            other => panic!("expected GitStatus, got {other:?}"),
        }
    }

    // --- §5/§15: abandon warns (not refuses) on dirty worktree ----------

    #[test]
    fn abandon_warns_on_dirty_worktree_then_force_removes_on_confirm() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        let wt = to_absolute(
            Path::new(REPO),
            Path::new(&app.tabs[0].meta.worktree_path_relative),
        );
        git.set_dirty_at(&wt, true);

        // Unconfirmed abandon on a dirty worktree asks for confirmation and
        // leaves the tab in place.
        let effect = app
            .dispatch(Command::AbandonWorktree { confirm: false }, &svc)
            .unwrap();
        assert!(matches!(effect, Effect::AbandonWarning { dirty: true }));
        assert_eq!(app.tabs.len(), 1); // not removed

        // Confirming force-removes the dirty worktree and drops the tab.
        let effect = app
            .dispatch(Command::AbandonWorktree { confirm: true }, &svc)
            .unwrap();
        assert!(matches!(effect, Effect::Message(_)));
        assert_eq!(app.tabs.len(), 0);
        assert!(git.removed_worktrees().iter().any(|p| p == &wt));
    }

    #[test]
    fn abandon_confirms_even_a_clean_worktree() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        // A clean worktree is NOT removed on the first (unconfirmed) abandon: it
        // still asks first, just without the dirty-changes warning.
        let effect = app
            .dispatch(Command::AbandonWorktree { confirm: false }, &svc)
            .unwrap();
        assert!(matches!(effect, Effect::AbandonWarning { dirty: false }));
        assert_eq!(app.tabs.len(), 1); // not removed
        assert!(git.removed_worktrees().is_empty());

        // Confirming removes it and drops the tab.
        let effect = app
            .dispatch(Command::AbandonWorktree { confirm: true }, &svc)
            .unwrap();
        assert!(matches!(effect, Effect::Message(_)));
        assert_eq!(app.tabs.len(), 0);
    }

    // --- §26: save/load round trip --------------------------------------

    #[test]
    fn save_load_round_trip() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent.clone());
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config.clone());
        app.dispatch(
            Command::NewAgentTab {
                name: "Persisted Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        // Round-trip through to_project_state → persistence load.
        let saved = app.to_project_state(0);
        assert_eq!(saved.tabs.len(), 1);
        assert_eq!(saved.base_branch, "main");

        // Reload from the on-disk state.json into a fresh AppState (no spawn).
        let reloaded =
            crate::persistence::project_state::load_state(&fs, Path::new(STATE)).unwrap();
        let app2 = AppState::new(config, reloaded, REPO, STATE);
        assert_eq!(app2.tabs.len(), 1);
        assert_eq!(app2.tabs[0].meta.branch, "flightdeck/persisted-task");
        // Recovered/runtime tabs do NOT auto-spawn.
        assert_eq!(
            app2.tabs[0].session.primary_state(),
            ProcessState::NotStarted
        );
        assert_eq!(app2.selected_tab, Some(0));
    }

    // --- recovered tabs are not auto-started (SPECS §10) ----------------

    #[test]
    fn constructor_does_not_spawn_for_recovered_tabs() {
        let mut state = default_state("main");
        state.tabs.push(TabState {
            id: "recovered-x".to_string(),
            name: "x".to_string(),
            slug: "x".to_string(),
            agent: "opencode".to_string(),
            branch: "flightdeck/x".to_string(),
            worktree_path_relative: ".flightdeck/worktrees/x".to_string(),
            base_branch: "main".to_string(),
            base_commit_sha: "abc".to_string(),
            created_at: String::new(),
            attached_existing_branch: false,
            recovered: true,
            last_known_status: "session lost".to_string(),
            manual_status: None,
            containerized: false,
            container_image: None,
        });
        let app = AppState::new(Config::default(), state, REPO, STATE);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(
            app.tabs[0].session.primary_state(),
            ProcessState::NotStarted
        );
        assert_eq!(app.selected_tab, Some(0));
    }

    // --- Alt-Left/Right cycles the agent + shells ring (SPECS §19, §22) ---

    #[test]
    fn switch_child_cycles_through_agent_and_shells() {
        let dir = TempDir::new().unwrap();
        let (agent, _) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session(); // primary
        pty.queue_session(); // shell 1
        pty.queue_session(); // shell 2
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "t".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();
        app.dispatch(Command::NewChildTerminal, &svc).unwrap();
        app.dispatch(Command::NewChildTerminal, &svc).unwrap();
        // After creating two children, the second (index 1) is selected.
        assert_eq!(app.tabs[0].session.selected_child(), Some(1));

        // Next from last child wraps to the primary ("agent") tab.
        app.dispatch(Command::SwitchChildTerminal(Selector::Next), &svc)
            .unwrap();
        assert_eq!(app.tabs[0].session.selected_child(), None);
        // Next again → first child shell.
        app.dispatch(Command::SwitchChildTerminal(Selector::Next), &svc)
            .unwrap();
        assert_eq!(app.tabs[0].session.selected_child(), Some(0));
        // Prev → back to the primary ("agent") tab.
        app.dispatch(Command::SwitchChildTerminal(Selector::Prev), &svc)
            .unwrap();
        assert_eq!(app.tabs[0].session.selected_child(), None);
        // Prev from the primary wraps to the last child shell.
        app.dispatch(Command::SwitchChildTerminal(Selector::Prev), &svc)
            .unwrap();
        assert_eq!(app.tabs[0].session.selected_child(), Some(1));
    }

    // --- Resuming a session starts the agents (user-requested behaviour) --

    fn recovered_tab(slug: &str) -> TabState {
        TabState {
            id: slug.to_string(),
            name: slug.to_string(),
            slug: slug.to_string(),
            agent: "opencode".to_string(),
            branch: format!("flightdeck/{slug}"),
            worktree_path_relative: format!(".flightdeck/worktrees/{slug}"),
            base_branch: "main".to_string(),
            base_commit_sha: "abc".to_string(),
            created_at: String::new(),
            attached_existing_branch: false,
            recovered: true,
            last_known_status: "session lost".to_string(),
            manual_status: None,
            containerized: false,
            container_image: None,
        }
    }

    #[test]
    fn resume_agents_starts_primary_when_worktree_exists() {
        let dir = TempDir::new().unwrap();
        let (agent, _) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let mut ps = default_state("main");
        ps.tabs.push(recovered_tab("r"));
        let mut app = AppState::new(config, ps, REPO, STATE);
        app.set_pty_size(PtySize { rows: 24, cols: 80 });

        let git = FakeGit::new().with_root(REPO);
        let fs = FakeFs::new().with_dir("/repo/.flightdeck/worktrees/r");
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();

        let started = app.resume_agents(&services(&git, &fs, &pty, &clock));
        assert_eq!(started, 1);
        assert_eq!(pty.spawns().len(), 1);
        assert_eq!(app.tabs[0].session.primary_state(), ProcessState::Running);
        assert!(!app.tabs[0].meta.recovered);
    }

    #[test]
    fn resume_agents_skips_tab_with_missing_worktree() {
        let dir = TempDir::new().unwrap();
        let (agent, _) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);

        let mut ps = default_state("main");
        ps.tabs.push(recovered_tab("gone"));
        let mut app = AppState::new(config, ps, REPO, STATE);
        app.set_pty_size(PtySize { rows: 24, cols: 80 });

        let git = FakeGit::new().with_root(REPO);
        let fs = FakeFs::new(); // worktree dir does NOT exist
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();

        let started = app.resume_agents(&services(&git, &fs, &pty, &clock));
        assert_eq!(started, 0);
        assert_eq!(pty.spawns().len(), 0);
        assert_eq!(
            app.tabs[0].session.primary_state(),
            ProcessState::NotStarted
        );
    }

    // --- Container execution (SPECS §31) ---------------------------------

    fn plain_agent(key: &str) -> AgentDef {
        AgentDef {
            key: key.to_string(),
            display_name: key.to_string(),
            command: key.to_string(),
            args: vec![],
            status_patterns: StatusPatterns::default(),
        }
    }

    fn exec_config() -> Config {
        let mut c = config_with_agent(plain_agent("opencode"));
        c.containers.enabled = true;
        c
    }

    fn project_image() -> String {
        image::project_image_tag(&repo_hash(std::path::Path::new(REPO)), "opencode")
    }

    fn new_tab_cmd() -> Command {
        Command::NewAgentTab {
            name: "add auth".to_string(),
            agent_key: Some("opencode".to_string()),
        }
    }

    #[test]
    fn create_tab_launches_podman_run_when_containerized() {
        let image = project_image();
        let mut app = fresh_state(exec_config());
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let container = FakeContainerRuntime::new().with_image(image.clone());
        let svc = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
        };

        app.dispatch(new_tab_cmd(), &svc).unwrap();

        // The container is started detached (run -d …), carrying the image +
        // guardrails; the PTY then attaches to it.
        let started = container.started();
        assert_eq!(started.len(), 1, "container started detached once");
        let run = &started[0];
        assert_eq!(run[0], "run");
        assert!(run.contains(&"-d".to_string()), "detached");
        assert!(run.contains(&"--cap-drop".to_string()));
        assert!(run.contains(&image), "image present in run args");

        let spawns = pty.spawns();
        assert_eq!(spawns.len(), 1, "primary PTY spawned once");
        assert_eq!(spawns[0].0, "podman");
        let name = container_name(&app.tabs[0].meta.id);
        assert_eq!(
            spawns[0].1,
            vec!["attach".to_string(), name],
            "PTY attaches"
        );
        // The tab records that it is containerized + the image used.
        assert!(app.tabs[0].meta.containerized);
        assert_eq!(app.tabs[0].meta.container_image, Some(image));
    }

    #[test]
    fn create_tab_refused_when_image_missing() {
        let mut app = fresh_state(exec_config());
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let container = FakeContainerRuntime::new(); // no image registered
        let svc = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
        };

        let err = app.dispatch(new_tab_cmd(), &svc).unwrap_err();
        assert!(err.to_string().contains("image"), "got: {err}");
        // Placeholder tab is removed on failure (no dead state).
        assert!(app.tabs.is_empty());
        assert_eq!(pty.spawns().len(), 0, "nothing spawned");
    }

    #[test]
    fn refused_when_runtime_unavailable() {
        let mut app = fresh_state(exec_config());
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let container = FakeContainerRuntime::new();
        container.set_unavailable("podman machine not running");
        let svc = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
        };

        // Validation fails before any git mutation (no worktree added).
        assert!(app.dispatch(new_tab_cmd(), &svc).is_err());
        assert!(git.added_worktrees().is_empty());
        assert!(app.tabs.is_empty());
    }

    #[test]
    fn child_shell_execs_into_container() {
        let image = project_image();
        let mut app = fresh_state(exec_config());
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let container = FakeContainerRuntime::new().with_image(image);
        let svc = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
        };
        app.dispatch(new_tab_cmd(), &svc).unwrap();
        let name = container_name(&app.tabs[0].meta.id);

        app.dispatch(Command::NewChildTerminal, &svc).unwrap();
        let last = pty.spawns().pop().unwrap();
        assert_eq!(last.0, "podman");
        assert_eq!(last.1[0], "exec");
        assert_eq!(last.1[1], "-it");
        assert_eq!(last.1[2], name, "execs into the agent's container");
    }

    #[test]
    fn new_agent_child_spawns_agent_and_close_is_type_checked() {
        use crate::terminal::session::TerminalKind;
        let dir = TempDir::new().unwrap();
        let (agent, cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session(); // primary agent
        pty.queue_session(); // + agent child
        pty.queue_session(); // shell child
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);
        let mut app = fresh_state(config);

        app.dispatch(
            Command::NewAgentTab {
                name: "task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        // "+ agent" spawns the agent command as an agent-kind child.
        app.dispatch(Command::NewAgentTerminal { agent_key: None }, &svc)
            .unwrap();
        assert_eq!(app.tabs[0].session.child_count(), 1);
        assert_eq!(
            app.tabs[0].session.child(0).unwrap().kind,
            TerminalKind::Agent
        );
        assert_eq!(
            pty.spawns().pop().unwrap().0,
            cmd,
            "agent child runs the agent command, not a shell"
        );

        // "Close Agent" closes the selected agent child.
        app.dispatch(Command::CloseAgentTerminal, &svc).unwrap();
        assert_eq!(app.tabs[0].session.child_count(), 0);

        // A shell child is refused by CloseAgentTerminal (type-checked).
        app.dispatch(Command::NewChildTerminal, &svc).unwrap();
        let effect = app.dispatch(Command::CloseAgentTerminal, &svc).unwrap();
        assert!(matches!(effect, Effect::Refused(_)));
        assert_eq!(
            app.tabs[0].session.child_count(),
            1,
            "the shell must survive a Close Agent"
        );
    }

    #[test]
    fn force_close_removes_the_container() {
        let image = project_image();
        let mut app = fresh_state(exec_config());
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let container = FakeContainerRuntime::new().with_image(image);
        let svc = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
        };
        app.dispatch(new_tab_cmd(), &svc).unwrap();
        let name = container_name(&app.tabs[0].meta.id);

        app.dispatch(
            Command::CloseAgentTab {
                action: Some(CloseAction::ForceTerminate),
            },
            &svc,
        )
        .unwrap();
        assert!(app.tabs.is_empty());
        assert!(
            container
                .removed_containers()
                .iter()
                .any(|(n, f)| n == &name && *f),
            "container force-removed on close"
        );
    }

    #[test]
    fn resume_reattaches_to_running_container() {
        let mut config = exec_config();
        config.ui.default_agent = "opencode".to_string();
        let mut ps = default_state("main");
        let mut tab = recovered_tab("r");
        tab.containerized = true;
        ps.tabs.push(tab);
        let mut app = AppState::new(config, ps, REPO, STATE);

        let git = FakeGit::new().with_root(REPO);
        let fs = FakeFs::new().with_dir("/repo/.flightdeck/worktrees/r");
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let name = container_name("r");
        let container = FakeContainerRuntime::new();
        container.set_container_state(&name, ContainerState::Running);
        let svc = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
        };

        let started = app.resume_agents(&svc);
        assert_eq!(started, 1, "reattaches to the running container");
        let spawns = pty.spawns();
        assert_eq!(spawns[0].0, "podman");
        assert_eq!(spawns[0].1, vec!["attach".to_string(), name]);
    }

    #[test]
    fn resume_starts_fresh_when_container_gone() {
        let config = exec_config();
        let mut ps = default_state("main");
        let mut tab = recovered_tab("r");
        tab.containerized = true;
        ps.tabs.push(tab);
        let mut app = AppState::new(config, ps, REPO, STATE);

        let git = FakeGit::new().with_root(REPO);
        let fs = FakeFs::new().with_dir("/repo/.flightdeck/worktrees/r");
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let image = image::project_image_tag(&repo_hash(std::path::Path::new(REPO)), "opencode");
        // Container is gone (default Absent) but the image exists → resume starts
        // a fresh detached container rather than leaving the tab agent-less.
        let container = FakeContainerRuntime::new().with_image(image);
        let svc = Services {
            git: &git,
            fs: &fs,
            pty: &pty,
            clock: &clock,
            container: &container,
        };

        let started = app.resume_agents(&svc);
        assert_eq!(started, 1, "starts a fresh container when none is running");
        assert_eq!(
            container.started().len(),
            1,
            "a detached container was started"
        );
        let name = container_name("r");
        assert_eq!(
            pty.spawns()[0].1,
            vec!["attach".to_string(), name],
            "PTY attaches"
        );
    }
}
