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
//! [`HostModel::start_ticking`] spawns a foreground task that wakes every
//! [`TICK_INTERVAL`] and runs one [`HostModel::turn`]: apply a pending
//! viewport size, `AppHost::tick` (pump every project's PTYs and workers, then
//! publish to the web and the relay), and redraw when the host says something
//! changed — or at least every [`COARSE_REDRAW`], because elapsed times and
//! countdowns move without any event. A quit asked for by the host (Ctrl-q, a
//! confirmed dialog) or by a signal ends the app from here.
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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use flightdeck::contracts::real::{RealClock, RealFs, SystemCommandRunner};
use flightdeck::contracts::PtySize;
use flightdeck::host::{AppHost, HostEvent};
use flightdeck::notify::SystemNotifier;
use flightdeck::runtime::PodmanCli;
use flightdeck::terminal::pty::PortablePtyBackend;
use flightdeck::Env;
use gpui::{Context, Task};

use crate::notify::{self, AttentionState};

/// How often the host turns. The TUI polls every 50 ms when idle; 16 ms keeps
/// agent output within a frame of arriving.
pub const TICK_INTERVAL: Duration = Duration::from_millis(16);

/// Redraw at least this often even when nothing arrived, for elapsed times
/// (`working · 2m`) and countdowns.
pub const COARSE_REDRAW: Duration = Duration::from_secs(1);

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
    /// Turns since the last redraw, for [`COARSE_REDRAW`].
    quiet_turns: u32,
    torn_down: bool,
    /// The needs-you count last shown on the app icon; `None` until the app
    /// asks for it ([`HostModel::report_attention`]), so tests never touch the
    /// developer's Dock.
    attention: Option<AttentionState>,
    /// Every event dispatched, in order — what the tests compare a button
    /// with its chord by.
    #[cfg(test)]
    pub dispatched: Vec<HostEvent>,
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
            quiet_turns: 0,
            torn_down: false,
            attention: None,
            #[cfg(test)]
            dispatched: Vec::new(),
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

    /// Spawn the turn loop on GPUI's foreground executor.
    pub fn start_ticking(&mut self, cx: &mut Context<Self>) {
        self._ticker = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(TICK_INTERVAL).await;
            if this.update(cx, |model, cx| model.turn(cx)).is_err() {
                break;
            }
        }));
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
        let changed = self.host.tick();
        if let Some(attention) = self.attention.as_mut() {
            notify::apply(attention, self.host.needs_you_count(), cx);
        }
        self.quiet_turns += 1;
        let coarse =
            self.quiet_turns as u128 * TICK_INTERVAL.as_millis() >= COARSE_REDRAW.as_millis();
        let redraw = changed || coarse;
        if redraw {
            self.quiet_turns = 0;
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
        let is_overlay = matches!(event, HostEvent::Overlay(_));
        if let Err(e) = self.host.handle(event) {
            if is_overlay {
                eprintln!("flightdeck-desktop: overlay input refused: {e}");
            } else {
                self.host.message(format!("Error: {e}"));
            }
        }
        cx.notify();
        if self.host.should_quit() {
            cx.quit();
        }
    }

    /// Ask for every project's terminals to be `size` (applied next turn).
    pub fn set_viewport(&mut self, size: PtySize) {
        if self.viewport != Some(size) {
            self.pending_viewport = Some(size);
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
