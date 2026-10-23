//! [`AppHost`]'s worker mode: the host `flightdeck worker run` drives (SPECS
//! §33, `specs/WORKER_PRD.md` §4.4).
//!
//! A worker host is an ordinary [`AppHost`] with extras: it may hold no
//! project at all (a fresh worker), never starts FlightDeck Remote, serves the
//! web protocol on Tailscale only, keeps a login shell for the attached
//! window, and confines project browsing, opening and cloning to `$HOME`. A
//! child of [`crate::host`] so it can reach the host's private state.
//!
//! The run loop calls only [`AppHost::open_worker`],
//! [`AppHost::resume_all_projects`], [`AppHost::start_worker`],
//! [`AppHost::tick`], the worker API below, [`AppHost::persist`],
//! [`AppHost::terminate_sessions`] and [`AppHost::stop_worker`]: nothing else
//! is guaranteed safe on an empty workspace.
//!
//! What a worker adds to a turn:
//!
//! * [`AppHost::pump`] drains the shell into its `worker:shell` stream,
//!   forwards the agents' notifications ([`WorkerNotifier`]) as
//!   `Delta::Notification`, turns what the TUI would have shown in its status
//!   line into [`crate::web::protocol::HostMessage`]s ([`messages`]), reports clones, samples
//!   [`health`] every [`health::HEALTH_INTERVAL_MS`] and logs listener
//!   changes.
//! * [`AppHost::publish`] routes the window's worker frames (`ResizeTerminal`,
//!   the picker's [`WorkerOp`]s, attach kinds) and publishes a
//!   [`WorkerHostState`] with the rest of the host state.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::contracts::{Config, Notification, NotificationSound, PtySize, Result, WorkerConfig};
use crate::host::AppHost;
use crate::persistence::workspace::{load_workspace, WorkspaceUi};
use crate::web::credentials::{BrowserToken, BOOTSTRAP_CODE_TTL_MS};
use crate::web::protocol::{
    Ack, AckOutcome, Delta, DirEntryView, DirListing, HostKind, MessageLevel,
    NotificationSoundView, NotificationView, ProjectId, ProjectScan, ScannedRepo, ServerMsg,
    TerminalId, ViewerId, WorkerOp, WorkerShellView, WorkerStatusView, WORKER_SHELL_TERMINAL_ID,
};
use crate::web::server::{
    ConnectionInfo, ListenPlan, ListenStatus, ServerOptions, ShutdownNotice, WebInbound,
    WebOutbound, WebServerHandle, WorkerHostState,
};
use crate::worker::control::{DeviceReport, DeviceState, PairingCode};
use crate::worker::paths::WorkerPaths;
use crate::worker::status::{
    AgentReport, AttachedReport, ListenReport, ProjectReport, WorkerRuntimeReport,
};
use crate::{
    apply_update_notice, locate_web_terminal, open_project, resume_active_project_agents,
    web_host_state_now, Env, WebFlavor, WebSurface, Workspace,
};

pub mod clone;
pub mod health;
pub mod messages;
pub mod notify;
pub mod projects;
pub mod shell;

#[cfg(test)]
mod tests;

pub use notify::WorkerNotifier;

use clone::{CloneEvent, CloneJobs, CloneRunner};
use health::{AgentCounts, HealthProbe, HealthSampler};
use messages::{level_of, HostMessageLog};
use projects::Confinement;
use shell::WorkerShell;

/// The answer to every command that needs a project, on a worker that has
/// none yet (design A9).
pub const NO_PROJECT_REFUSAL: &str = "Open a project first: this worker has no projects yet.";

/// How often a worker whose server could not start at all tries again.
/// (A Tailscale plan that has no address yet is a *running* server that is
/// not listening, and retries on its own poll.)
pub const LISTEN_RETRY_MS: u64 = 30_000;

/// The least time between two progress frames of one clone: at most four a
/// second reach the window, however chatty git is.
pub const CLONE_PROGRESS_MIN_MS: u64 = 250;

/// The commands a worker refuses whatever its window says (design §2.4), and
/// why. The server refuses them first; the host re-checks the same table as
/// defence in depth, so a refusal holds even if one slipped past.
pub use crate::web::commands::worker_refusal;

/// The one message a worker posts when a newer FlightDeck is out: it has no
/// status bar to badge, and the update is the user's to run.
pub fn update_message(latest: &str) -> String {
    format!(
        "FlightDeck {latest} is available: run `flightdeck update`, then \
         `flightdeck worker restart`, in the worker shell."
    )
}

/// Everything a worker host needs beyond the services in [`Env`].
pub struct WorkerOptions {
    /// Where the worker workspace file and the device tokens live.
    pub paths: WorkerPaths,
    /// The confinement root for browsing/opening/cloning, and the worker
    /// shell's working directory.
    pub home: PathBuf,
    pub config: WorkerConfig,
    /// The effective global config: what an empty workspace uses where a
    /// project's config would otherwise answer.
    pub global_config: Config,
    /// Where the web server listens: Tailscale addresses in production,
    /// fixed loopback addresses in tests.
    pub listen: ListenPlan,
    pub health: Box<dyn health::HealthProbe + Send>,
    pub cloner: Arc<dyn clone::CloneRunner>,
}

/// A worker op's answer: `Ok((ack detail, answer frame))` when it applied,
/// `Err(refusal)` when it was refused.
type OpOutcome = std::result::Result<(Option<String>, Option<ServerMsg>), OpRefusal>;

/// Why a worker op was refused, and the frame that still answers it (a scan
/// that could not run still tells the picker why, in its own frame).
struct OpRefusal {
    reason: String,
    frame: Option<Box<ServerMsg>>,
}

impl OpRefusal {
    fn new(reason: impl Into<String>) -> Self {
        OpRefusal {
            reason: reason.into(),
            frame: None,
        }
    }
}

/// One clone in flight, as the window learns of it.
struct CloneMeta {
    url: String,
    /// The host clock at the last progress frame sent, for
    /// [`CLONE_PROGRESS_MIN_MS`].
    last_progress_ms: Option<u64>,
}

/// A worker host's state beyond an ordinary [`AppHost`]'s.
pub(crate) struct WorkerExt {
    /// The confinement root as configured (the shell's working directory).
    home: PathBuf,
    config: WorkerConfig,
    /// `[worker] projects_root`, resolved against the canonical home.
    projects_root: PathBuf,
    listen: ListenPlan,
    /// Whether the once-a-day update check is on (`[update] check`, global).
    update_check: bool,
    probe: Box<dyn HealthProbe + Send>,
    cloner: Arc<dyn CloneRunner>,
    pub(super) shell: WorkerShell,
    messages: HostMessageLog,
    /// The [`WorkerNotifier`]'s queue: every agent notification the pump fired.
    notifications: Arc<Mutex<Vec<Notification>>>,
    health: HealthSampler,
    last_health: Option<crate::web::protocol::Health>,
    clones: CloneJobs,
    clone_meta: HashMap<u64, CloneMeta>,
    confinement: Confinement,
    /// The pane size the window last reported: what a project opened later,
    /// and every agent spawned later, starts at.
    last_window_size: Option<PtySize>,
    /// The host clock when the worker host was opened.
    started_ms: u64,
    /// Whether [`AppHost::start_worker`] ran.
    started: bool,
    /// Whether the coming stop is a restart ([`AppHost::set_shutdown_intent`]).
    shutdown_restarting: bool,
    log_lines: Vec<String>,
    last_listen: Option<ListenStatus>,
    /// Why the server is not running, when it could not start at all.
    not_listening_reason: Option<String>,
    /// When to try starting the server again, after it could not start.
    listen_retry_at_ms: Option<u64>,
    /// Every frame the worker emitted of its own accord (not an answer to a
    /// frame), server or not: what the tests read.
    #[cfg(test)]
    sent: Vec<WebOutbound>,
}

impl WorkerExt {
    fn log(&mut self, line: impl Into<String>) {
        self.log_lines.push(line.into());
    }

    /// Send a frame the worker produced of its own accord, when a server
    /// runs to carry it.
    fn emit(&mut self, handle: Option<&WebServerHandle>, frame: WebOutbound) {
        #[cfg(test)]
        self.sent.push(frame.clone());
        if let Some(handle) = handle {
            handle.send(frame);
        }
    }

    /// The grid a project starts at: the window's last pane, else the
    /// configured default.
    fn seed_size(&self) -> PtySize {
        self.last_window_size.unwrap_or(PtySize {
            rows: self.config.default_rows,
            cols: self.config.default_cols,
        })
    }

    /// Keep a host message, and hand it to the window when one is attached
    /// (it is then read). With none attached it waits, unread, for the next.
    fn post(
        &mut self,
        handle: Option<&WebServerHandle>,
        level: MessageLevel,
        text: String,
        at_ms: i64,
    ) {
        self.log(format!("message ({}): {text}", level_label(level)));
        let message = self.messages.push(level, text, at_ms);
        if let Some(handle) = handle.filter(|h| h.window_count() > 0) {
            self.emit(
                Some(handle),
                WebOutbound::Windows(ServerMsg::Delta(Delta::HostMessage(message))),
            );
            self.messages.mark_all_read();
        }
    }
}

fn level_label(level: MessageLevel) -> &'static str {
    match level {
        MessageLevel::Info => "info",
        MessageLevel::Warning => "warning",
        MessageLevel::Error => "error",
    }
}

fn shell_terminal_id() -> TerminalId {
    TerminalId::new(WORKER_SHELL_TERMINAL_ID)
}

fn notification_view(note: &Notification, at_ms: i64) -> NotificationView {
    NotificationView {
        at_ms,
        title: note.title.clone(),
        body: note.body.clone(),
        sound: match note.sound {
            NotificationSound::None => NotificationSoundView::None,
            NotificationSound::Completion => NotificationSoundView::Completion,
            NotificationSound::InputRequired => NotificationSoundView::InputRequired,
        },
    }
}

/// The device name a Desktop's label carries: `FlightDeck Desktop/1.2 (box)`
/// names `box` (design A11); any other label is shown as it is.
pub fn device_name(label: Option<&str>) -> String {
    let label = label.map(str::trim).unwrap_or("");
    if let Some(inner) = label
        .strip_suffix(')')
        .and_then(|rest| rest.rfind('(').map(|i| rest[i + 1..].trim()))
        .filter(|inner| !inner.is_empty())
    {
        return inner.to_string();
    }
    if label.is_empty() {
        "unnamed device".to_string()
    } else {
        label.to_string()
    }
}

fn device_report(record: &BrowserToken, connections: &[ConnectionInfo]) -> DeviceReport {
    use crate::web::protocol::ConnectionKind;
    let live: Vec<&ConnectionInfo> = connections
        .iter()
        .filter(|c| c.token == record.id)
        .collect();
    let state = if live.iter().any(|c| c.kind == ConnectionKind::Window) {
        DeviceState::Attached
    } else if live.iter().any(|c| c.kind == ConnectionKind::Background) {
        DeviceState::Background
    } else {
        DeviceState::Offline
    };
    DeviceReport {
        id: record.id.as_str().to_string(),
        label: device_name(record.label.as_deref()),
        address: live
            .first()
            .map(|c| c.address.to_string())
            .or_else(|| record.address.clone()),
        paired_at: record.created_unix_secs,
        last_seen: (record.last_seen_unix_secs > 0).then_some(record.last_seen_unix_secs),
        state,
        revoked: !record.is_active(),
    }
}

fn ambiguous(selector: &str, candidates: &[&DeviceReport]) -> String {
    let names: Vec<String> = candidates
        .iter()
        .map(|d| format!("{} ({})", d.label, d.id))
        .collect();
    format!(
        "'{selector}' matches several devices: {}. Name one by its id.",
        names.join(", ")
    )
}

impl<'a> AppHost<'a> {
    /// Open the worker workspace (it may be empty): reopen the projects
    /// remembered in `opts.paths.workspace`, seed every project's PTY size
    /// to `default_cols`×`default_rows`, never start FlightDeck Remote, and
    /// capture what the TUI would show in its status line as host messages.
    pub fn open_worker(
        env: Env<'a>,
        notifier: &'a WorkerNotifier,
        opts: WorkerOptions,
    ) -> Result<AppHost<'a>> {
        let mut workspace = Workspace {
            projects: Vec::new(),
            active: 0,
        };
        let mut workspace_ui = WorkspaceUi::default();
        let mut skipped = Vec::new();
        // A missing (or unreadable) file is a fresh worker: no projects yet.
        if let Ok(saved) = load_workspace(env.fs, &opts.paths.workspace) {
            workspace_ui = saved.ui.clone();
            for p in &saved.projects {
                let path = Path::new(p);
                if !env.fs.is_dir(path) {
                    skipped.push(format!("not reopening {p}: the folder is gone"));
                    continue;
                }
                match open_project(&env, path, None) {
                    Ok(proj) if !workspace.contains_root(proj.git.root()) => {
                        workspace.projects.push(proj)
                    }
                    Ok(_) => {}
                    Err(e) => skipped.push(format!("not reopening {p}: {e}")),
                }
            }
            workspace.set_active(saved.active);
        }
        let web_surface =
            WebSurface::with_credentials(&opts.global_config.web, opts.paths.credentials.clone());
        let ws_path = opts.paths.workspace.clone();
        let mut host = AppHost::worker_from_parts(env, notifier, workspace, web_surface, opts)?;
        host.ws_path = Some(ws_path);
        host.workspace_ui = workspace_ui;
        host.store_home = crate::app::state::user_home();
        if let Some(w) = host.worker.as_mut() {
            for line in skipped {
                w.log(line);
            }
        }
        Ok(host)
    }

    /// Make a worker host out of an already-built workspace and web surface:
    /// [`AppHost::open_worker`] minus reading the workspace file. No I/O
    /// beyond confining `opts.home`, no threads. The seam the tests build on.
    pub(crate) fn worker_from_parts(
        env: Env<'a>,
        notifier: &'a WorkerNotifier,
        workspace: Workspace,
        mut web_surface: WebSurface,
        opts: WorkerOptions,
    ) -> Result<AppHost<'a>> {
        let confinement = Confinement::new(env.fs, &opts.home)?;
        let size = PtySize {
            rows: opts.config.default_rows,
            cols: opts.config.default_cols,
        };
        web_surface.flavor = WebFlavor {
            kind: HostKind::Worker,
            fallback_config: Some(Box::new(opts.global_config.clone())),
            fallback_size: size,
        };
        let started_ms = env.clock.now_millis();
        let mut host = AppHost::from_parts(env, notifier, workspace, web_surface, false);
        host.kind = HostKind::Worker;
        // W6/W18: a worker never starts FlightDeck Remote, whatever `[remote]`
        // says. Without the relay channels `start` has nothing to start.
        host.remote_wiring = None;
        host.ui.capture_messages = true;
        for p in host.workspace.projects.iter_mut() {
            p.state.set_pty_size(size);
        }
        let projects_root = opts.config.projects_root_path(confinement.home());
        host.worker = Some(Box::new(WorkerExt {
            home: opts.home,
            projects_root,
            listen: opts.listen,
            update_check: opts.global_config.update.check,
            config: opts.config,
            probe: opts.health,
            cloner: opts.cloner,
            shell: WorkerShell::new(size),
            messages: HostMessageLog::new(),
            notifications: notifier.queue(),
            health: HealthSampler::new(started_ms),
            last_health: None,
            clones: CloneJobs::new(),
            clone_meta: HashMap::new(),
            confinement,
            last_window_size: None,
            started_ms,
            started: false,
            shutdown_restarting: false,
            log_lines: Vec::new(),
            last_listen: None,
            not_listening_reason: None,
            listen_retry_at_ms: None,
            #[cfg(test)]
            sent: Vec::new(),
        }));
        Ok(host)
    }

    /// Resume the agents of every open project (a worker has no "active"
    /// screen: a remote window browses each project on its own).
    pub fn resume_all_projects(&mut self) {
        let env = &self.env;
        for p in self.workspace.projects.iter_mut() {
            let services = env.services(&p.git);
            let _ = p.state.resume_agents(&services);
        }
    }

    /// Start the web server (Tailscale listeners, no browser assets) and the
    /// worker shell. A bind failure is not fatal: the worker keeps running,
    /// not listening, and retries. Idempotent.
    pub fn start_worker(&mut self) {
        let now0 = self.env.clock.now_millis();
        let update_check = match self.worker.as_mut() {
            Some(w) if !w.started => {
                w.started = true;
                w.update_check
            }
            _ => return,
        };

        // The startup grace (SPECS §24), as `start` gives a TUI: resumed agents
        // settling to idle must not ping the window in a burst.
        for p in self.workspace.projects.iter_mut() {
            p.state.begin_notification_grace(now0);
        }

        // The once-a-day update check (SPECS §30). Nothing is applied for
        // the worker: the user runs `flightdeck update` in the worker shell.
        if let Some(update_tx) = self.update_tx.take() {
            if let Some(latest) =
                crate::update::start_check(update_check, self.env.clock.now_unix_secs(), update_tx)
            {
                self.ui.message(update_message(&latest));
                apply_update_notice(&mut self.workspace, latest);
            }
        }

        self.start_listening(now0);

        let pty = self.env.pty;
        if let Some(w) = self.worker.as_mut() {
            let home = w.home.clone();
            match w.shell.start(pty, &home) {
                Ok(()) => w.log("worker shell started"),
                Err(e) => {
                    w.log(format!("worker shell did not start: {e}"));
                    self.ui
                        .message(format!("The worker shell did not start: {e}"));
                }
            }
        }
    }

    /// Start the server on the worker's listen plan. A failure is logged and
    /// retried after [`LISTEN_RETRY_MS`].
    fn start_listening(&mut self, now_ms: u64) {
        let Some(listen) = self.worker.as_ref().map(|w| w.listen.clone()) else {
            return;
        };
        let options = ServerOptions {
            host_kind: HostKind::Worker,
            listen,
            // Design A5: the protocol routes only, no browser SPA.
            serve_assets: false,
        };
        let mut initial = web_host_state_now(
            &self.workspace,
            &mut self.web_surface,
            &self.ui,
            self.env.clock,
            now_ms,
        );
        initial.worker = self.worker_host_state();
        let started = self.web_surface.start_with(options, initial);
        let Some(w) = self.worker.as_mut() else {
            return;
        };
        match started {
            Ok(()) => {
                w.not_listening_reason = None;
                w.listen_retry_at_ms = None;
            }
            Err(e) => {
                w.log(format!("not listening: {e}"));
                w.not_listening_reason = Some(e.to_string());
                w.listen_retry_at_ms = Some(now_ms.saturating_add(LISTEN_RETRY_MS));
            }
        }
    }

    /// A worker's share of [`AppHost::pump`]. Returns whether anything
    /// arrived that a window would see.
    pub(super) fn pump_worker(&mut self, now_ms: u64) -> bool {
        let mut changed = false;
        let at_ms = now_ms as i64;

        // --- The shell: its output into the `worker:shell` stream, its exit
        //     onto the stream's facts. ---
        {
            let Some(w) = self.worker.as_mut() else {
                return false;
            };
            let web_surface = &mut self.web_surface;
            let id = shell_terminal_id();
            changed |= w
                .shell
                .drain(&mut |bytes| web_surface.tee_terminal(&id, bytes));
            if let Some(code) = w.shell.poll_exit() {
                changed = true;
                web_surface.streams.closed(&id, code);
                w.log(match code {
                    Some(code) => format!("worker shell exited with status {code}"),
                    None => "worker shell exited".to_string(),
                });
            }
        }

        // --- The listener: retry a server that could not start, and log
        //     every change of where it listens. ---
        let retry = self.web_surface.handle.is_none()
            && self
                .worker
                .as_ref()
                .and_then(|w| w.listen_retry_at_ms)
                .is_some_and(|at| now_ms >= at);
        if retry {
            self.start_listening(now_ms);
        }
        let Some(w) = self.worker.as_mut() else {
            return changed;
        };
        let handle = self.web_surface.handle.as_ref();
        if let Some(status) = handle.map(|h| h.listen_status()) {
            if w.last_listen.as_ref() != Some(&status) {
                let line = if status.bound.is_empty() {
                    format!(
                        "not listening: {}",
                        status.reason.as_deref().unwrap_or("no address")
                    )
                } else {
                    let bound: Vec<String> = status.bound.iter().map(|a| a.to_string()).collect();
                    match &status.advertised {
                        Some(advertised) => {
                            format!("listening on {} ({advertised})", bound.join(", "))
                        }
                        None => format!("listening on {}", bound.join(", ")),
                    }
                };
                w.log(line);
                w.last_listen = Some(status);
            }
        }

        // --- Notifications: what an OS notification would have said, to
        //     every connected Desktop (design A7). ---
        let notes = match w.notifications.lock() {
            Ok(mut q) => std::mem::take(&mut *q),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };
        for note in notes {
            changed = true;
            w.emit(
                handle,
                WebOutbound::All(ServerMsg::Delta(Delta::Notification(notification_view(
                    &note, at_ms,
                )))),
            );
        }

        // --- Host messages: what the TUI would have shown in its status
        //     line. A worker has no screen to show it on, so the notification
        //     the TUI would have drawn is taken down at once. ---
        let captured: Vec<String> = self.ui.messages_out.drain(..).collect();
        if !captured.is_empty() {
            changed = true;
            self.ui.overlay = crate::tui::render::UiOverlay::None;
            for text in captured {
                w.post(handle, level_of(&text), text, at_ms);
            }
        }

        // --- Clones: progress (throttled) and results to the window. ---
        let mut finished = Vec::new();
        for event in w.clones.poll() {
            changed = true;
            match event {
                CloneEvent::Progress { id, line } => {
                    let Some(meta) = w.clone_meta.get_mut(&id) else {
                        continue;
                    };
                    let due = meta
                        .last_progress_ms
                        .is_none_or(|last| now_ms.saturating_sub(last) >= CLONE_PROGRESS_MIN_MS);
                    if !due {
                        continue;
                    }
                    meta.last_progress_ms = Some(now_ms);
                    let frame = WebOutbound::Windows(ServerMsg::CloneProgress(
                        crate::web::protocol::CloneProgress {
                            clone_id: id,
                            url: meta.url.clone(),
                            phase: line.phase,
                            percent: line.percent,
                        },
                    ));
                    w.emit(handle, frame);
                }
                CloneEvent::Done { id, result } => {
                    let url = w.clone_meta.remove(&id).map(|m| m.url).unwrap_or_default();
                    finished.push((id, url, result));
                }
            }
        }

        // --- Health, every `HEALTH_INTERVAL_MS`. ---
        if w.health.due(now_ms) {
            let counts = agent_counts(&self.workspace, now_ms);
            let sample = w.health.sample(
                &*w.probe,
                now_ms,
                w.confinement.home(),
                &w.projects_root,
                counts,
            );
            w.last_health = Some(sample);
        }

        for (id, url, result) in finished {
            self.finish_clone(id, url, result, at_ms);
        }
        changed
    }

    /// A clone ended: open what it cloned, or say why it failed — to the
    /// window as a [`crate::web::protocol::CloneResult`], and as a host
    /// message either way.
    fn finish_clone(
        &mut self,
        id: u64,
        url: String,
        result: std::result::Result<PathBuf, String>,
        at_ms: i64,
    ) {
        let (frame, level, text) = match result {
            Ok(path) => match self.open_worker_project(&path) {
                Ok(opened) => (
                    crate::web::protocol::CloneResult {
                        clone_id: id,
                        url: url.clone(),
                        ok: true,
                        path: Some(path.display().to_string()),
                        project_id: Some(opened.project_id),
                        error: None,
                    },
                    MessageLevel::Info,
                    format!("Cloned {url} into {}. {}", path.display(), opened.sentence),
                ),
                Err(reason) => (
                    crate::web::protocol::CloneResult {
                        clone_id: id,
                        url: url.clone(),
                        ok: true,
                        path: Some(path.display().to_string()),
                        project_id: None,
                        error: Some(reason.clone()),
                    },
                    MessageLevel::Error,
                    format!("Cloned {url} into {}, but {reason}", path.display()),
                ),
            },
            Err(reason) => (
                crate::web::protocol::CloneResult {
                    clone_id: id,
                    url: url.clone(),
                    ok: false,
                    path: None,
                    project_id: None,
                    error: Some(reason.clone()),
                },
                MessageLevel::Error,
                format!("Cloning {url} failed: {reason}"),
            ),
        };
        let handle = self.web_surface.handle.as_ref();
        if let Some(w) = self.worker.as_mut() {
            w.emit(handle, WebOutbound::Windows(ServerMsg::CloneResult(frame)));
            w.post(handle, level, text, at_ms);
        }
    }

    /// Open the project at `path` (already confined and canonical), or switch
    /// to it when it is open: what the TUI's folder browser does, without its
    /// notification and on an empty workspace too. The new project starts at
    /// the window's last pane size, and its recovered agents resume. The
    /// workspace file is written at once, so a crash does not forget it.
    pub(crate) fn open_worker_project(
        &mut self,
        path: &Path,
    ) -> std::result::Result<Opened, String> {
        let mut proj =
            open_project(&self.env, path, None).map_err(|e| format!("could not open it: {e}"))?;
        let root = proj.git.root().to_path_buf();
        let project_id = ProjectId::new(root.display().to_string());
        if let Some(i) = self
            .workspace
            .projects
            .iter()
            .position(|p| p.git.root() == root)
        {
            self.workspace.set_active(i);
            resume_active_project_agents(&mut self.workspace, &self.env);
            return Ok(Opened {
                project_id,
                sentence: "Project already open — switched to it.".to_string(),
            });
        }
        let size = self
            .worker
            .as_ref()
            .map(|w| w.seed_size())
            .unwrap_or_default();
        {
            let services = self.env.services(&proj.git);
            proj.state.set_pty_size(size);
            proj.state
                .begin_notification_grace(self.env.clock.now_millis());
            let _ = proj.state.resume_agents(&services);
        }
        let name = proj.name.clone();
        self.workspace.projects.push(proj);
        self.workspace.active = self.workspace.projects.len() - 1;
        let _ = self.persist();
        if let Some(w) = self.worker.as_mut() {
            w.log(format!("opened project {}", root.display()));
        }
        Ok(Opened {
            project_id,
            sentence: format!("Opened project '{name}'."),
        })
    }

    /// A worker's share of [`AppHost::route_inbound`]: `Some(frames)` when
    /// the event is the worker's to answer, `None` to let the ordinary
    /// terminal routing run (input, viewport, a window's attach replay).
    pub(super) fn route_worker_inbound(&mut self, event: &WebInbound) -> Option<Vec<WebOutbound>> {
        use crate::web::protocol::ConnectionKind;
        match event {
            // A background link is a Desktop's status-only line (§2.5): it
            // takes no terminal bytes, so it gets no replay either.
            WebInbound::ViewerAttached {
                kind: ConnectionKind::Background,
                label,
                ..
            } => {
                let w = self.worker.as_mut()?;
                w.log(format!("background link from {}", device_name(Some(label))));
                Some(Vec::new())
            }
            // A window: the snapshot the server built at attach carried every
            // unread message, so they are read now. Then the ordinary replay.
            WebInbound::ViewerAttached {
                kind: ConnectionKind::Window,
                label,
                address,
                ..
            } => {
                let w = self.worker.as_mut()?;
                w.messages.mark_all_read();
                w.log(format!(
                    "window attached: {} ({address})",
                    device_name(Some(label))
                ));
                None
            }
            WebInbound::ResizeTerminal {
                terminal_id, size, ..
            } => {
                self.resize_from_window(terminal_id, *size);
                Some(Vec::new())
            }
            WebInbound::WorkerOp { viewer_id, seq, op } => {
                Some(self.run_worker_op(viewer_id, *seq, op.clone()))
            }
            _ => None,
        }
    }

    /// W5 (§2.7): the window's pane for `terminal_id` is `size`. Resize that
    /// terminal (an agent's, or the worker shell), and remember the size so a
    /// project opened or an agent spawned later starts at it too.
    fn resize_from_window(&mut self, terminal_id: &TerminalId, size: PtySize) {
        use crate::terminal::session::{MIN_GRID_COLS, MIN_GRID_ROWS};
        let size = PtySize {
            rows: size.rows.max(MIN_GRID_ROWS),
            cols: size.cols.max(MIN_GRID_COLS),
        };
        if terminal_id.as_str() == WORKER_SHELL_TERMINAL_ID {
            if let Some(w) = self.worker.as_mut() {
                w.shell.resize(size);
            }
        } else if let Some((pi, ti, target)) =
            locate_web_terminal(&self.workspace, terminal_id.as_str())
        {
            let tab_id = self.workspace.projects[pi].state.tabs[ti].meta.id.clone();
            let target = match target {
                crate::tui::render::ChildTarget::Primary => crate::view::TerminalRef::Primary,
                crate::tui::render::ChildTarget::Child(c) => crate::view::TerminalRef::Child(c),
            };
            self.resize_terminal(pi, &tab_id, target, size);
        }
        if let Some(w) = self.worker.as_mut() {
            w.last_window_size = Some(size);
        }
        for p in self.workspace.projects.iter_mut() {
            p.state.set_pty_size(size);
        }
    }

    /// Run one of the window's project-picker or shell operations, answering
    /// the [`Ack`] and then whatever the operation produced, to the viewer
    /// that asked.
    fn run_worker_op(&mut self, viewer_id: &ViewerId, seq: u64, op: WorkerOp) -> Vec<WebOutbound> {
        let (outcome, detail, answer) = match self.worker_op(seq, op) {
            Ok((detail, answer)) => (AckOutcome::Applied, detail, answer),
            Err(refusal) => (
                AckOutcome::Rejected,
                Some(refusal.reason),
                refusal.frame.map(|frame| *frame),
            ),
        };
        let mut out = vec![WebOutbound::Viewer {
            viewer_id: viewer_id.clone(),
            msg: ServerMsg::Ack(Ack {
                seq,
                outcome,
                detail,
            }),
        }];
        if let Some(msg) = answer {
            out.push(WebOutbound::Viewer {
                viewer_id: viewer_id.clone(),
                msg,
            });
        }
        out
    }

    /// One worker op: `Ok((ack detail, answer frame))` when applied,
    /// `Err(refusal)` when refused.
    fn worker_op(&mut self, seq: u64, op: WorkerOp) -> OpOutcome {
        let fs = self.env.fs;
        let Some(w) = self.worker.as_mut() else {
            return Err(OpRefusal::new("This FlightDeck is not a worker."));
        };
        match op {
            WorkerOp::BrowseDir { path, show_hidden } => {
                let listing = projects::browse(fs, &w.confinement, path.as_deref(), show_hidden)
                    .map_err(|e| OpRefusal::new(e.to_string()))?;
                let workspace = &self.workspace;
                let frame = DirListing {
                    seq,
                    path: listing.path.display().to_string(),
                    parent: listing.parent.map(|p| p.display().to_string()),
                    home: w.confinement.home().display().to_string(),
                    entries: listing
                        .entries
                        .into_iter()
                        .map(|e| DirEntryView {
                            is_open_project: workspace.contains_root(&e.path),
                            name: e.name,
                            path: e.path.display().to_string(),
                            is_git_repo: e.is_git_repo,
                        })
                        .collect(),
                    truncated: listing.truncated,
                };
                Ok((None, Some(ServerMsg::DirListing(frame))))
            }
            WorkerOp::ScanProjects => {
                let root = w.projects_root.clone();
                let mut frame = ProjectScan {
                    seq,
                    root: root.display().to_string(),
                    root_exists: fs.is_dir(&root),
                    repos: Vec::new(),
                    refusal: None,
                };
                match projects::scan(fs, &w.confinement, &root, 2) {
                    Ok(entries) => {
                        frame.repos = entries
                            .into_iter()
                            .map(|e| ScannedRepo {
                                is_open_project: self.workspace.contains_root(&e.path),
                                name: e.name,
                                path: e.path.display().to_string(),
                            })
                            .collect();
                        Ok((None, Some(ServerMsg::ProjectScan(frame))))
                    }
                    Err(e) => {
                        let reason = e.to_string();
                        frame.refusal = Some(reason.clone());
                        Err(OpRefusal {
                            reason,
                            frame: Some(Box::new(ServerMsg::ProjectScan(frame))),
                        })
                    }
                }
            }
            WorkerOp::OpenProjectPath { path } => {
                let canonical = projects::validate_open(fs, &w.confinement, &path)
                    .map_err(|e| OpRefusal::new(e.to_string()))?;
                let opened = self
                    .open_worker_project(&canonical)
                    .map_err(|reason| OpRefusal::new(format!("Could not open {path}: {reason}")))?;
                Ok((Some(opened.sentence), None))
            }
            WorkerOp::CloneProject { url, name } => {
                if w.clone_meta.contains_key(&seq) {
                    return Err(OpRefusal::new(format!("Clone {seq} is already running.")));
                }
                let dest =
                    clone::plan_clone(fs, &w.confinement, &url, name.as_deref(), &w.projects_root)
                        .map_err(OpRefusal::new)?;
                w.clones
                    .start(seq, url.clone(), dest.clone(), Arc::clone(&w.cloner));
                w.clone_meta.insert(
                    seq,
                    CloneMeta {
                        url: url.clone(),
                        last_progress_ms: None,
                    },
                );
                w.log(format!("cloning {url} into {}", dest.display()));
                Ok((Some(format!("Cloning into {}…", dest.display())), None))
            }
            WorkerOp::RestartWorkerShell => {
                let home = w.home.clone();
                match w.shell.restart(self.env.pty, &home) {
                    Ok(()) => {
                        w.log("worker shell restarted");
                        Ok((Some("Restarted the worker shell.".to_string()), None))
                    }
                    Err(e) => Err(OpRefusal::new(format!(
                        "The worker shell did not start: {e}"
                    ))),
                }
            }
        }
    }

    /// The worker's own part of the published state, or `None` on any other
    /// host.
    pub(super) fn worker_host_state(&self) -> Option<WorkerHostState> {
        let w = self.worker.as_ref()?;
        let id = shell_terminal_id();
        let streams = &self.web_surface.streams;
        let shell = w.shell.facts().map(|facts| WorkerShellView {
            terminal_id: id.clone(),
            title: facts.title,
            geometry: facts.geometry,
            byte_len: streams.byte_len(&id),
            replay_from: streams.replay_from(&id),
            alive: facts.alive,
            exit_code: facts.exit_code,
        });
        Some(WorkerHostState {
            home: w.confinement.home().display().to_string(),
            shell,
            health: w.last_health.clone(),
            status: self.worker_status_view(w),
            host_messages: w.messages.unread(),
        })
    }

    fn listen_status(&self, w: &WorkerExt) -> ListenStatus {
        match self.web_surface.handle.as_ref() {
            Some(handle) => handle.listen_status(),
            None => ListenStatus {
                reason: Some(
                    w.not_listening_reason
                        .clone()
                        .unwrap_or_else(|| "not started".to_string()),
                ),
                ..ListenStatus::default()
            },
        }
    }

    fn worker_status_view(&self, w: &WorkerExt) -> WorkerStatusView {
        let status = self.listen_status(w);
        WorkerStatusView {
            listening: status.bound.iter().map(|a| a.to_string()).collect(),
            advertised: status.advertised,
            not_listening_reason: status.reason,
            unread_messages: w.messages.unread_count(),
            projects_root: w.projects_root.display().to_string(),
        }
    }

    /// What kind of host this is.
    pub fn host_kind(&self) -> HostKind {
        self.kind
    }

    /// What `worker status` shows about the running worker.
    pub fn worker_report(&self) -> WorkerRuntimeReport {
        use crate::web::protocol::ConnectionKind;
        let now_ms = self.env.clock.now_millis();
        let (uptime_secs, unread_messages, listen) = match self.worker.as_ref() {
            Some(w) => {
                let status = self.listen_status(w);
                (
                    now_ms.saturating_sub(w.started_ms) / 1000,
                    w.messages.unread_count(),
                    status,
                )
            }
            None => (0, 0, ListenStatus::default()),
        };
        let connections = self
            .web_surface
            .handle
            .as_ref()
            .map(|h| h.connections())
            .unwrap_or_default();
        let devices = self.devices();
        let attached = connections
            .iter()
            .find(|c| c.kind == ConnectionKind::Window)
            .map(|c| AttachedReport {
                device_id: c.token.as_str().to_string(),
                label: devices
                    .iter()
                    .find(|d| d.id == c.token.as_str())
                    .map(|d| d.label.clone())
                    .unwrap_or_else(|| device_name(Some(&c.label))),
                since: (c.since_ms.max(0) as u64) / 1000,
            });
        let background_links = connections
            .iter()
            .filter(|c| c.kind == ConnectionKind::Background)
            .count() as u32;
        let projects = self
            .workspace
            .projects
            .iter()
            .map(|p| ProjectReport {
                name: p.name.clone(),
                root: p.git.root().display().to_string(),
                agents: p
                    .state
                    .tabs
                    .iter()
                    .map(|tab| AgentReport {
                        id: tab.meta.id.clone(),
                        name: tab.meta.name.clone(),
                        agent: tab.meta.agent.clone(),
                        status: tab
                            .display_status(self.now_ms)
                            .interpreted
                            .as_str()
                            .to_string(),
                        running: primary_running(tab),
                    })
                    .collect(),
            })
            .collect();
        WorkerRuntimeReport {
            version: crate::VERSION.to_string(),
            pid: std::process::id(),
            uptime_secs,
            listen: ListenReport {
                listening: !listen.bound.is_empty(),
                addresses: listen.bound.iter().map(|a| a.to_string()).collect(),
                advertised: listen.advertised,
                reason: listen.reason,
                origin: listen.origin.map(|o| o.label().to_string()),
            },
            devices,
            attached,
            background_links,
            projects,
            unread_messages,
        }
    }

    /// Mint a single-use pairing code (`worker pair`).
    pub fn mint_pairing_code(&mut self) -> PairingCode {
        let code = {
            let mut store = match self.web_surface.credentials.lock() {
                Ok(store) => store,
                Err(poisoned) => poisoned.into_inner(),
            };
            store.mint_bootstrap_code().reveal().to_string()
        };
        let status = match self.worker.as_ref() {
            Some(w) => self.listen_status(w),
            None => ListenStatus::default(),
        };
        let listening = !status.bound.is_empty();
        let address = listening
            .then(|| {
                status
                    .advertised
                    .clone()
                    .or_else(|| status.bound.first().map(|a| a.to_string()))
            })
            .flatten();
        if let Some(w) = self.worker.as_mut() {
            w.log("pairing code minted");
        }
        PairingCode {
            code,
            expires_in_secs: BOOTSTRAP_CODE_TTL_MS / 1000,
            address,
            listening,
            not_listening_reason: (!listening).then_some(status.reason).flatten(),
        }
    }

    /// Every paired Desktop, with its connection state.
    pub fn devices(&self) -> Vec<DeviceReport> {
        let connections = self
            .web_surface
            .handle
            .as_ref()
            .map(|h| h.connections())
            .unwrap_or_default();
        let store = match self.web_surface.credentials.lock() {
            Ok(store) => store,
            Err(poisoned) => poisoned.into_inner(),
        };
        store
            .records()
            .iter()
            .map(|record| device_report(record, &connections))
            .collect()
    }

    /// Revoke the Desktop `selector` names (an exact id, an exact label, or a
    /// unique id prefix) and close its connections; returns the device and
    /// how many connections were closed. `Err` names the candidates when the
    /// selector is ambiguous.
    pub fn revoke_device(
        &mut self,
        selector: &str,
    ) -> std::result::Result<(DeviceReport, u32), String> {
        let selector = selector.trim();
        if selector.is_empty() {
            return Err(
                "Name the device to revoke: its id or its name (see `flightdeck worker devices`)."
                    .to_string(),
            );
        }
        let devices = self.devices();
        let active: Vec<&DeviceReport> = devices.iter().filter(|d| !d.revoked).collect();
        let chosen = if let Some(d) = active.iter().find(|d| d.id == selector) {
            *d
        } else {
            let by_label: Vec<&DeviceReport> = active
                .iter()
                .copied()
                .filter(|d| d.label.eq_ignore_ascii_case(selector))
                .collect();
            match by_label.as_slice() {
                [one] => *one,
                [] => {
                    let by_prefix: Vec<&DeviceReport> = active
                        .iter()
                        .copied()
                        .filter(|d| d.id.starts_with(selector))
                        .collect();
                    match by_prefix.as_slice() {
                        [one] => *one,
                        [] => return Err(format!("No paired device matches '{selector}'.")),
                        many => return Err(ambiguous(selector, many)),
                    }
                }
                many => return Err(ambiguous(selector, many)),
            }
        };
        let token = {
            let mut store = match self.web_surface.credentials.lock() {
                Ok(store) => store,
                Err(poisoned) => poisoned.into_inner(),
            };
            let token = store
                .records()
                .iter()
                .find(|r| r.id.as_str() == chosen.id)
                .map(|r| r.id.clone())
                .ok_or_else(|| format!("No paired device matches '{selector}'."))?;
            // Refused for the rest of this process whether or not the file
            // could be written; a failed write is still reported.
            store
                .revoke(&token)
                .map_err(|e| format!("Revoked, but the device file could not be saved: {e}"))?;
            token
        };
        let closed = match self.web_surface.handle.as_ref() {
            Some(handle) => {
                let closed = handle
                    .connections()
                    .iter()
                    .filter(|c| c.token == token)
                    .count() as u32;
                handle.recheck_credentials();
                closed
            }
            None => 0,
        };
        let mut report = chosen.clone();
        report.revoked = true;
        report.state = DeviceState::Offline;
        if let Some(w) = self.worker.as_mut() {
            w.log(format!(
                "revoked device {} ({}), closed {closed} connection(s)",
                report.label, report.id
            ));
        }
        Ok((report, closed))
    }

    /// Record whether the coming stop is a restart, so connections close
    /// with "restarting" rather than "worker stopped".
    pub fn set_shutdown_intent(&mut self, restarting: bool) {
        if let Some(w) = self.worker.as_mut() {
            w.shutdown_restarting = restarting;
        }
    }

    /// Close every connection (as a stop or a restart, per
    /// [`AppHost::set_shutdown_intent`]) and stop the worker shell. Design
    /// A8: a stop is `HostQuit` with "worker stopped", which a Desktop's
    /// background link reads as "offline, keep retrying"; a restart is
    /// `Restarting`. Idempotent.
    pub fn stop_worker(&mut self) {
        let Some(w) = self.worker.as_mut() else {
            return;
        };
        let notice = if w.shutdown_restarting {
            ShutdownNotice {
                reason: crate::web::protocol::ShutdownReason::Restarting,
                detail: None,
                initiator: None,
            }
        } else {
            ShutdownNotice {
                detail: Some("worker stopped".to_string()),
                ..ShutdownNotice::host_quit(None)
            }
        };
        w.shell.terminate();
        w.log(if w.shutdown_restarting {
            "stopping for a restart"
        } else {
            "stopping"
        });
        self.web_surface.stop(notice);
    }

    /// Lines for the worker log since the last call: listen changes,
    /// takeovers, host messages.
    pub fn take_log_lines(&mut self) -> Vec<String> {
        self.worker
            .as_mut()
            .map(|w| std::mem::take(&mut w.log_lines))
            .unwrap_or_default()
    }
}

/// What [`AppHost::open_worker_project`] opened.
pub(crate) struct Opened {
    pub(crate) project_id: ProjectId,
    /// The sentence the window's ack carries.
    pub(crate) sentence: String,
}

fn primary_running(tab: &crate::app::state::RuntimeTab) -> bool {
    use crate::contracts::ProcessState;
    tab.session.primary().is_some_and(|t| {
        matches!(
            t.process_state(),
            ProcessState::Running | ProcessState::Starting
        )
    })
}

fn agent_counts(workspace: &Workspace, _now_ms: u64) -> AgentCounts {
    let mut counts = AgentCounts::default();
    for tab in workspace.projects.iter().flat_map(|p| p.state.tabs.iter()) {
        counts.total += 1;
        if primary_running(tab) {
            counts.running += 1;
        }
    }
    counts
}
