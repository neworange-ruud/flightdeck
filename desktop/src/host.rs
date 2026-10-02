//! The [`AppHost`] as a GPUI entity: the one owner of the workspace, driven
//! from GPUI's main-thread executor.
//!
//! ## Lifetimes and threads
//!
//! `AppHost<'a>` borrows its services and is `!Send`. A GPUI entity must be
//! `'static` but may be `!Send` (entities live on the main thread), so the
//! app builds its real services once and leaks them ([`RealServices::leak`]):
//! one allocation for the life of the process, exactly the lifetime the TUI's
//! stack-held services have in `flightdeck::run`. The host then is an
//! `AppHost<'static>` held in a [`HostModel`], and every view reads it through
//! the entity, on the main thread.
//!
//! ## The turn loop
//!
//! [`HostModel::start_ticking`] spawns a foreground task that runs one
//! [`HostModel::turn`] after each sleep: apply a pending viewport size,
//! `AppHost::tick` (pump every project's PTYs and workers, then publish to the
//! web and the relay), and redraw when something on screen changed. A quit
//! asked for by the host (Ctrl-q, a confirmed dialog) or by a signal ends the
//! app from here.
//!
//! How long it sleeps is the terminal element's [`Cadence`]
//! (desktop/NOTES-M0.md, "Performance (S4)"): every millisecond for a moment
//! after terminal input, so an echo is read in time for the next frame; every
//! [`HOST_ACTIVE_TURN`] (16 ms) while output flows; every [`HOST_IDLE_TURN`]
//! (50 ms, the TUI's loop) when idle.
//! Terminal input restarts the sleep, so the fast turns begin at the
//! keystroke.
//!
//! "Something changed" is the host's own `tick` result (PTY output, worker
//! results, status changes, …) or a change in the time-derived text the
//! views show (an agent's `done · 3m`, a pairing countdown), which the model
//! checks every [`CLOCK_CHECK`] through [`HostModel::clock_signature`] rather
//! than redrawing on a timer. An idle app therefore draws no frames at all;
//! [`SAFETY_REDRAW`] only guards against a time-derived value the signature
//! does not know about.
//!
//! ## Teardown
//!
//! [`HostModel::teardown`] runs the TUI's order — stop services, persist,
//! terminate sessions, remove an isolated run's temp directory — once, from
//! GPUI's quit hook (window close, Cmd-Q, Ctrl-q and SIGTERM/SIGINT all quit
//! through it).
//!
//! ## Attention
//!
//! When the app asks ([`HostModel::report_attention`]), each turn also reads
//! `AppHost::needs_you_count` and hands it to [`crate::notify`], which keeps
//! the Dock badge current and asks the OS to flag a background window when a
//! new agent starts waiting. The banner and sound are the host's own
//! (`SystemNotifier`, the TUI's).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use flightdeck::contracts::real::{RealClock, RealFs, SystemCommandRunner};
use flightdeck::contracts::PtySize;
use flightdeck::host::{AppHost, HostEvent};
use flightdeck::notify::SystemNotifier;
use flightdeck::runtime::PodmanCli;
use flightdeck::terminal::pty::PortablePtyBackend;
use flightdeck::Env;
use gpui::{Context, Task};

use crate::notify::{self, AttentionState};
use crate::terminal::cadence::Cadence;
use crate::views::split::{PaneSizes, SplitLayout};
use flightdeck::view::TerminalRef;

/// How often the host turns while output flows: its turn services every
/// project, and output anywhere keeps it active, so it runs at the display's
/// 60 Hz rather than a single terminal's 8 ms. Echoes still get the 1 ms
/// turns right after input.
pub const HOST_ACTIVE_TURN: Duration = Duration::from_millis(16);

/// How often the host turns when nothing is happening: the TUI's own loop, so
/// output an agent starts on its own is read as soon as the TUI would read it,
/// and an idle app costs what an idle TUI does.
pub const HOST_IDLE_TURN: Duration = Duration::from_millis(50);

/// How often the time-derived text on screen is checked for a change (see the
/// module docs). Elapsed times and countdowns move in whole seconds.
pub const CLOCK_CHECK: Duration = Duration::from_millis(250);

/// Redraw at least this often even when nothing seems to have changed: a
/// backstop for time-derived display [`HostModel::clock_signature`] does not
/// cover, far too rare to cost anything.
pub const SAFETY_REDRAW: Duration = Duration::from_secs(30);

/// The real services the host borrows, leaked to `'static` (see the module
/// docs). Built once, by the app's start-up.
pub struct RealServices {
    fs: RealFs,
    pty: PortablePtyBackend,
    clock: RealClock,
    container: PodmanCli,
    command: SystemCommandRunner,
    notifier: SystemNotifier,
}

impl RealServices {
    /// The process's services, for its whole life.
    pub fn leak() -> &'static RealServices {
        Box::leak(Box::new(RealServices {
            fs: RealFs,
            pty: PortablePtyBackend,
            clock: RealClock,
            container: PodmanCli,
            command: SystemCommandRunner,
            notifier: SystemNotifier,
        }))
    }

    /// The service bundle `AppHost::open` takes.
    pub fn env(&'static self) -> Env<'static> {
        Env {
            fs: &self.fs,
            pty: &self.pty,
            clock: &self.clock,
            container: &self.container,
            command: &self.command,
            terminal: crate::terminal::desktop_profile(),
        }
    }

    /// The OS notification backend.
    pub fn notifier(&'static self) -> &'static SystemNotifier {
        &self.notifier
    }
}

/// The host entity. See the module docs.
pub struct HostModel {
    host: AppHost<'static>,
    /// Set by SIGTERM/SIGINT/SIGHUP (`flightdeck::signals`); polled each turn.
    shutdown: Option<Arc<AtomicBool>>,
    /// A PTY size measured by the terminal element, applied on the next turn
    /// (never from inside a paint).
    pending_viewport: Option<PtySize>,
    /// The size every project's terminals were last resized to.
    viewport: Option<PtySize>,
    /// Split view: the size each of the selected agent's terminals measured
    /// in its own pane, applied on the next turn ([`HostModel::set_pane_size`]).
    pane_sizes: PaneSizes,
    /// When the last redraw was asked for, for [`SAFETY_REDRAW`].
    last_redraw: Instant,
    /// When the time-derived display was last checked, and what it read.
    last_clock_check: Instant,
    clock_signature: u64,
    /// Terminal input and output times, which set the turn rate.
    cadence: Cadence,
    torn_down: bool,
    /// The needs-you count last shown on the app icon; `None` until the app
    /// asks for it ([`HostModel::report_attention`]), so tests never touch the
    /// developer's Dock.
    attention: Option<AttentionState>,
    /// Every event dispatched, in order — what the tests compare a button
    /// with its chord by.
    #[cfg(test)]
    pub dispatched: Vec<HostEvent>,
    /// Every front-end row the host handed back, in order, instead of opening
    /// a window or a process from a test.
    #[cfg(test)]
    pub performed: Vec<crate::menus::AppCommand>,
    _ticker: Option<Task<()>>,
}

impl HostModel {
    /// Wrap an opened (not yet started) host. Call [`HostModel::start_ticking`]
    /// to drive it; tests drive [`HostModel::turn`] themselves.
    pub fn new(host: AppHost<'static>) -> HostModel {
        HostModel {
            host,
            shutdown: None,
            pending_viewport: None,
            viewport: None,
            pane_sizes: PaneSizes::default(),
            last_redraw: Instant::now(),
            last_clock_check: Instant::now(),
            clock_signature: 0,
            cadence: Cadence::with_rates(HOST_ACTIVE_TURN, HOST_IDLE_TURN),
            torn_down: false,
            attention: None,
            #[cfg(test)]
            dispatched: Vec::new(),
            #[cfg(test)]
            performed: Vec::new(),
            _ticker: None,
        }
    }

    /// The host, for reading state back.
    pub fn host(&self) -> &AppHost<'static> {
        &self.host
    }

    /// The host, mutably — for layout-driven state edits no event spells
    /// (focusing the primary terminal of the selected agent).
    pub fn host_mut(&mut self) -> &mut AppHost<'static> {
        &mut self.host
    }

    /// Watch this flag for a shutdown signal.
    pub fn set_shutdown_flag(&mut self, flag: Arc<AtomicBool>) {
        self.shutdown = Some(flag);
    }

    /// Show the needs-you count on the app icon and ask the OS to draw
    /// attention to the window when someone new is waiting
    /// ([`crate::notify`]), every turn from now on.
    pub fn report_attention(&mut self) {
        self.attention = Some(AttentionState::default());
    }

    /// Spawn the turn loop on GPUI's foreground executor (restarting it if
    /// it runs, which cuts short the current sleep).
    pub fn start_ticking(&mut self, cx: &mut Context<Self>) {
        self._ticker = Some(cx.spawn(async move |this, cx| loop {
            let delay = this.update(cx, |model, _| {
                model.cadence.next_delay(Instant::now(), false)
            });
            let Ok(delay) = delay else {
                break;
            };
            cx.background_executor().timer(delay).await;
            if this.update(cx, |model, cx| model.turn(cx)).is_err() {
                break;
            }
        }));
    }

    /// A fingerprint of the time-derived text the views show: every agent
    /// row's badge and elapsed time as the sidebar words it, and the open
    /// overlay (a pairing code's countdown lives there). A redraw is due when
    /// it changes.
    pub fn clock_signature(&self) -> u64 {
        let host = &self.host;
        let mut hasher = DefaultHasher::new();
        for i in 0..host.project_count() {
            let (Some(state), Some(git)) = (host.project_state(i), host.git_status(i)) else {
                continue;
            };
            for row in
                flightdeck::view::agent_row_views(state, git, host.now_ms(), host.now_unix_secs())
            {
                format!("{:?}", row.badge).hash(&mut hasher);
                row.status_since_secs
                    .map(flightdeck::view::format_elapsed)
                    .hash(&mut hasher);
            }
        }
        format!("{:?}", host.overlay()).hash(&mut hasher);
        hasher.finish()
    }

    /// One turn: see the module docs. Returns whether it redrew.
    pub fn turn(&mut self, cx: &mut Context<Self>) -> bool {
        if self.torn_down {
            return false;
        }
        if let Some(size) = self.pending_viewport.take() {
            if self.viewport != Some(size) {
                self.viewport = Some(size);
                self.host.resize_projects(|_| size);
            }
        }
        self.sync_terminal_sizes();
        let changed = self.host.tick();
        let now = Instant::now();
        if changed {
            self.cadence.output(now);
        }
        if let Some(attention) = self.attention.as_mut() {
            notify::apply(attention, self.host.needs_you_count(), cx);
        }
        let mut clock_moved = false;
        if now.duration_since(self.last_clock_check) >= CLOCK_CHECK {
            self.last_clock_check = now;
            let signature = self.clock_signature();
            clock_moved = signature != self.clock_signature;
            self.clock_signature = signature;
        }
        let redraw =
            changed || clock_moved || now.duration_since(self.last_redraw) >= SAFETY_REDRAW;
        if redraw {
            self.last_redraw = now;
            cx.notify();
        }
        let signalled = self
            .shutdown
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed));
        if self.host.should_quit() || signalled {
            cx.quit();
        }
        redraw
    }

    /// Handle one input, exactly as the TUI's key map would. A failure is
    /// shown the way the TUI's own handlers show theirs (`Error: …`) rather
    /// than ending the app; an overlay input that no longer fits the overlay
    /// on screen (a click racing a close) changes nothing and is only logged.
    pub fn dispatch(&mut self, event: HostEvent, cx: &mut Context<Self>) {
        #[cfg(test)]
        self.dispatched.push(event.clone());
        // Terminal input: turn fast for the echo, starting now.
        if matches!(event, HostEvent::TerminalInput(_) | HostEvent::Paste(_)) {
            self.cadence.input(Instant::now());
            if self._ticker.is_some() {
                self.start_ticking(cx);
            }
        }
        let is_overlay = matches!(event, HostEvent::Overlay(_));
        if let Err(e) = self.host.handle(event) {
            if is_overlay {
                eprintln!("flightdeck-desktop: overlay input refused: {e}");
            } else {
                self.host.message(format!("Error: {e}"));
            }
        }
        // Palette rows only the window can perform (Connect to Remote, New
        // Window): the host queued them; open them once this update is done.
        for action in self.host.take_front_end_actions() {
            let command = crate::menus::AppCommand::for_front_end(action);
            #[cfg(test)]
            self.performed.push(command);
            #[cfg(not(test))]
            cx.defer(move |cx| command.perform(cx));
        }
        cx.notify();
        if self.host.should_quit() {
            cx.quit();
        }
    }

    /// The terminal element measured `size`: every project's terminals are
    /// to be that size (applied next turn). In split view the element sits in
    /// the active terminal's pane, so the size is that pane's alone
    /// ([`HostModel::set_pane_size`]) and the viewport stays as it was.
    pub fn set_viewport(&mut self, size: PtySize) {
        if let Some(layout) = SplitLayout::read(&self.host) {
            let (project, tab_id, active) = (layout.project, layout.tab_id.clone(), layout.active);
            self.set_pane_size(project, &tab_id, active, size);
            return;
        }
        if self.viewport != Some(size) {
            self.pending_viewport = Some(size);
        }
    }

    /// Split view: the pane showing `target` of Agent Tab `tab_id` in
    /// `project` measured `size`. Applied to that terminal alone on the next
    /// turn; ignored when that tab is not the one split view shows (a pane
    /// racing a tab switch).
    pub fn set_pane_size(
        &mut self,
        project: usize,
        tab_id: &str,
        target: TerminalRef,
        size: PtySize,
    ) {
        let Some(layout) = SplitLayout::read(&self.host) else {
            return;
        };
        if layout.project != project || layout.tab_id != tab_id {
            return;
        }
        self.pane_sizes.record(&layout.key(), target, size);
    }

    /// The TUI's `sync_terminal_sizes`, per turn: in split view each of the
    /// selected agent's terminals gets the size its pane measured; otherwise
    /// every terminal of the selected agent gets the viewport (which also
    /// undoes the column sizes when split view is turned off). Only the
    /// selected agent is on screen, so only it is synced; another agent left
    /// in split view gets the viewport when the window next resizes, and its
    /// own pane sizes when it is selected again. Each resize happens only
    /// when the size differs ([`AppHost::resize_terminal`]).
    fn sync_terminal_sizes(&mut self) {
        match SplitLayout::read(&self.host) {
            Some(layout) => {
                let sizes: Vec<_> = self.pane_sizes.for_key(&layout.key()).collect();
                for (target, size) in sizes {
                    self.host
                        .resize_terminal(layout.project, &layout.tab_id, target, size);
                }
            }
            None => {
                let Some(size) = self.viewport else {
                    return;
                };
                let project = self.host.active_project_index();
                let Some(tab) = self.host.active_state().selected() else {
                    return;
                };
                let tab_id = tab.meta.id.clone();
                let children = tab.session.child_count();
                let targets = std::iter::once(TerminalRef::Primary)
                    .chain((0..children).map(TerminalRef::Child));
                for target in targets {
                    self.host.resize_terminal(project, &tab_id, target, size);
                }
            }
        }
    }

    /// The TUI's clean teardown (SPECS §25), once. Safe to call again.
    pub fn teardown(&mut self) {
        if std::mem::replace(&mut self.torn_down, true) {
            return;
        }
        self.host.stop_services();
        if let Err(e) = self.host.persist() {
            eprintln!("flightdeck-desktop: could not save state: {e}");
        }
        self.host.terminate_sessions();
        self.host.cleanup_isolated();
    }
}
