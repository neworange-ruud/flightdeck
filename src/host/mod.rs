//! The front-end-neutral application host (SPECS §23, §27).
//!
//! [`AppHost`] owns everything the event loop used to own that is *not* the
//! terminal: the open [`Workspace`], the interactive [`Ui`] state layered over
//! it, the per-tick servicing of every project (PTY drain, background worker
//! results, status files, notifications), FlightDeck Remote (relay link,
//! pairing, the phone command bridge and its `deferred_pty` queue) and
//! FlightDeck Web (browser frames in, state and deltas out, the input lock).
//!
//! It deliberately does **not** own a loop or a thread. A front-end drives it
//! from its own loop — the TUI from its terminal poll loop, the GPUI desktop
//! app from its executor — by calling [`AppHost::pump`] and [`AppHost::publish`]
//! (or [`AppHost::tick`], which is both) once per turn, feeding input through
//! [`AppHost::handle`], and reading state back for rendering. Everything that
//! is genuinely the front-end's stays with the front-end: reading raw input,
//! mapping keys and mouse gestures, laying out panes (and so deciding PTY
//! sizes), drawing, and suspending itself for `$EDITOR`.
//!
//! Every modal the TUI can show — confirmations, text prompts, choice lists,
//! the palette, help, the configuration manager, the pairing and access
//! overlays — is readable through [`AppHost::overlay`] as plain data
//! ([`overlay`]) and answerable through [`HostEvent::Overlay`], routed into
//! the same handlers the TUI's keys reach.
//!
//! Why a top-level module rather than `crate::app`: `app` is the headless
//! per-project core that `web` and `remote` themselves build on. The host sits
//! one layer *above* all three — it wires a whole workspace of `AppState`s to
//! the web server and the relay — so nesting it under `app` would invert the
//! dependency. Both front-ends then sit on top of the host.
//!
//! This module names no terminal-UI library type, in its API or anywhere in
//! its body: a GPUI front-end links it without learning how the TUI draws.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use crate::app::commands::{Command, Selector};
use crate::app::modes::InputMode;
use crate::app::state::AppState;
use crate::contracts::error::{FlightDeckError, Result};
use crate::contracts::{Notifier, PtySize};
use crate::git::status::WorktreeStatus;
use crate::persistence::workspace::{
    load_workspace, save_workspace, workspace_state_path, WorkspaceState, WORKSPACE_VERSION,
};
use crate::remote::commands::{CommandLedger, PendingFirstTask};
use crate::remote::pairing::{build_channel, PairingSession};
use crate::remote::{ProjectView, RemoteBridge, RemoteInbound, RemoteOutbound};
use crate::{
    apply_host_event, apply_update_notice, build_web_host_state, cleanup_isolated_run,
    drain_create_outcomes, drain_pty_output, drive_pairing_overlay, isolated_status_dir,
    open_project, open_web_access_overlay, persist_quietly, rebind_web_interface,
    record_web_transitions, refresh_web_access_overlay, reload_all_projects_config,
    resize_workspace, resolve_dialog_outcomes, service_remote_commands, spawn_finish_count,
    spawn_status_refresh, spawn_worktree_job, start_isolated_session, start_remote,
    terminate_all_sessions, update_check_enabled, web_dialog_view, web_host_state_now,
    web_input_holder, web_started_message, Env, RemoteSetup, StatusMsg, Ui, WebSurface, Workspace,
    WorkspaceTerminals,
};
use flightdeck_remote_protocol::ProjectId;

pub mod overlay;
pub use overlay::{
    AgentChoice, ButtonRole, ConfigView, DialogButton, DialogKind, DialogRow, DialogView,
    GitStatusView, HostNotices, MessageView, NewAgentForm, NewAgentTarget, OverlayInput,
    OverlayKey, OverlayView, PairingView, PaletteRow, PaletteView, WebAccessOverlay,
};

#[cfg(test)]
mod tests;

/// Refresh the git-status cache every N ticks (a tick is one loop iteration,
/// roughly the front-end's poll interval when idle — 50 ms in the TUI). Kept
/// coarse so we never block the UI.
pub const GIT_REFRESH_EVERY: u64 = 40;

/// One front-end-neutral input for [`AppHost::handle`].
///
/// These are the actions a key map or a click resolves *to*, not raw keys: a
/// front-end owns its own input vocabulary (terminal key events, GPUI
/// keystrokes, mouse hit-testing) and translates it into these. Each arm does
/// exactly what the TUI's corresponding key action does, through the same
/// helpers, so the two front-ends cannot drift on what an action means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    /// Dispatch an app-core command against the active project. Commands that
    /// need interactive input first (an empty New/Rename, Close, …) open the
    /// matching prompt on the [`AppHost`]'s dialog state instead.
    Command(Command),
    /// Switch the active project (workspace-level, not an `AppState` command).
    SwitchProject(Selector),
    /// Bytes for the focused terminal, already encoded by the front-end.
    /// Claims the input lock first, exactly like a desktop keystroke (D14 as
    /// revised): refused bytes are dropped, never queued.
    TerminalInput(Vec<u8>),
    /// One atomic paste. A text-editing dialog consumes it as literal
    /// characters; otherwise only a focused terminal receives it.
    Paste(String),
    /// Every project's terminals are now this viewport size. A front-end whose
    /// chrome differs per project uses [`AppHost::resize_projects`] instead.
    Resize(PtySize),
    /// Leave terminal focus (APP mode).
    FocusApp,
    /// Enter terminal focus (TERMINAL mode).
    FocusTerminal,
    /// Ask the host to quit; see [`AppHost::should_quit`].
    Quit,
    /// Open the command palette (the TUI's `Ctrl-p`); read it back through
    /// [`AppHost::overlay`] and answer it with [`HostEvent::Overlay`].
    OpenPalette,
    /// Open the help overlay (the TUI's `F1`).
    OpenHelp,
    /// Run one palette row without the palette open — a front-end's menu item
    /// for a workspace-level action (`Open Project`, `Open Configuration`,
    /// `Pair Phone`, `Start Web Interface`, …) that no [`Command`] spells.
    /// Refused unless the palette would offer that row right now.
    RunPaletteAction(crate::tui::palette::PaletteAction),
    /// Answer the overlay on screen (see [`AppHost::overlay`]).
    Overlay(OverlayInput),
}

/// The UI-agnostic application host. See the module docs.
///
/// Borrows its services (`'a`) rather than owning them, the same shape the
/// event loop always had: a front-end builds the concrete `RealFs`/`RealClock`/…
/// once and lends them for the host's whole life.
pub struct AppHost<'a> {
    env: Env<'a>,
    notifier: &'a dyn Notifier,
    workspace: Workspace,
    ui: Ui,
    /// Whether this is an `--isolated` run (SPECS §32): one project, nothing
    /// persisted, a temp status directory to clean up.
    isolated: bool,
    /// Where the workspace file lives, or `None` for an isolated run (which
    /// writes no workspace file at all).
    ws_path: Option<PathBuf>,
    /// Loop iterations so far; drives the coarse git refresh and the one-shot
    /// first-tick autopair seam.
    tick: u64,
    /// The clock reading taken at the top of the current [`AppHost::pump`], so
    /// every phase of one turn — and the front-end's render — agree on "now".
    now_ms: u64,
    /// Home dir for locating agent session stores (used to pin each tab's
    /// resume session id). Resolved once; `None` disables pinning.
    store_home: Option<PathBuf>,
    update_rx: Receiver<String>,
    /// The update-check sender, handed to the checker by [`AppHost::start`].
    update_tx: Option<Sender<String>>,
    /// The relay-client channel ends [`AppHost::start`] hands to the client
    /// thread. `None` once started.
    remote_wiring: Option<(Sender<RemoteInbound>, Receiver<RemoteOutbound>)>,
    remote_in_rx: Receiver<RemoteInbound>,
    remote_out_tx: Sender<RemoteOutbound>,
    remote_setup: Option<RemoteSetup>,
    remote_bridge: Option<RemoteBridge>,
    /// The desktop pairing surface (Settings → Remote overlay). `Some` only
    /// while the QR/code overlay is on screen.
    pairing_session: Option<PairingSession>,
    autopair_hint: Option<String>,
    remote_ledger: CommandLedger,
    remote_first_tasks: Vec<PendingFirstTask>,
    remote_has_persisted_pairing: bool,
    web_surface: WebSurface,
    interfaces: crate::web::interfaces::RealInterfaceEnumerator,
    #[cfg(debug_assertions)]
    web_test_code: Option<String>,
}

impl<'a> AppHost<'a> {
    /// Open the workspace for a launch from `cwd`: the launch project (which
    /// must be a git repository, and is always opened and made active) plus
    /// every other project remembered from the previous session.
    ///
    /// Runs SPECS §7 startup for each project (init, recover — agents are
    /// never auto-relaunched here; see [`AppHost::resume_launch_project`]).
    /// Spawns no threads: [`AppHost::start`] does that.
    pub fn open(
        env: Env<'a>,
        notifier: &'a dyn Notifier,
        cwd: &Path,
        isolated: bool,
    ) -> Result<AppHost<'a>> {
        let isolated_root: Option<PathBuf> = if isolated {
            Some(isolated_status_dir())
        } else {
            None
        };

        // The launch project (the cwd's repository) must be a git repo — fail fast
        // with the friendly message if not. It is always opened and made active.
        let launch = open_project(&env, cwd, isolated_root.as_deref()).map_err(|e| {
            FlightDeckError::Git(format!(
                "not inside a Git repository (run FlightDeck from a git project): {e}"
            ))
        })?;

        let mut workspace = Workspace {
            projects: vec![launch],
            active: 0,
        };

        // Reopen any other projects remembered from the previous session (best
        // effort): skip the launch project, folders that no longer exist, and any
        // that are no longer git repositories. Each project's own tabs are still
        // recovered from its `state.json` (agents are never auto-relaunched).
        // An isolated run is exactly one project by definition: skip the reopen
        // loop entirely (SPECS §32). This must not merely pass `None` through to
        // `open_project` for each remembered project and discard the result —
        // `open_project` runs `startup` (init, config writes, `.gitignore`
        // update, state load + recovery) against every one of them, including
        // the launch repo's own root when it is already in the workspace file
        // (the normal case for a repo the user has opened before), before the
        // `contains_root` guard below throws the duplicate away. Binding
        // `ws_path` to `None` also makes teardown skip writing the workspace
        // file for free (Task 8).
        let ws_path = if isolated {
            None
        } else {
            workspace_state_path()
        };
        if let Some(ref wp) = ws_path {
            if let Ok(saved) = load_workspace(env.fs, wp) {
                for p in &saved.projects {
                    let pr = Path::new(p);
                    if !env.fs.is_dir(pr) {
                        continue;
                    }
                    match open_project(&env, pr, None) {
                        Ok(proj) if !workspace.contains_root(proj.git.root()) => {
                            workspace.projects.push(proj)
                        }
                        _ => {}
                    }
                }
            }
        }

        let web_surface = WebSurface::new(&workspace.active_project().state.config.web);
        let mut host = AppHost::from_parts(env, notifier, workspace, web_surface, isolated);
        host.ws_path = ws_path;
        host.store_home = crate::app::state::user_home();
        Ok(host)
    }

    /// Assemble a host around an already-built workspace and web surface. No
    /// I/O and no threads — the seam the tests build on, with a web surface
    /// whose credential store sits on a fake filesystem.
    pub(crate) fn from_parts(
        env: Env<'a>,
        notifier: &'a dyn Notifier,
        workspace: Workspace,
        web_surface: WebSurface,
        isolated: bool,
    ) -> AppHost<'a> {
        let (update_tx, update_rx) = std::sync::mpsc::channel::<String>();
        // FlightDeck Remote (optional): a long-lived relay-client thread, mirroring
        // the update-check thread idiom. Off by default — when disabled `start`
        // spawns nothing and the channels stay idle, so behaviour is unchanged.
        let (remote_in_tx, remote_in_rx) = std::sync::mpsc::channel::<RemoteInbound>();
        let (remote_out_tx, remote_out_rx) = std::sync::mpsc::channel::<RemoteOutbound>();
        AppHost {
            env,
            notifier,
            workspace,
            ui: Ui::default(),
            isolated,
            ws_path: None,
            tick: 0,
            now_ms: 0,
            store_home: None,
            update_rx,
            update_tx: Some(update_tx),
            remote_wiring: Some((remote_in_tx, remote_out_rx)),
            remote_in_rx,
            remote_out_tx,
            remote_setup: None,
            remote_bridge: None,
            pairing_session: None,
            autopair_hint: None,
            remote_ledger: CommandLedger::new(),
            remote_first_tasks: Vec::new(),
            remote_has_persisted_pairing: false,
            web_surface,
            interfaces: crate::web::interfaces::RealInterfaceEnumerator,
            #[cfg(debug_assertions)]
            web_test_code: None,
        }
    }

    // -----------------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------------

    /// Start the *active* (launched) project's agents: resume every recovered
    /// tab whose worktree still exists, or — in an isolated run — start its one
    /// fresh session. Call once, after the front-end has seeded PTY sizes, so
    /// agents spawn at the right width.
    ///
    /// Other projects reopened from the workspace file are shown but their
    /// agents are not auto-resumed; switching to one resumes it on demand.
    ///
    /// An isolated run's single session failing to start is an `Err` (not a
    /// `warnings.push`): `AppState::warnings` has no renderer, so surfacing it
    /// that way would launch a blank UI with no session and no visible
    /// message. The front-end should still run its full teardown.
    pub fn resume_launch_project(&mut self) -> Result<()> {
        let active = self.workspace.active;
        let p = &mut self.workspace.projects[active];
        let services = self.env.services(&p.git);
        if self.isolated {
            // One fresh session; nothing to resume, because nothing was
            // recovered (SPECS §32).
            start_isolated_session(&mut p.state, &services)
        } else {
            let _ = p.state.resume_agents(&services);
            Ok(())
        }
    }

    /// Bring the background services up: the startup notification grace, the
    /// once-a-day update check, FlightDeck Remote (when enabled) and FlightDeck
    /// Web (when `[web] enabled`). Call once, right before the first turn.
    pub fn start(&mut self) {
        let env = &self.env;
        let workspace = &mut self.workspace;

        // Suppress notifications briefly at startup so resumed/just-launched agents
        // settling to idle don't produce a burst of "finished" alerts (SPECS §24).
        let now0 = env.clock.now_millis();
        for p in workspace.projects.iter_mut() {
            p.state.begin_notification_grace(now0);
        }

        // Once-a-day update notice (SPECS §30): surface any cached "newer version"
        // finding immediately and, when due, kick off a background check. Applied to
        // every project so whichever is active shows the hint.
        if let Some(update_tx) = self.update_tx.take() {
            let check_enabled = update_check_enabled(&workspace.active_project().state);
            if let Some(latest) =
                crate::update::start_check(check_enabled, env.clock.now_unix_secs(), update_tx)
            {
                apply_update_notice(workspace, latest);
            }
        }

        // FlightDeck Remote (optional). When disabled `start_remote` spawns
        // nothing and returns `None`, so every tee/tick below is a cheap no-op.
        if let Some((remote_in_tx, remote_out_rx)) = self.remote_wiring.take() {
            self.remote_setup = start_remote(env, workspace, remote_in_tx, remote_out_rx);
        }
        // The outbound feed bridge exists only while the relay thread does. It builds
        // the phone-facing snapshots/deltas/transcript/events each tick and seals
        // them. A passthrough sealer is the default; when an already-established
        // pairing exists, the real E2E channel is installed right away (spec §7.1).
        // When remote is disabled this stays `None`, so every tee/tick below is a
        // cheap no-op and behaviour is bit-for-bit unchanged.
        if self.remote_bridge.is_none() {
            self.remote_bridge = self.remote_setup.as_ref().map(|_| {
                RemoteBridge::passthrough(now0 + crate::app::state::NOTIFY_STARTUP_GRACE_MS)
            });
            // Locate agent session files (per worktree) for transcript reconstruction
            // (remote-control-72k). Uses the same home the resume machinery uses.
            if let Some(b) = self.remote_bridge.as_mut() {
                b.set_transcript_home(self.store_home.clone());
            }
            if let (Some(b), Some(setup)) =
                (self.remote_bridge.as_mut(), self.remote_setup.as_ref())
            {
                if let Some(est) = &setup.established {
                    if let Ok((seal, open)) = build_channel(
                        &setup.identity_scalar,
                        &est.peer_ka_b64,
                        est.pairing_id.as_str(),
                        &est.claim_token,
                    ) {
                        b.install_channel(seal, open, est.last_sent_seq);
                    }
                }
            }
        }
        // Test / E2E seam (read once at startup): when `FLIGHTDECK_REMOTE_AUTOPAIR`
        // holds a 4-digit value and remote is enabled, the desktop offers pairing
        // non-interactively on the first tick using that fixed code, so an automated
        // harness gets a deterministic claim token instead of a random one plus a
        // keypress. `None` in every normal run, so behaviour is unchanged.
        self.autopair_hint = std::env::var("FLIGHTDECK_REMOTE_AUTOPAIR")
            .ok()
            .filter(|v| v.len() == 4 && v.bytes().all(|b| b.is_ascii_digit()));
        // Whether a phone pairing was persisted at startup and has not been
        // forgotten this session. `RemoteBridge::is_paired()` only turns true once
        // the phone reconnects, so this keeps "Unpair Phone" available (and "Pair
        // Phone" gated) for a configured-but-currently-absent phone. Cleared on
        // unpair and on a relay-side pairing rejection.
        self.remote_has_persisted_pairing = self
            .remote_setup
            .as_ref()
            .map(|s| s.established.is_some())
            .unwrap_or(false);

        // FlightDeck Web (optional): the embedded browser surface, started here
        // only when `[web] enabled` opted in — D10 makes auto-start the
        // front-end's decision, which is why `server::start` deliberately ignores
        // the flag. The surface itself was built with the host (it is cheap and
        // holds no buffers until the server runs).
        //
        // Test / E2E seam, debug builds only (read once at startup): when
        // `FLIGHTDECK_WEB_TEST_CODE` holds four digits, the running web server
        // always has *that* bootstrap code live, so the Playwright suite (D15) can
        // exchange it in a real browser instead of screen-scraping a TUI overlay for
        // a random one. `None` in every normal run, and absent entirely from a
        // release build — see `WebSurface::ensure_test_bootstrap_code`.
        #[cfg(debug_assertions)]
        {
            self.web_test_code = std::env::var("FLIGHTDECK_WEB_TEST_CODE")
                .ok()
                .filter(|v| v.len() == 4 && v.bytes().all(|b| b.is_ascii_digit()));
        }
        if self.workspace.active_project().state.config.web.enabled {
            let config = self.workspace.active_project().state.config.web.clone();
            // Deliberately no access overlay here. `[web] enabled` is a user who
            // asked for the server on every launch, not for a modal on every
            // launch; `Show Web Access` in the palette is how they reach the code.
            let initial = web_host_state_now(
                &self.workspace,
                &mut self.web_surface,
                &self.ui,
                self.env.clock,
                now0,
            );
            match self.web_surface.start(&config, initial) {
                Ok((addr, exposure)) => self.ui.message(web_started_message(addr, exposure)),
                Err(e) => self.ui.message(format!("Web interface did not start: {e}")),
            }
        }
    }

    /// Stop the background services: tell any attached browser that FlightDeck
    /// itself is going away, before the listener closes (Q5), so it enters a
    /// terminal state instead of spinning in "reconnecting…" against a host that
    /// no longer exists; then tear down the relay client (best-effort join).
    /// Idempotent.
    pub fn stop_services(&mut self) {
        self.web_surface
            .stop(crate::web::server::ShutdownNotice::host_quit(None));
        if let Some(setup) = self.remote_setup.take() {
            setup.handle.stop();
        }
    }

    /// Persist every project's `state.json` and the workspace file (SPECS §9).
    /// Skipped entirely for an isolated run (SPECS §32). Keeps going past a
    /// failing project so one bad write never costs the others theirs; the last
    /// error is returned.
    pub fn persist(&self) -> Result<()> {
        let mut persist_result = Ok(());
        if !self.isolated {
            for p in self.workspace.projects.iter() {
                let services = self.env.services(&p.git);
                if let Err(e) = persist_quietly(&p.state, &services) {
                    persist_result = Err(e);
                }
            }
        }
        if let Some(wp) = &self.ws_path {
            let ws_state = WorkspaceState {
                version: WORKSPACE_VERSION,
                projects: self
                    .workspace
                    .projects
                    .iter()
                    .map(|p| p.git.root().to_string_lossy().to_string())
                    .collect(),
                active: self.workspace.active,
            };
            let _ = save_workspace(self.env.fs, wp, &ws_state);
        }
        persist_result
    }

    /// Terminate every session so no orphaned child processes remain (SPECS §25).
    pub fn terminate_sessions(&mut self) {
        for p in self.workspace.projects.iter_mut() {
            terminate_all_sessions(&mut p.state);
        }
    }

    /// Remove an isolated run's temp status directory. Call only after
    /// [`AppHost::terminate_sessions`] — otherwise a hook still running
    /// mid-teardown could recreate files under a directory just deleted. A
    /// no-op for a normal run.
    pub fn cleanup_isolated(&self) {
        if self.isolated {
            cleanup_isolated_run(self.env.fs, &isolated_status_dir());
        }
    }

    // -----------------------------------------------------------------------
    // Driving: one turn = pump → (front-end sizing) → publish → render → input
    // -----------------------------------------------------------------------

    /// One turn, for a front-end that sizes its terminals elsewhere: exactly
    /// [`AppHost::pump`] followed by [`AppHost::publish`]. Returns whether
    /// anything arrived that warrants a redraw (see [`AppHost::pump`]).
    pub fn tick(&mut self) -> bool {
        let pumped = self.pump();
        let published = self.publish();
        pumped || published
    }

    /// First phase of a turn: service **every** project so background projects
    /// stay live — drain their PTYs, finalize completed worktrees, poll status
    /// files, fire notifications — then the update check, the relay link and
    /// the phone command bridge (including the `deferred_pty` flush), the
    /// pairing overlay, and the coarse git-status refresh.
    ///
    /// Reads the clock once; the rest of the turn (and [`AppHost::now_ms`])
    /// sees that same instant.
    ///
    /// Returns `true` when something arrived this turn that changes what is on
    /// screen: PTY output, a worker result, a notification, an agent status
    /// change, a relay frame, an update notice. It is a hint, not a proof of
    /// the opposite: time-derived display (elapsed times, countdowns, spinners)
    /// moves without any event, so a front-end that skips redraws on `false`
    /// should still redraw on a coarse timer.
    pub fn pump(&mut self) -> bool {
        let env = &self.env;
        let workspace = &mut self.workspace;
        let ui = &mut self.ui;
        let web_surface = &mut self.web_surface;
        let remote_bridge = &mut self.remote_bridge;
        let now_ms = env.clock.now_millis();
        self.now_ms = now_ms;
        let active = workspace.active;
        let n = workspace.projects.len();
        let before = status_fingerprint(workspace, now_ms);
        let mut changed = false;

        // --- Service EVERY project each tick so background projects stay live:
        //     drain their PTYs, finalize completed worktrees, poll status files,
        //     and fire notifications regardless of which project is on screen. ---
        for idx in 0..n {
            let is_active = idx == active;
            let p = &mut workspace.projects[idx];

            drain_pty_output(&mut p.state, now_ms, |sid, which, mint, bytes| {
                changed = true;
                // FlightDeck Web (D2): the raw chunk into this terminal's replay
                // ring, and straight out to every attached viewer. Only while the
                // server is running — see `WebSurface` on the memory this costs
                // and why it is not paid by a user who never starts it.
                web_surface.tee(sid, which, mint, bytes);
                if let Some(b) = remote_bridge.as_mut() {
                    // Primary (None) bytes no longer build the transcript — it is
                    // reconstructed from the agent's session file each tick (see
                    // `RemoteBridge::sync_transcript`, remote-control-72k), because
                    // full-screen agents paint the alt-screen and emit no lines.
                    // Child bytes still stream to the phone iff that child backs
                    // the session's live remote shell.
                    if let Some(child_index) = which {
                        b.shell_pump(sid, child_index, bytes);
                    }
                }
            });

            {
                let services = env.services(&p.git);
                changed |=
                    drain_create_outcomes(&p.create_rx, &mut p.state, &services, ui, is_active);
            }

            // Prune cache entries for tabs that no longer exist.
            p.cache
                .retain(|id, _| p.state.tabs.iter().any(|t| &t.meta.id == id));

            while let Ok(msg) = p.status_rx.try_recv() {
                changed = true;
                match msg {
                    StatusMsg::Update(id, status) => {
                        p.state
                            .observe_git_status(&id, &status, env.clock.now_unix_secs());
                        p.cache.insert(id, status);
                    }
                    StatusMsg::Done => p.status_in_flight = false,
                }
            }

            {
                let services = env.services(&p.git);
                p.state.poll_status_files(&services, now_ms);
                p.state
                    .sync_activity(services.clock.now_unix_secs(), now_ms, is_active);
                // Pin each freshly-launched agent's session id for later
                // resume. A no-op unless a tab is awaiting its session file, and
                // rate-limited to `SESSION_SCAN_INTERVAL_MS` when one is, since
                // that wait has no deadline.
                if let Some(home) = &self.store_home {
                    p.state.pin_resumable_sessions(home, &services, now_ms);
                }
            }

            // Prefix the project name so alerts read "project: tab" — useful
            // when several projects are open at once (SPECS §24).
            for mut note in p.state.take_finish_notifications(now_ms) {
                changed = true;
                note.title = format!("{}: {}", p.name, note.title);
                self.notifier.notify(&note);
            }

            // FlightDeck Web (D11): the same lifecycle signal, recorded a second
            // time for the browser's activity feed. Deliberately a *tee at the
            // source* rather than a second read of the notifications above:
            // `take_finish_notifications` spends each tab's arming and drops
            // whatever `[notifications]` disabled or the startup grace window
            // suppressed, so a feed built from its output would be missing
            // exactly the events D11 exists to deliver. Running after it is
            // therefore free of consequence for the desktop — the two keep
            // separate per-tab edge memory — and this record happens whether or
            // not the server is up, so a browser opened later lands on history
            // rather than silence (`WebSurface::activity`).
            //
            // A finished session's row also wants the count artboard 2e shows
            // (`finished, 18 files touched`), which only git knows: each
            // returned request is one `git status --porcelain` on that tab's
            // worktree, spawned here and answered into `count_tx`. This is the
            // *only* git work the feed adds, it is per finished session rather
            // than per tick, and the periodic cache above still refreshes the
            // active project alone.
            for request in record_web_transitions(web_surface, p, env.clock, now_ms) {
                spawn_finish_count(&p.git, &web_surface.count_tx, request);
            }
        }

        // Land the finish-edge counts that came back, and let go of any row
        // that has waited too long for one — before anything reads the feed
        // this tick.
        web_surface.drain_finish_counts(env.clock, now_ms);

        // --- Apply a completed background update check (SPECS §30). ---
        while let Ok(latest) = self.update_rx.try_recv() {
            changed = true;
            apply_update_notice(workspace, latest);
        }

        // --- Drain relay-client events (link state, envelopes, presence) into
        //     the outbound bridge, then push this tick's feed. Inbound is
        //     handled before the tick so a just-arrived `request_snapshot` /
        //     pairing is reflected in what we send. Command envelopes beyond
        //     snapshot/transcript requests are queued for the command-bridge
        //     task via `RemoteBridge::take_pending_commands`. ---
        let remote_out_tx = &self.remote_out_tx;
        if let Some(b) = remote_bridge.as_mut() {
            let identity_scalar = self
                .remote_setup
                .as_ref()
                .map(|s| s.identity_scalar.as_slice())
                .unwrap_or(&[]);
            while let Ok(msg) = self.remote_in_rx.try_recv() {
                changed = true;
                // Drive the pairing overlay + E2E go-live off the pairing frames.
                match &msg {
                    RemoteInbound::PairingOffered {
                        pairing_id,
                        claim_token,
                        expires_at_ms,
                    } => {
                        if let Some(ps) = self.pairing_session.as_mut() {
                            ps.on_offered(pairing_id.clone(), claim_token.clone(), *expires_at_ms);
                        }
                    }
                    RemoteInbound::PairingClaimed {
                        pairing_id,
                        peer_key_agreement_public_key,
                        ..
                    } => {
                        if let Some(ps) = self.pairing_session.as_mut() {
                            if ps.on_claimed(
                                pairing_id.clone(),
                                peer_key_agreement_public_key.clone(),
                            ) {
                                // The instant a phone joins: derive the real
                                // channel and swap it in for the passthrough.
                                if let Ok((_pid, seal, open)) = ps.derive_channel(identity_scalar) {
                                    b.install_channel(seal, open, 0);
                                }
                            }
                        }
                    }
                    RemoteInbound::HandshakeFailed { reason, retrying } => {
                        // The relay link never reached `auth_ok`, so no pairing
                        // code can arrive. Tell the overlay why: a refusal (no
                        // relay password configured, for instance) fails the
                        // attempt, a transient failure just explains the wait.
                        // Without this the overlay showed "Requesting a pairing
                        // code from the relay…" forever while the client
                        // backoff-looped in silence.
                        if let Some(ps) = self.pairing_session.as_mut() {
                            ps.on_handshake_failed(reason, *retrying);
                        }
                    }
                    RemoteInbound::PairingRejected { .. } => {
                        // The relay no longer recognizes our pairing; the client
                        // dropped the stale record and will re-offer. Give the
                        // user a clear, actionable state instead of a silent,
                        // endless "reconnecting" (remote-control-1jy).
                        self.pairing_session = None;
                        self.remote_has_persisted_pairing = false;
                        ui.close_remote_overlay();
                        ui.message(
                            "Phone pairing is no longer recognized by the relay. \
                             Open Settings → Remote to pair again.",
                        );
                    }
                    RemoteInbound::PairingRevoked { .. } => {
                        // The phone unpaired this Mac (spec §10.2). The client
                        // already dropped the pairing; clear the overlay/session
                        // and let the user know they can pair again.
                        self.pairing_session = None;
                        self.remote_has_persisted_pairing = false;
                        ui.close_remote_overlay();
                        ui.message(
                            "Your phone unpaired this Mac. \
                             Open Settings → Remote to pair again.",
                        );
                    }
                    _ => {}
                }
                b.handle_inbound(msg);
            }
            {
                let views: Vec<ProjectView> = workspace
                    .projects
                    .iter()
                    .map(|p| ProjectView {
                        id: ProjectId::new(p.name.clone()),
                        name: &p.name,
                        state: &p.state,
                        cache: &p.cache,
                    })
                    .collect();
                b.tick(&views, now_ms, &mut |out| {
                    let _ = remote_out_tx.send(out);
                });
            }
            // Inbound phone commands queued by the bridge: idempotency-check,
            // translate, execute on this (main) thread through the existing
            // Command/PTY paths, and ack each with its actual outcome.
            service_remote_commands(
                b,
                &mut self.remote_ledger,
                &mut self.remote_first_tasks,
                workspace,
                env,
                now_ms,
                &mut |out| {
                    let _ = remote_out_tx.send(out);
                },
            );
        } else {
            // Remote disabled: drain (and drop) so the channel never fills.
            while self.remote_in_rx.try_recv().is_ok() {}
        }

        // --- Test / E2E seam: on the first tick, auto-offer pairing with the
        //     fixed `FLIGHTDECK_REMOTE_AUTOPAIR` code when set and remote is
        //     enabled. This just requests the same offer the palette action does. ---
        if self.tick == 0 && self.autopair_hint.is_some() && self.remote_setup.is_some() {
            ui.pending_pair = true;
        }

        // A confirmed unpair (handled by `drive_pairing_overlay` below) forgets
        // the pairing, so drop the persisted flag before it is consumed.
        if ui.pending_unpair {
            self.remote_has_persisted_pairing = false;
        }
        // Refresh the palette's pairing gate: paired iff the live bridge has an
        // active pairing or a persisted one is still configured this session.
        ui.remote_paired = remote_bridge
            .as_ref()
            .map(|b| b.is_paired())
            .unwrap_or(false)
            || self.remote_has_persisted_pairing;

        // --- Desktop pairing surface (Settings → Remote): start an offer, keep
        //     the overlay in sync with the pairing session, and handle unpair. ---
        drive_pairing_overlay(
            ui,
            &mut self.pairing_session,
            remote_bridge.as_mut(),
            self.remote_setup.as_ref(),
            remote_out_tx,
            self.autopair_hint.as_deref(),
            now_ms,
        );

        // --- Refresh the git-status cache for the ACTIVE project only (it is
        //     the only one whose sidebar/info bar is on screen). ---
        if self.tick.is_multiple_of(GIT_REFRESH_EVERY) {
            let p = &mut workspace.projects[active];
            if !p.status_in_flight && spawn_status_refresh(&p.state, &p.git, &p.status_tx) {
                p.status_in_flight = true;
            }
        }
        self.tick = self.tick.wrapping_add(1);

        changed || status_fingerprint(workspace, now_ms) != before
    }

    /// Second phase of a turn, after the front-end has synced the active
    /// project's terminal sizes to its layout: drain what the browsers said,
    /// publish the state and the deltas that describe how it changed, act on
    /// the web start/stop/rebind requests the dialogs queued, keep the access
    /// overlay and the input lock current, and hand any queued worktree jobs to
    /// their background workers.
    ///
    /// Returns `true` when a browser frame was handled this turn.
    pub fn publish(&mut self) -> bool {
        let env = &self.env;
        let workspace = &mut self.workspace;
        let ui = &mut self.ui;
        let web_surface = &mut self.web_surface;
        let now_ms = self.now_ms;
        let mut changed = false;

        // --- FlightDeck Web: drain what the browsers said, then publish the
        //     state and the deltas that describe how it changed.
        //
        //     Ordering matters and is deliberate. Inbound is drained *first*, so
        //     a selection the browser just moved (D3) is reflected in the state
        //     published on this same tick rather than a tick later. And publish
        //     comes after the front-end's size sync, so the geometry the
        //     browser letterboxes is the grid the PTY actually has (D4). ---
        if web_surface.running() {
            let inbound: Vec<crate::web::server::WebInbound> =
                web_surface.inbound_rx.try_iter().collect();
            for event in inbound {
                changed = true;
                // A `Command` frame is the browser's palette pressing Enter:
                // `run_web_command` routes it into the same `run_palette_action`
                // the desktop's own palette calls, and answers with the ack that
                // dispatch earned. The server has already refused an unknown
                // name, a read-only seat's frame (D14) and every command whose
                // effect must not land for a browser (D16, including `quit`), so
                // reaching here means a controller sent something runnable.
                if let crate::web::server::WebInbound::Command {
                    viewer_id,
                    label,
                    command,
                } = &event
                {
                    // D13: a dialog this command opens is tagged with the seat
                    // that asked, so the desktop can say `opened from browser ·
                    // 192.168.2.20` about a modal nobody at this keyboard
                    // requested.
                    let origin = crate::web::protocol::DialogOrigin::Browser {
                        viewer_id: Some(viewer_id.clone()),
                        label: label.clone(),
                    };
                    let reply = crate::run_web_command(
                        command,
                        &origin,
                        workspace,
                        env,
                        ui,
                        &mut web_surface.activity,
                    );
                    if let Some(handle) = web_surface.handle.as_ref() {
                        handle.send(crate::web::server::WebOutbound::Viewer {
                            viewer_id: viewer_id.clone(),
                            msg: crate::web::protocol::ServerMsg::Ack(reply.ack),
                        });
                        // SPECS §21's panel, to the viewer that asked and to no
                        // other (§6.5 R16). After the ack, so a browser that
                        // reads frames in order learns the command landed
                        // before it is handed what the command produced.
                        if let Some(view) = reply.git_status {
                            handle.send(crate::web::server::WebOutbound::Viewer {
                                viewer_id: viewer_id.clone(),
                                msg: crate::web::protocol::ServerMsg::GitStatus(view),
                            });
                        }
                        // SPECS §8's manager, likewise to the viewer that
                        // asked and after the ack (§6.5 R22).
                        if let Some(view) = reply.config {
                            handle.send(crate::web::server::WebOutbound::Viewer {
                                viewer_id: viewer_id.clone(),
                                msg: crate::web::protocol::ServerMsg::Configuration(view),
                            });
                        }
                    }
                    continue;
                }
                let mut host = WorkspaceTerminals {
                    projects: &mut workspace.projects,
                };
                let out = web_surface.streams.apply_inbound(&event, &mut host);
                if let Some(handle) = web_surface.handle.as_ref() {
                    for frame in out {
                        handle.send(frame);
                    }
                }
            }

            let activity = web_surface.activity_events(env.clock);
            let next = build_web_host_state(
                workspace,
                &web_surface.streams,
                activity,
                web_dialog_view(
                    ui,
                    &workspace.active_project().name,
                    &workspace.active_project().state,
                ),
                now_ms,
            );
            let decided = std::mem::take(&mut ui.dialog_decisions);
            if next != web_surface.published {
                // Publish, *then* the matching deltas: publishing changes what
                // the next attach sees and notifies nobody, deliberately, so the
                // host is the one that says what changed (see `HostState`).
                let mut frames = crate::web::stream::deltas(&web_surface.published, &next);
                // D13: the diff can only say `Superseded` about a dialog that is
                // gone. Where somebody actually decided, say so.
                resolve_dialog_outcomes(&mut frames, &decided);
                if let Some(handle) = web_surface.handle.as_ref() {
                    handle.publish_state(next.clone());
                    for delta in frames {
                        handle.send(crate::web::server::WebOutbound::All(
                            crate::web::protocol::ServerMsg::Delta(delta),
                        ));
                    }
                }
                web_surface.published = next;
            }
        }
        // Drained whether or not anyone is watching (D13). A desktop-only run
        // still decides dialogs, and a list nobody ever reads would grow for the
        // life of the process — so the take above is paired with a clear here
        // rather than living inside the `running()` branch.
        ui.dialog_decisions.clear();

        // --- Start / stop the web interface, when the palette asked (D10). ---
        if ui.pending_web_start {
            ui.pending_web_start = false;
            let config = workspace.active_project().state.config.web.clone();
            let initial = web_host_state_now(workspace, web_surface, ui, env.clock, now_ms);
            match web_surface.start(&config, initial) {
                // The access overlay carries the bound address *and* D5's
                // warning in a stronger form than this one line does, so when
                // it is about to open the line would only be overwritten by it
                // — and a message the user never sees is worse than no message.
                Ok((addr, exposure)) => {
                    if !ui.pending_web_access_open {
                        ui.message(web_started_message(addr, exposure));
                    }
                }
                Err(e) => {
                    ui.pending_web_access_open = false;
                    ui.message(format!("Web interface did not start: {e}"));
                }
            }
        }
        if ui.pending_web_stop {
            ui.pending_web_stop = false;
            if web_surface.running() {
                web_surface.stop(crate::web::server::ShutdownNotice::server_stopped());
                ui.web_access = None;
                ui.message("Web interface stopped.".to_string());
            }
        }
        // A revocation is two halves, and this is the one the listener owns: the
        // key handler took the credentials away, and this closes the sockets
        // that were still using them (§6.5 R20). Nothing is reported back — the
        // overlay's own notice already says what was revoked, and a second
        // sentence from here would be the desktop congratulating itself.
        if std::mem::take(&mut ui.pending_web_recheck_credentials) {
            if let Some(handle) = web_surface.handle.as_ref() {
                handle.recheck_credentials();
            }
        }

        // --- The access overlay (D5, Q1; design 2a). --------------------------
        //
        // Everything the overlay cannot do for itself happens here, because
        // everything it cannot do for itself needs the listener: rebinding
        // between its two states, and knowing what address was actually bound.
        // The store handle is republished every tick so the key handler can
        // mint against exactly the store the server verifies against, and is
        // withdrawn the moment there is no server — an overlay describing a
        // binding that no longer exists is the thing this whole surface is
        // meant to prevent.
        ui.web_credentials = web_surface
            .running()
            .then(|| Arc::clone(&web_surface.credentials));
        if let Some(bind) = ui.pending_web_rebind.take() {
            rebind_web_interface(
                workspace,
                web_surface,
                ui,
                &self.interfaces,
                env.clock,
                now_ms,
                bind,
            );
        }
        if std::mem::take(&mut ui.pending_web_access_open) {
            open_web_access_overlay(web_surface, ui, &self.interfaces);
        }
        #[cfg(debug_assertions)]
        if let Some(digits) = self.web_test_code.as_deref() {
            web_surface.ensure_test_bootstrap_code(digits);
        }
        // Rebuilt every tick, which is what makes the countdown move without the
        // renderer touching a credential — the same contract the phone pairing
        // overlay has with `remote_pairing_view`.
        refresh_web_access_overlay(web_surface, ui);
        ui.web_running = web_surface.running();

        // --- The input lock (D14 as revised). ---------------------------------
        //
        // The desktop is one of the writers, so it holds the same lock every
        // browser does and reads it from the same place. Three things happen
        // here, all once per tick:
        //
        //   1. The handle is picked up (or dropped) as the server starts and
        //      stops, so `write_active_pty` has something to claim through —
        //      and, when the server is stopped, deliberately does not.
        //   2. `sync_input_lock` retires a holder that has gone quiet. Nobody
        //      *causes* an expiry, so without a tick nothing would announce it
        //      and both surfaces would keep naming somebody who stopped typing.
        //   3. The palette's explicit override is applied, and only here: it is
        //      the one act that may cut into a live burst.
        match web_surface.handle.as_ref() {
            Some(handle) => {
                if ui.input_lock.is_none() {
                    ui.input_lock = Some(handle.input_lock());
                }
                if std::mem::take(&mut ui.pending_input_preempt) {
                    let message = match handle.preempt_input_for_desktop(now_ms as i64) {
                        Some(interrupted) => {
                            format!("Input lock taken from {interrupted}.")
                        }
                        // Nobody was mid-burst. Say what happened rather than
                        // implying somebody was interrupted.
                        None => "Input lock held by this desktop.".to_string(),
                    };
                    ui.message(message);
                }
                handle.sync_input_lock(now_ms as i64);
                // Named only while somebody else could be typing: with no
                // browser seated as a writer the chip would be permanent noise
                // about a contest that cannot happen.
                ui.input_holder = web_input_holder(handle);
            }
            None => {
                ui.pending_input_preempt = false;
                ui.input_lock = None;
                ui.input_holder = None;
            }
        }

        // A browser answering a dialog (the New Agent form, say) queues its
        // worktree job from inside this phase, with no desktop input to follow
        // it. Hand it off now rather than leaving it parked until somebody
        // touches the keyboard; a desktop-queued job has already gone through
        // `after_input`, so this is an empty drain for it.
        self.flush_pending_jobs();

        changed
    }

    /// Feed one front-end-neutral input. Hands off any worktree job the input
    /// queued before returning, so the caller need not call
    /// [`AppHost::after_input`] as well. Any handled input warrants a redraw.
    ///
    /// An `Err` is either a dispatch failure the TUI would also have hit, or an
    /// [`OverlayInput`] that does not fit the overlay on screen (a button the
    /// dialog does not show, a palette row it does not offer); the latter
    /// changes nothing.
    pub fn handle(&mut self, event: HostEvent) -> Result<()> {
        // The same function the TUI's key map resolves into (`handle_key`), so
        // the two front-ends share one meaning per action.
        let result = apply_host_event(event, &mut self.workspace, &self.env, &mut self.ui);
        self.after_input();
        result
    }

    /// Housekeeping after the front-end handled an input itself (its own key
    /// map, a click): hand off queued worktree-creation jobs to the owning
    /// project's background worker so `git worktree add` never blocks the loop.
    pub fn after_input(&mut self) {
        self.flush_pending_jobs();
    }

    fn flush_pending_jobs(&mut self) {
        for pj in self.ui.pending_jobs.drain(..) {
            if let Some(p) = self.workspace.projects.get(pj.project) {
                spawn_worktree_job(pj.job, &p.git, &p.git_lock, &p.create_tx);
            }
        }
    }

    /// Resize every project's sessions — not only the active one — so a
    /// background agent's output wraps correctly the moment the user switches
    /// back to it. `viewport` maps a project's state to its PTY viewport size,
    /// because only the front-end knows its chrome (sidebar, borders, collapsed
    /// bars) and that can differ per project and per input mode.
    pub fn resize_projects(&mut self, viewport: impl Fn(&AppState) -> PtySize) {
        resize_workspace(&mut self.workspace, viewport);
    }

    /// Record every project's PTY size without resizing any live session —
    /// the seed a front-end applies before [`AppHost::resume_launch_project`]
    /// so agents spawn at the right width.
    pub fn seed_pty_sizes(&mut self, viewport: impl Fn(&AppState) -> PtySize) {
        for p in self.workspace.projects.iter_mut() {
            let size = viewport(&p.state);
            p.state.set_pty_size(size);
        }
    }

    /// A config file the user asked to open in `$EDITOR` (SPECS §8), if any.
    /// Taking it is the front-end's cue to suspend itself, run the editor, and
    /// then call [`AppHost::reload_config`].
    pub fn take_pending_editor(&mut self) -> Option<PathBuf> {
        self.ui.pending_editor.take().map(|(_project, path)| path)
    }

    /// Show a one-line notification dialog.
    pub fn message(&mut self, msg: impl Into<String>) {
        self.ui.message(msg);
    }

    /// Reload every open project's effective config (after an external edit).
    pub fn reload_config(&mut self) {
        reload_all_projects_config(&mut self.workspace, &self.env);
    }

    // -----------------------------------------------------------------------
    // Reading state back (for rendering)
    // -----------------------------------------------------------------------

    /// Whether a dispatched quit (a key, the palette, a confirmed dialog) asked
    /// the app to exit.
    pub fn should_quit(&self) -> bool {
        self.ui.should_quit
    }

    /// The instant the current turn runs at (read by [`AppHost::pump`]); render
    /// with this so the frame agrees with the state it shows.
    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    /// Whether this is an `--isolated` run (SPECS §32).
    pub fn is_isolated(&self) -> bool {
        self.isolated
    }

    /// Number of open projects (always at least one).
    pub fn project_count(&self) -> usize {
        self.workspace.projects.len()
    }

    /// Index of the active (on-screen) project.
    pub fn active_project_index(&self) -> usize {
        self.workspace.active
    }

    /// A project's display name (its repository folder name).
    pub fn project_name(&self, index: usize) -> Option<&str> {
        self.workspace.projects.get(index).map(|p| p.name.as_str())
    }

    /// A project's repository root.
    pub fn project_root(&self, index: usize) -> Option<&Path> {
        self.workspace.projects.get(index).map(|p| p.git.root())
    }

    /// A project's application state.
    pub fn project_state(&self, index: usize) -> Option<&AppState> {
        self.workspace.projects.get(index).map(|p| &p.state)
    }

    /// A project's git-status cache, keyed by tab id (SPECS §21).
    pub fn git_status(&self, index: usize) -> Option<&HashMap<String, WorktreeStatus>> {
        self.workspace.projects.get(index).map(|p| &p.cache)
    }

    /// The active project's application state.
    pub fn active_state(&self) -> &AppState {
        &self.workspace.active_project().state
    }

    /// The active project's application state, mutably — for front-end work
    /// that is state-shaped but layout-driven (syncing terminal sizes to split
    /// columns, scrolling a selection drag).
    pub fn active_state_mut(&mut self) -> &mut AppState {
        &mut self.workspace.active_project_mut().state
    }

    /// Whether the active project's terminal currently has input focus.
    pub fn terminal_focused(&self) -> bool {
        self.active_state().mode() == InputMode::Terminal
    }

    /// Who holds the web input lock, when somebody other than this desktop
    /// could be typing (D14 as revised); `None` means draw no chip.
    pub fn input_holder(&self) -> Option<&str> {
        self.ui.input_holder.as_deref()
    }

    /// The overlay on screen, if any, as plain data: the same prompt, palette,
    /// configuration manager, help, pairing or access overlay the TUI would
    /// draw this frame, in the order [`HostEvent::Overlay`] answers them.
    ///
    /// Live overlays (the pairing and access countdowns) are refreshed by
    /// [`AppHost::pump`] and [`AppHost::publish`]; read this after them.
    pub fn overlay(&self) -> Option<OverlayView> {
        crate::tui::overlay_bridge::overlay_view(
            &self.ui,
            &self.workspace,
            self.pairing_session.as_ref(),
            &self.web_surface.credentials,
        )
    }

    /// The non-modal hints the TUI keeps in its status bar (SPECS §30, §32).
    pub fn notices(&self) -> HostNotices {
        let state = self.active_state();
        HostNotices {
            update: state.update_available.as_ref().map(|latest| {
                crate::web::protocol::UpdateNotice {
                    latest_version: latest.clone(),
                }
            }),
            isolated: state.isolated,
        }
    }

    // -----------------------------------------------------------------------
    // Crate-internal: the TUI client's view of the same state
    // -----------------------------------------------------------------------

    /// The pieces the TUI's own input handlers (key map, mouse hit-testing,
    /// the renderer) work on directly. Crate-internal: those handlers speak
    /// terminal-UI types, and an external front-end goes through
    /// [`AppHost::handle`] and the accessors above instead.
    pub(crate) fn tui_parts(&mut self) -> (&mut Workspace, &Env<'a>, &mut Ui) {
        (&mut self.workspace, &self.env, &mut self.ui)
    }

    /// The workspace and dialog state, read-only (for rendering).
    pub(crate) fn view(&self) -> (&Workspace, &Ui) {
        (&self.workspace, &self.ui)
    }
}

/// Every tab's display-ready lifecycle state across the workspace, cheap
/// enough to take twice a turn: a status file or a finalized worktree moves
/// it without any byte of PTY output, and [`AppHost::pump`] must still say
/// "redraw" for those.
fn status_fingerprint(
    workspace: &Workspace,
    now_ms: u64,
) -> Vec<(usize, crate::contracts::InterpretedStatus)> {
    workspace
        .projects
        .iter()
        .enumerate()
        .flat_map(|(i, p)| {
            p.state
                .tabs
                .iter()
                .map(move |t| (i, t.display_status(now_ms).interpreted))
        })
        .collect()
}
