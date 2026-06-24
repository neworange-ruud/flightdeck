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
    Clock, Config, FileSystem, FlightDeckError, GitExecutor, InterpretedStatus, ManualStatus,
    ProcessState, ProjectState, PtyBackend, PtySize, Result, TabId, TabState, STATE_VERSION,
};
use crate::fs::paths::{to_absolute, to_relative, worktree_path};
use crate::git::branch::{branch_name, decide_branch, slugify, BranchDecision};
use crate::git::remote::{github_pr_url, plan_push, push_branch, PushPlan};
use crate::git::status::{
    check_merge_preconditions, collect_status, merge_back, MergeDecision, MergeRequest,
};
use crate::git::worktree::{create_worktree, plan_worktree, remove_worktree_if_safe, WorktreePlan};
use crate::persistence::project_state::save_state;
use crate::terminal::session::Session;
use crate::terminal::shell::shell_launch;

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
}

/// A live Agent Tab: persisted metadata + a live terminal session + the cached
/// interpreted status from output pattern matching (SPECS §3, §24).
///
/// The `meta` field is exactly what is serialized to `state.json`; the rest is
/// runtime-only and not persisted.
pub struct RuntimeTab {
    /// The persisted tab metadata (serialized verbatim to `state.json`).
    pub meta: TabState,
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
}

impl RuntimeTab {
    /// Build a runtime tab from persisted metadata, with an empty session and no
    /// cached interpreted status. Does **not** spawn anything (SPECS §10).
    fn from_meta(meta: TabState) -> Self {
        RuntimeTab {
            meta,
            session: Session::new(),
            interpreted: None,
            interpreted_at_ms: None,
            last_activity_ms: None,
            status_file: None,
            status_file_seen: None,
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

    // -----------------------------------------------------------------------
    // Tab/selection helpers
    // -----------------------------------------------------------------------

    /// The selected runtime tab, if any.
    pub fn selected(&self) -> Option<&RuntimeTab> {
        self.selected_tab.and_then(|i| self.tabs.get(i))
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
        match cmd {
            Command::NewAgentTab { name, agent_key } => {
                self.cmd_new_agent_tab(&name, agent_key.as_deref(), services)
            }
            Command::RenameAgentTab { new_name } => self.cmd_rename(&new_name, services),
            Command::CloseAgentTab { action } => self.cmd_close_tab(action, services),
            Command::PushBranch { confirm } => self.cmd_push(confirm, services),
            Command::FinishLocalMerge { confirm } => self.cmd_finish_merge(confirm, services),
            Command::AbandonWorktree { confirm } => self.cmd_abandon(confirm, services),
            Command::NewChildTerminal | Command::OpenShell => self.cmd_new_child(services),
            Command::CloseChildTerminal => self.cmd_close_child(),
            Command::SwitchAgentTab(sel) => self.cmd_switch_tab(sel),
            Command::SwitchChildTerminal(sel) => self.cmd_switch_child(sel),
            Command::SetManualStatus(status) => self.cmd_set_manual_status(status, services),
            Command::RestartAgent => self.cmd_restart_agent(services),
            Command::ShowGitStatus => self.cmd_show_git_status(services),
            Command::ShowHelp => Ok(Effect::ShowHelp),
            Command::Quit => Ok(Effect::Quit),
        }
    }

    /// NEW-TAB FLOW (SPECS §4, §16, §17). Validation precedes ALL git mutation
    /// (SPECS §16): if the agent command is missing we fail before creating any
    /// branch/worktree/process.
    fn cmd_new_agent_tab(
        &mut self,
        name: &str,
        agent_key: Option<&str>,
        services: &Services,
    ) -> Result<Effect> {
        // (a) look up the agent in the registry.
        let key = agent_key
            .map(|k| k.to_string())
            .unwrap_or_else(|| self.registry.default_key.clone());
        let agent = self
            .registry
            .get(&key)
            .cloned()
            .ok_or_else(|| FlightDeckError::Config(format!("unknown agent '{key}'")))?;

        // (b) validate the command BEFORE any git mutation (SPECS §16).
        validate_agent(&agent)?;

        // (c) slug + branch name with the configured prefix.
        let slug = slugify(name);
        if slug.is_empty() {
            return Err(FlightDeckError::Config(
                "tab name produced an empty slug".to_string(),
            ));
        }
        let prefix = self.config.git.branch_prefix.clone();
        let branch = branch_name(&prefix, &slug);

        // (d) decide create vs attach (surface attach, never silent, SPECS §11).
        let decision = decide_branch(services.git, &branch)?;
        let attached = matches!(decision, BranchDecision::AttachExisting);

        // (e) plan the worktree.
        let worktrees_root_rel = self.config.worktrees.root.clone();
        let target = worktree_path(&self.repo_root, &worktrees_root_rel, &slug);
        let worktrees_root_abs = to_absolute(&self.repo_root, Path::new(&worktrees_root_rel));
        let plan = plan_worktree(services.git, &branch, &target, &worktrees_root_abs)?;

        // (f) materialize the worktree as planned.
        let worktree_abs = match plan {
            WorktreePlan::Create => {
                // Create the branch from base only when it does not already exist.
                let create_branch = matches!(decision, BranchDecision::Create);
                create_worktree(
                    services.git,
                    &branch,
                    &self.base_branch,
                    &target,
                    create_branch,
                )?;
                target.clone()
            }
            WorktreePlan::ReuseManaged { path } => path,
            WorktreePlan::RefuseCheckedOutElsewhere { path } => {
                return Err(FlightDeckError::Refused(format!(
                    "branch '{branch}' is already checked out at {}",
                    path.display()
                )));
            }
        };

        // (g) spawn the primary terminal (NO initial prompt, SPECS §17).
        let launch = build_launch(&agent, &worktree_abs);
        let status_file = agent_status_file(&worktree_abs);
        let mut session = Session::new();
        session.spawn_primary(
            services.pty,
            &launch.command,
            &launch.args,
            &launch.cwd,
            self.pty_size,
        )?;

        // (h) record the new TabState.
        let base_commit_sha = services.git.rev_parse(&self.base_branch)?;
        let worktree_rel = to_relative(&self.repo_root, &worktree_abs)
            .unwrap_or_else(|_| worktree_abs.clone())
            .to_string_lossy()
            .to_string();
        let created_at = services.clock.now_iso8601();
        let id = format!("{slug}-{created_at}");
        let meta = TabState {
            id,
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
        };
        self.tabs.push(RuntimeTab {
            meta,
            session,
            interpreted: Some(InterpretedStatus::Starting),
            interpreted_at_ms: None,
            last_activity_ms: None,
            status_file: Some(status_file),
            status_file_seen: None,
        });
        // Focus the new tab.
        self.selected_tab = Some(self.tabs.len() - 1);

        // (i) persist.
        self.persist(services)?;

        if attached {
            Ok(Effect::AttachedExisting { branch })
        } else {
            Ok(Effect::Message(format!("Created Agent Tab on {branch}")))
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

        // Remove the tab from runtime state.
        self.tabs.remove(idx);
        self.fix_selection_after_removal(idx);
        self.persist(services)?;
        Ok(Effect::Message("Closed Agent Tab.".to_string()))
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
        let Some(tab) = self.selected() else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let agent_branch = tab.meta.branch.clone();
        let base_branch = tab.meta.base_branch.clone();
        let agent_worktree =
            to_absolute(&self.repo_root, Path::new(&tab.meta.worktree_path_relative));
        let base_worktree = self.repo_root.clone();
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
            primary_running,
            user_confirmed: confirm,
        };

        match check_merge_preconditions(services.git, &req)? {
            MergeDecision::Refused(reason) => return Ok(Effect::Refused(reason)),
            MergeDecision::Allowed => {}
        }

        let outcome = merge_back(services.git, &req)?;
        if outcome.conflicted {
            return Ok(Effect::Refused(outcome.message));
        }
        if outcome.merged {
            Ok(Effect::Message(format!(
                "Merged {agent_branch} into {base_branch}."
            )))
        } else {
            Ok(Effect::Refused(outcome.message))
        }
    }

    /// Abandon Worktree (SPECS §5/§15). A clean worktree is removed immediately.
    /// A dirty worktree returns [`Effect::AbandonWarning`] so the UI can confirm;
    /// once the user confirms (`confirm` true) it is force-removed regardless of
    /// uncommitted changes. The tab is dropped after a successful removal.
    fn cmd_abandon(&mut self, confirm: bool, services: &Services) -> Result<Effect> {
        let Some(idx) = self.selected_tab else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let worktree = to_absolute(
            &self.repo_root,
            Path::new(&self.tabs[idx].meta.worktree_path_relative),
        );

        // Without confirmation, a dirty worktree must be confirmed before we
        // discard its uncommitted changes.
        if !confirm && services.git.is_dirty(&worktree)? {
            return Ok(Effect::AbandonWarning);
        }

        match remove_worktree_if_safe(services.git, services.fs, &worktree, confirm) {
            Ok(()) => {
                // Tear down any live session, then drop the tab.
                let _ = self.tabs[idx].session.terminate_all();
                self.tabs.remove(idx);
                self.fix_selection_after_removal(idx);
                self.persist(services)?;
                Ok(Effect::Message("Abandoned worktree.".to_string()))
            }
            Err(FlightDeckError::Refused(reason)) => Ok(Effect::Refused(reason)),
            Err(e) => Err(e),
        }
    }

    /// New child shell terminal in the selected tab's worktree (SPECS §19).
    fn cmd_new_child(&mut self, services: &Services) -> Result<Effect> {
        let size = self.pty_size;
        let repo_root = self.repo_root.clone();
        let Some(tab) = self.selected_mut() else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        let cwd = to_absolute(&repo_root, Path::new(&tab.meta.worktree_path_relative));
        let (cmd, args) = shell_launch();
        let idx = tab
            .session
            .spawn_child(services.pty, &cmd, &args, &cwd, size)?;
        Ok(Effect::Message(format!("Opened child terminal #{idx}.")))
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
        Ok(Effect::Message("Closed child terminal.".to_string()))
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

    /// Spawn (or re-spawn) the primary agent for tab `idx`. Re-validates the
    /// agent's command first (SPECS §16) and marks the tab no longer recovered.
    fn start_primary_for(&mut self, idx: usize, services: &Services) -> Result<()> {
        let size = self.pty_size;
        let repo_root = self.repo_root.clone();
        let agent_key = self.tabs[idx].meta.agent.clone();
        let agent = self
            .registry
            .get(&agent_key)
            .cloned()
            .ok_or_else(|| FlightDeckError::Config(format!("unknown agent '{agent_key}'")))?;
        validate_agent(&agent)?;

        let cwd = to_absolute(
            &repo_root,
            Path::new(&self.tabs[idx].meta.worktree_path_relative),
        );
        let launch = build_launch(&agent, &cwd);
        let status_file = agent_status_file(&cwd);
        let tab = &mut self.tabs[idx];
        tab.session.spawn_primary(
            services.pty,
            &launch.command,
            &launch.args,
            &launch.cwd,
            size,
        )?;
        tab.interpreted = Some(InterpretedStatus::Starting);
        tab.interpreted_at_ms = None;
        tab.last_activity_ms = None;
        tab.status_file = Some(status_file);
        tab.status_file_seen = None;
        tab.meta.recovered = false;
        Ok(())
    }

    /// Restart the primary agent of the selected (recovered/stopped) tab
    /// (SPECS §10, §23). Re-validates the agent before spawning.
    fn cmd_restart_agent(&mut self, services: &Services) -> Result<Effect> {
        let Some(idx) = self.selected_tab else {
            return Err(FlightDeckError::Other("no tab selected".to_string()));
        };
        self.start_primary_for(idx, services)?;
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
            if self.start_primary_for(idx, services).is_ok() {
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
        let worktree = to_absolute(&self.repo_root, Path::new(&tab.meta.worktree_path_relative));
        let status = collect_status(
            services.git,
            &tab.meta.branch,
            &tab.meta.base_branch,
            &tab.meta.base_commit_sha,
            &worktree,
        )?;
        Ok(Effect::GitStatus(Box::new(status)))
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
    use crate::contracts::{AgentDef, StatusPatterns, UiConfig, WorktreesConfig};
    use crate::persistence::project_state::default_state;
    use crate::testing::{FakeClock, FakeFs, FakeGit, FakePty};
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    // --- Test scaffolding -------------------------------------------------

    /// An agent whose command is a real executable file (absolute path) so
    /// `validate_agent` passes via the `contains('/')` branch.
    fn make_real_agent(dir: &TempDir, key: &str) -> (AgentDef, String) {
        let path = dir.path().join(key);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
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
        Services {
            git,
            fs,
            pty,
            clock,
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
        // Stop the primary so it is not "running" (precondition).
        handle.set_state(ProcessState::Stopped);
        // Both worktrees clean (default).
        let effect = app
            .dispatch(Command::FinishLocalMerge { confirm: true }, &svc)
            .unwrap();
        assert!(matches!(effect, Effect::Message(_)));
        assert_eq!(git.merges().len(), 1);
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
        assert!(matches!(effect, Effect::AbandonWarning));
        assert_eq!(app.tabs.len(), 1); // not removed

        // Confirming force-removes the dirty worktree and drops the tab.
        let effect = app
            .dispatch(Command::AbandonWorktree { confirm: true }, &svc)
            .unwrap();
        assert!(matches!(effect, Effect::Message(_)));
        assert_eq!(app.tabs.len(), 0);
        assert!(git.removed_worktrees().iter().any(|p| p == &wt));
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
}
