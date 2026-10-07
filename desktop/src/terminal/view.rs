//! The terminal view: draws one terminal's grid and routes input to it.
//!
//! Where the terminal lives is the view's [`TerminalSource`]:
//!
//! - **Owned** (the `--spike-terminal` window and `--bench`): the view owns a
//!   [`Terminal`] and polls it. A foreground task drains the PTY into the grid
//!   through [`super::pump`] and asks for a frame only when the screen changed
//!   (new bytes, or a synchronized update the emulator released); how often it
//!   polls is [`super::cadence`]'s call (every millisecond just after input,
//!   every 8 ms while output flows, every 25 ms when idle). Keys go through the
//!   spike's small encoder ([`super::input`]).
//! - **Host** (the app): the terminal on screen is the host's — the active
//!   project's selected agent's focused terminal
//!   ([`AppHost::active_terminal`](flightdeck::host::AppHost::active_terminal)).
//!   The host's own turn drains every PTY, so the view polls nothing, and it
//!   redraws when the host notifies, which the host does only when something
//!   changed (see `crate::host`). Input is
//!   the app's: bytes go to the host as `HostEvent::TerminalInput` (so the web
//!   input lock applies, as for the TUI), keys through the keymap-aware
//!   [`terminal_key_down`] (the TUI's bytes, byte for byte), and committed text
//!   — IME compositions included — through [`ImeState`] behind GPUI's
//!   `EntityInputHandler`. The grid size is measured by the element and
//!   handed to the host, which resizes every project's PTYs on its next turn
//!   (the desktop owns PTY geometry; FlightDeck Web letterboxes the size the
//!   grid then has, specs/WEB_INTERFACE.md D4).
//!
//! In both, the element redraws only the rows the emulator reports changed
//! ([`super::rowcache`], fed by `TerminalGrid::damage_since`), and nothing
//! repaints an idle terminal: there is no periodic repaint. GPUI draws at most
//! one frame per display refresh however often a view notifies, so output
//! arriving over many polls lands in one frame.
//!
//! **The mouse** follows the TUI (`handle_mouse_project` in `src/lib.rs`):
//!
//! - When the program asked for mouse reporting, presses, releases and the
//!   motion its mode asks for are forwarded, encoded by the core's
//!   `encode_mouse_button` / `encode_mouse_report` (the TUI's encoders, so the
//!   bytes are the same). Shift overrides a press: Shift-drag selects even
//!   over a mouse-driven app.
//! - Otherwise a left drag selects. The selection is copied to the system
//!   clipboard on release and stays highlighted; a click that selected nothing
//!   clears it. A drag held past the top or bottom edge scrolls the history a
//!   line every [`AUTOSCROLL_EVERY`] and extends the selection into it. The
//!   drag keeps following the pointer outside the element and the window (the
//!   element registers window-level listeners, see `super::element`).
//! - The wheel goes to the program when it has mouse reporting on (a wheel
//!   report per notch, as the TUI sends); otherwise it scrolls the history,
//!   [`SCROLL_LINES`] per notch as in the TUI, and trackpad pixels accumulate
//!   into whole lines. On an alternate screen without reporting it does
//!   nothing, as in the TUI (there is no history there). A grid larger than
//!   the element — a remote host's, which this window may not resize — is
//!   panned first: the wheel reaches its hidden rows before the history, a
//!   sideways swipe its hidden columns, and a typed key brings the cursor
//!   back into view (see [`super::pan`]).
//! - **Links.** Holding the platform's secondary modifier (Cmd on macOS, Ctrl
//!   elsewhere) underlines the web link under the pointer and shows a hand
//!   cursor; a click with it held opens the link in the browser instead of
//!   selecting or reaching the program. Links are read off the text
//!   ([`flightdeck::terminal::grid::links`]), wrapped rows included, and only
//!   `http(s)` ever opens.
//! - Cmd-C on macOS copies the selection too (release already has). Input —
//!   a key, a paste — drops the selection and returns to the live screen (the
//!   host does that for the app; the owned path does it here).
//!
//! **The cursor** is the program's DECSCUSR shape: a filled block, an
//! underline or a bar; a hollow block when this terminal does not have key
//! focus or the window is not the active one. It blinks only when the program
//! asked for a blinking cursor, the terminal is focused and the window
//! active: [`BLINK_INTERVAL`] a phase, so at most 2 frames a second, and none
//! at all otherwise (the S4 idle budget).
//!
//! **Size and dimming.** The text size is `[ui] desktop_terminal_font_size`
//! plus the session zoom (Cmd +/-/0 on macOS, see [`super::zoom`]). In the app
//! the terminal dims in APP mode when `[ui] dim_terminal_in_app_mode` is on,
//! exactly the TUI's rule (`dim_terminal` in `src/tui/render.rs`).

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use flightdeck::terminal::grid::links::{link_at, Link};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, Bounds, ClipboardItem, Context, Entity, EntityInputHandler, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, Modifiers, ModifiersChangedEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render,
    ScrollDelta, ScrollWheelEvent, ShapedLine, Styled, Task, UTF16Selection, Window,
};

use flightdeck::app::keymap::encode_paste;
use flightdeck::contracts::{ProcessState, PtySize, UiConfig};
use flightdeck::host::HostEvent;
use flightdeck::terminal::grid::MouseMode;
use flightdeck::terminal::session::Terminal;
use flightdeck::tui::platform;
use flightdeck_desktop::keys::{terminal_key_down, ImeState, KeymapAction, OptionKey, TerminalKey};

use super::bench::Probe;
use super::cadence::Cadence;
use super::element::{CellMetrics, TerminalElement};
use super::input;
use super::layout::{self, CellSpan, TermPalette};
use super::pan::{self, Pan, Viewport};
use super::rowcache::RowCache;
use super::zoom::{TerminalZoom, ZoomChord};
use crate::host::HostModel;
use crate::remote::RemoteModel;
use crate::theme::{Hex, Palette};

/// History rows one wheel notch scrolls: the TUI's `SCROLL_LINES`.
pub const SCROLL_LINES: usize = 3;
/// How often a drag held past an edge scrolls, a line at a time: the TUI's
/// loop tick, which is also its auto-scroll step.
pub const AUTOSCROLL_EVERY: Duration = Duration::from_millis(50);
/// One phase of a blinking cursor (on, then off): 2 phases, so at most 2
/// frames, a second.
pub const BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// Where the terminal a [`TerminalView`] draws lives.
pub enum TerminalSource {
    /// The view owns it (the spike window).
    Owned(Terminal),
    /// The host owns it: whichever terminal is on screen in the active
    /// project (the app).
    Host(Entity<HostModel>),
    /// A FlightDeck on another machine owns it: the terminal a remote window
    /// is showing, mirrored over the link (`crate::remote`). It draws and takes
    /// keys exactly as the host's does; only where the bytes go differs.
    Remote(Entity<RemoteModel>),
}

/// A local selection drag in progress.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Drag {
    /// Where the pointer last was, in window coordinates (it may be outside
    /// the grid: that is what drives auto-scroll).
    position: Point<Pixels>,
    /// The cell the selection head was last put on, so a move within one
    /// cell repaints nothing.
    cell: (u16, u16),
}

/// Which way a drag past an edge scrolls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Top,
    Bottom,
}

pub struct TerminalView {
    source: TerminalSource,
    focus_handle: FocusHandle,
    palette: TermPalette,
    metrics: Option<CellMetrics>,
    /// The font size the row cache's shaped text was made at.
    shaped_at: Option<f32>,
    /// Laid-out and shaped rows from earlier frames (see [`super::rowcache`]).
    /// The element borrows it for each frame.
    row_cache: RowCache<Vec<ShapedLine>>,
    /// The button held while forwarding a drag to a mouse-aware program.
    forwarded_button: Option<u8>,
    /// The cell the last motion report was for (see `new_motion_cell`).
    motion_cell: Option<(u16, u16)>,
    /// A local selection drag in progress.
    drag: Option<Drag>,
    /// Where the pointer is over the grid (window coordinates), when it is.
    pointer: Option<Point<Pixels>>,
    /// Whether the secondary modifier (Cmd / Ctrl) is held.
    link_modifier: bool,
    /// The link under the pointer while the modifier is held: underlined, and
    /// what a click opens.
    hover_link: Option<Link>,
    /// The auto-scroll loop while a drag is held past an edge, and whether it
    /// is still running (it ends itself once the drag ends or comes back).
    autoscroll: Option<Task<()>>,
    autoscrolling: bool,
    /// Fractional wheel notches (or trackpad lines) not yet acted on.
    scroll_remainder: f32,
    /// Whether that remainder is trackpad lines (else wheel notches).
    scroll_precise: bool,
    /// Fractional sideways wheel notches (or trackpad columns) not yet acted on.
    scroll_remainder_x: f32,
    /// How far a grid larger than the element is panned (see [`super::pan`]),
    /// for the grid it was panned on: a different terminal on screen starts
    /// from the bottom-left again.
    pan: Pan,
    pan_grid: Option<usize>,
    /// What the last frame showed of the grid.
    viewport: Viewport,
    /// Bring the cursor into view on the next frame (a key was typed).
    reveal_cursor: bool,
    exited: Option<ProcessState>,
    /// The composition in progress (Host source; the spike types keys).
    ime: ImeState,
    /// Whether the window was the active one at the last frame.
    window_active: bool,
    /// The window-activation observer is registered (on the first render,
    /// which is the first time the view has its window).
    observing_activation: bool,
    /// Blinking: whether the last frame wanted a blinking cursor, the phase
    /// (`true` = drawn), and the loop that flips it.
    blink_wanted: bool,
    blink_on: bool,
    blink: Option<Task<()>>,
    blinking: bool,
    /// Timestamps for `--bench` (see [`super::bench`]); `None` outside a bench
    /// run, so the hooks cost one branch each.
    probe: Option<Rc<RefCell<Probe>>>,
    /// Owned source: when the view last wrote input and last saw output,
    /// which sets the polling rate.
    cadence: Cadence,
    /// Owned source: PTY bytes read but not parsed yet (see
    /// [`super::PARSE_BUDGET`]).
    backlog: Vec<u8>,
    /// Owned source: poll again soon regardless of the cadence (a backlog is
    /// waiting, or the emulator holds a synchronized update).
    busy: bool,
    /// Owned source: the poll loop.
    poll: Option<Task<()>>,
}

impl TerminalView {
    /// A view owning `terminal` and polling it (the spike window).
    pub fn new(mut terminal: Terminal, cx: &mut Context<Self>) -> Self {
        let palette = TermPalette::from_palette(Palette::global(cx));
        terminal
            .screen_mut()
            .set_default_colors(channels(palette.fg), channels(palette.bg));
        let mut view = Self::with_source(TerminalSource::Owned(terminal), palette, cx);
        view.poll = Some(Self::start_polling(cx));
        view
    }

    /// The owned source's poll loop. Each turn sleeps for what
    /// [`Cadence::next_delay`] says, then runs [`Self::poll_once`]; it ends
    /// with the process. Restarted on input (see [`Self::write`]) so a
    /// keystroke's echo is polled for at once rather than after the rest of
    /// an idle sleep.
    fn start_polling(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| loop {
            let delay = this.update(cx, |view, _| {
                view.cadence.next_delay(Instant::now(), view.busy)
            });
            let Ok(delay) = delay else {
                break;
            };
            cx.background_executor().timer(delay).await;
            if !matches!(this.update(cx, |view, cx| view.poll_once(cx)), Ok(true)) {
                break;
            }
        })
    }

    /// Drain the owned PTY once and repaint if the screen changed. Returns
    /// whether to keep polling.
    fn poll_once(&mut self, cx: &mut Context<Self>) -> bool {
        let TerminalSource::Owned(terminal) = &mut self.source else {
            return false;
        };
        let started = Instant::now();
        let pumped = super::pump(terminal, &mut self.backlog);
        if let Some(probe) = &self.probe {
            probe
                .borrow_mut()
                .pumped(pumped.parsed, started.elapsed(), terminal.screen());
        }
        if pumped.parsed > 0 {
            self.cadence.output(Instant::now());
        }
        self.busy = pumped.backlog || terminal.screen().holds_output();
        let state = terminal.process_state();
        let ended = !matches!(state, ProcessState::Running | ProcessState::Starting);
        let newly_ended = ended && self.exited.is_none();
        if newly_ended {
            self.exited = Some(state);
        }
        if pumped.changed() || newly_ended {
            cx.notify();
        }
        !ended
    }

    /// A view of whatever terminal the host has on screen (the app). It
    /// redraws when the host does.
    pub fn for_host(host: Entity<HostModel>, cx: &mut Context<Self>) -> Self {
        let palette = TermPalette::from_palette(Palette::global(cx));
        cx.observe(&host, |_, _, cx| cx.notify()).detach();
        super::bench::log_frames_if_asked(cx);
        Self::with_source(TerminalSource::Host(host), palette, cx)
    }

    /// A view of whatever terminal a remote window shows. It redraws when the
    /// remote model does.
    pub fn for_remote(remote: Entity<RemoteModel>, cx: &mut Context<Self>) -> Self {
        let palette = TermPalette::from_palette(Palette::global(cx));
        cx.observe(&remote, |_, _, cx| cx.notify()).detach();
        Self::with_source(TerminalSource::Remote(remote), palette, cx)
    }

    fn with_source(source: TerminalSource, palette: TermPalette, cx: &mut Context<Self>) -> Self {
        // A zoom chord in any terminal resizes every terminal.
        cx.observe_global::<TerminalZoom>(|_, cx| cx.notify())
            .detach();
        Self {
            source,
            focus_handle: cx.focus_handle(),
            palette,
            metrics: None,
            shaped_at: None,
            forwarded_button: None,
            motion_cell: None,
            drag: None,
            pointer: None,
            link_modifier: false,
            hover_link: None,
            autoscroll: None,
            autoscrolling: false,
            scroll_remainder: 0.0,
            scroll_precise: false,
            scroll_remainder_x: 0.0,
            pan: Pan::default(),
            pan_grid: None,
            viewport: Viewport::default(),
            reveal_cursor: false,
            exited: None,
            ime: ImeState::default(),
            window_active: true,
            observing_activation: false,
            blink_wanted: false,
            blink_on: true,
            blink: None,
            blinking: false,
            row_cache: RowCache::default(),
            probe: None,
            cadence: Cadence::default(),
            backlog: Vec::new(),
            busy: false,
            poll: None,
        }
    }

    /// Attach a `--bench` probe (see [`super::bench`]).
    pub fn set_probe(&mut self, probe: Rc<RefCell<Probe>>) {
        self.probe = Some(probe);
    }

    pub fn probe(&self) -> Option<&Rc<RefCell<Probe>>> {
        self.probe.as_ref()
    }

    /// The owned terminal (the spike and the bench); `None` for the host's.
    pub fn owned_terminal(&self) -> Option<&Terminal> {
        match &self.source {
            TerminalSource::Owned(terminal) => Some(terminal),
            TerminalSource::Host(_) | TerminalSource::Remote(_) => None,
        }
    }

    /// The owned terminal, mutably, for the bench driver.
    pub fn owned_terminal_mut(&mut self) -> Option<&mut Terminal> {
        match &mut self.source {
            TerminalSource::Owned(terminal) => Some(terminal),
            TerminalSource::Host(_) | TerminalSource::Remote(_) => None,
        }
    }

    /// Read the terminal on screen, if there is one.
    fn with_terminal<R>(&self, cx: &gpui::App, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
        match &self.source {
            TerminalSource::Owned(terminal) => Some(f(terminal)),
            TerminalSource::Host(host) => host.read(cx).host().active_terminal().map(f),
            TerminalSource::Remote(remote) => remote.read(cx).active_terminal().map(f),
        }
    }

    /// Change view-local state of the terminal on screen (selection,
    /// scrollback) — never its input, which goes through [`Self::write`].
    fn with_terminal_mut<R>(
        &mut self,
        cx: &mut gpui::App,
        f: impl FnOnce(&mut Terminal) -> R,
    ) -> Option<R> {
        match &mut self.source {
            TerminalSource::Owned(terminal) => Some(f(terminal)),
            TerminalSource::Host(host) => {
                host.update(cx, |model, _| model.host_mut().active_terminal_mut().map(f))
            }
            TerminalSource::Remote(remote) => {
                remote.update(cx, |model, _| model.active_terminal_mut().map(f))
            }
        }
    }

    /// The configured text size: the active project's effective `[ui]
    /// desktop_terminal_font_size` in the app (read each frame, so a saved
    /// change applies at once); the default for an owned terminal, which has
    /// no config.
    fn base_font_size(&self, cx: &gpui::App) -> u16 {
        match &self.source {
            TerminalSource::Owned(_) => UiConfig::DEFAULT_DESKTOP_TERMINAL_FONT_SIZE,
            TerminalSource::Host(host) => {
                host.read(cx)
                    .host()
                    .active_state()
                    .config
                    .ui
                    .desktop_terminal_font_size
            }
            TerminalSource::Remote(remote) => remote.read(cx).font_size(),
        }
    }

    /// The text size this frame draws at, in points: the setting plus the
    /// session zoom (see [`super::zoom`]).
    pub fn font_size(&self, cx: &gpui::App) -> f32 {
        match &self.source {
            TerminalSource::Owned(_) => TerminalZoom::current(cx).size_for(self.base_font_size(cx)),
            // The same rule split view's panes draw with.
            TerminalSource::Host(host) => super::zoom::app_font_size(host.read(cx).host(), cx),
            TerminalSource::Remote(_) => {
                TerminalZoom::current(cx).size_for(self.base_font_size(cx))
            }
        }
    }

    /// Whether the terminal draws dimmed: the TUI's `dim_terminal` — the app
    /// is in APP mode (the terminal is not receiving keys) and `[ui]
    /// dim_terminal_in_app_mode` is on. An owned terminal has no modes.
    pub fn dimmed(&self, cx: &gpui::App) -> bool {
        match &self.source {
            TerminalSource::Owned(_) => false,
            TerminalSource::Host(host) => {
                let host = host.read(cx).host();
                !host.terminal_focused() && host.active_state().config.ui.dim_terminal_in_app_mode
            }
            // The host's projects' setting is the host's; a remote window
            // never dims.
            TerminalSource::Remote(_) => false,
        }
    }

    /// The palette this frame paints with ([`Self::palette`], dimmed or not).
    pub fn frame_palette(&self, cx: &gpui::App) -> TermPalette {
        self.palette().with_dimmed(self.dimmed(cx))
    }

    /// `(offset, history)`: how far the view is scrolled into history and how
    /// much history there is, for the scrollbar.
    pub fn scroll_position(&self, cx: &gpui::App) -> Option<(usize, usize)> {
        self.with_terminal(cx, |t| {
            (t.screen().scrollback(), t.screen().scrollback_len())
        })
    }

    /// Whether text shaped for the last frame no longer fits: the font size
    /// changed (the cell usually does too, which the element also checks
    /// against [`Self::metrics`]). Records the new size.
    pub fn take_font_change(&mut self, points: f32) -> bool {
        self.shaped_at.replace(points) != Some(points)
    }

    /// Record whether the window is the active one (the element reads it each
    /// frame; the activation observer notifies on a change).
    pub fn set_window_active(&mut self, active: bool) {
        self.window_active = active;
    }

    /// Bring the row cache up to date for this frame and hand it to the
    /// element (which gives it back through [`Self::put_row_cache`]), with the
    /// selection overlay. An empty cache when no terminal is on screen.
    /// `focused` is key focus in an active window: it fills the block cursor
    /// and allows a blink.
    ///
    /// The cache belongs to one grid: when the terminal on screen changes (the
    /// host's selected agent or child shell), every row is laid out again. A
    /// grid's first damage report is always `Full`, so a new terminal is never
    /// drawn from another's rows either.
    pub fn prepare_rows(
        &mut self,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> (RowCache<Vec<ShapedLine>>, Vec<CellSpan>) {
        let mut cache = std::mem::take(&mut self.row_cache);
        let palette = self.frame_palette(cx);
        let blink_on = self.blink_on;
        let prepared = self.with_terminal(cx, |terminal| {
            let identity = grid_identity(terminal);
            let screen = terminal.screen();
            let placed = layout::cursor_placement(screen, focused);
            let blinks = focused && placed.is_some() && screen.cursor().blinking;
            // The off phase of a blink draws no cursor at all.
            let cursor = placed.filter(|_| !blinks || blink_on);
            let laid_out = cache.refresh_with_cursor(identity, screen, &palette, cursor);
            let selection = layout::selection_spans(screen, terminal.selection(), &palette);
            (laid_out, selection, blinks)
        });
        let Some((laid_out, selection, blinks)) = prepared else {
            self.set_blink_wanted(false, cx);
            return (RowCache::default(), Vec::new());
        };
        self.set_blink_wanted(blinks, cx);
        if let Some(probe) = &self.probe {
            probe.borrow_mut().rows_laid_out += laid_out as u64;
        }
        (cache, selection)
    }

    /// Take back the row cache lent by [`Self::prepare_rows`].
    pub fn put_row_cache(&mut self, cache: RowCache<Vec<ShapedLine>>) {
        self.row_cache = cache;
    }

    pub fn metrics(&self) -> Option<CellMetrics> {
        self.metrics
    }

    pub fn palette(&self) -> &TermPalette {
        &self.palette
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// What a frame shows of the grid in an element `fit` (rows, cols) cells
    /// big: all of it when it fits, else the panned part (see [`super::pan`]).
    /// Records it for the wheel, and keeps the pan within the grid.
    pub fn frame_viewport(&mut self, fit: (u16, u16), cx: &gpui::App) -> Viewport {
        let grid = self.with_terminal(cx, |t| {
            let cursor = t.screen().cursor();
            (
                grid_identity(t),
                t.screen().size(),
                (cursor.row, cursor.col),
            )
        });
        let Some((identity, size, cursor)) = grid else {
            self.viewport = Viewport::default();
            return self.viewport;
        };
        if self.pan_grid != Some(identity) {
            self.pan_grid = Some(identity);
            self.pan = Pan::default();
        }
        let mut view = self.pan.viewport(size, fit);
        if std::mem::take(&mut self.reveal_cursor) && view.overflows() {
            self.pan = pan::reveal(&view, cursor);
            view = self.pan.viewport(size, fit);
        }
        self.pan = self.pan.clamped(size, fit);
        self.viewport = view;
        view
    }

    /// What the last frame showed of the grid.
    #[cfg(test)]
    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub fn set_metrics(&mut self, metrics: CellMetrics) {
        self.metrics = Some(metrics);
    }

    /// Resize grid and PTY when the element's cell count changed. Cheap to call
    /// every frame. The host applies a new size to every project on its next
    /// turn (never from inside this paint).
    pub fn resize(&mut self, size: PtySize, cx: &mut gpui::App) {
        match &mut self.source {
            TerminalSource::Owned(terminal) => {
                if terminal.screen().size() != (size.rows, size.cols) {
                    let _ = terminal.resize(size);
                }
            }
            TerminalSource::Host(host) => host.update(cx, |model, _| model.set_viewport(size)),
            // D4: the host owns PTY geometry; the grid is the host's size and
            // the element letterboxes it.
            TerminalSource::Remote(_) => {}
        }
    }

    /// Send `bytes` to the terminal's program. An owned terminal is then
    /// polled fast for its answer; the host does the same for its own.
    fn write(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        match &mut self.source {
            TerminalSource::Owned(terminal) => {
                let _ = terminal.session_mut().write_input(bytes);
                self.cadence.input(Instant::now());
                if self.exited.is_none() {
                    self.poll = Some(Self::start_polling(cx));
                }
            }
            TerminalSource::Host(host) => {
                let event = HostEvent::TerminalInput(bytes.to_vec());
                host.update(cx, |model, cx| model.dispatch(event, cx));
            }
            TerminalSource::Remote(remote) => {
                let event = HostEvent::TerminalInput(bytes.to_vec());
                remote.update(cx, |model, cx| model.dispatch(event, cx));
            }
        }
    }

    // --- blinking -----------------------------------------------------------

    /// Follow the last frame's verdict on blinking: start the loop when a blink
    /// is wanted and none runs; when it is not, show the cursor steadily (the
    /// loop, if any, ends at its next wake without a frame).
    fn set_blink_wanted(&mut self, wanted: bool, cx: &mut Context<Self>) {
        self.blink_wanted = wanted;
        if !wanted {
            self.blink_on = true;
        } else if !self.blinking {
            self.start_blink(cx);
        }
    }

    /// (Re)start the blink loop with the cursor shown, so the next flip is a
    /// whole [`BLINK_INTERVAL`] away.
    fn start_blink(&mut self, cx: &mut Context<Self>) {
        self.blinking = true;
        self.blink_on = true;
        self.blink = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(BLINK_INTERVAL).await;
            if !matches!(this.update(cx, |view, cx| view.blink_tick(cx)), Ok(true)) {
                break;
            }
        }));
    }

    /// One blink phase. Flips the cursor and asks for a frame while a blink is
    /// still wanted in an active window; otherwise shows the cursor and ends.
    fn blink_tick(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.blink_wanted || !self.window_active {
            self.blinking = false;
            if !self.blink_on {
                self.blink_on = true;
                cx.notify();
            }
            return false;
        }
        self.blink_on = !self.blink_on;
        cx.notify();
        true
    }

    /// Typing shows the cursor at once and holds it for a whole phase, as
    /// terminals do.
    fn input_happened(&mut self, cx: &mut Context<Self>) {
        // A key brings the cursor back into view on a grid that does not fit
        // (see [`super::pan`]); the next frame works out the pan.
        if self.viewport.overflows() {
            self.reveal_cursor = true;
            cx.notify();
        }
        if self.blinking {
            let was_off = !self.blink_on;
            self.start_blink(cx);
            if was_off {
                cx.notify();
            }
        }
    }

    // --- keys ---------------------------------------------------------------

    /// The terminal's own app chords, before anything reaches the program:
    /// the zoom (macOS, see [`super::zoom`]) and Cmd-C with a selection. Returns
    /// whether the key was taken. Cmd-C with nothing selected is not taken, so
    /// it goes on to the platform as before.
    fn terminal_chord(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        if let Some(chord) = ZoomChord::of(keystroke) {
            let base = self.base_font_size(cx);
            let changed = cx.default_global::<TerminalZoom>().apply(chord, base);
            if changed {
                cx.notify();
            }
            return true;
        }
        let m = &keystroke.modifiers;
        let copy = platform::IS_MACOS
            && m.platform
            && !m.control
            && !m.alt
            && !m.shift
            && keystroke.key == "c";
        if copy {
            if let Some(text) = self.with_terminal_mut(cx, |t| t.selected_text()).flatten() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                return true;
            }
        }
        false
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(probe) = &self.probe {
            probe.borrow_mut().key_received();
        }
        if self.terminal_chord(event, cx) {
            cx.stop_propagation();
            return;
        }
        match &self.source {
            TerminalSource::Owned(_) => self.owned_key_down(event, cx),
            TerminalSource::Host(_) | TerminalSource::Remote(_) => {
                self.host_key_down(event, window, cx)
            }
        }
    }

    /// The app's Terminal mode: the keymap decides (a lenient table chord is
    /// performed, everything else is the TUI's PTY bytes), and printable text
    /// is left to the input handler so IME compositions work.
    fn host_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match terminal_key_down(
            crate::commands::keymap(),
            event,
            OptionKey::for_this_platform(),
        ) {
            TerminalKey::Action(entry) => {
                window.dispatch_action(Box::new(KeymapAction::for_entry(entry)), cx);
                cx.stop_propagation();
            }
            TerminalKey::Pty(bytes) => {
                self.write(&bytes, cx);
                self.input_happened(cx);
                cx.stop_propagation();
            }
            TerminalKey::Text | TerminalKey::Ignore => {}
        }
    }

    fn owned_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let TerminalSource::Owned(terminal) = &mut self.source else {
            return;
        };
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        // Copy / paste: Cmd+C/V on macOS, Ctrl+Shift+C/V elsewhere (Ctrl+C
        // alone must stay SIGINT).
        let clipboard_chord = if platform::IS_MACOS {
            m.platform && !m.control && !m.alt
        } else {
            m.control && m.shift && !m.alt
        };
        if clipboard_chord && ks.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                // The core's paste encoder, the one the TUI and the app use.
                let bytes = encode_paste(&text, terminal.bracketed_paste());
                terminal.clear_selection();
                terminal.scroll_to_bottom();
                self.write(&bytes, cx);
                self.input_happened(cx);
                cx.stop_propagation();
                // The jump back to the live screen shows before any echo does.
                cx.notify();
            }
            return;
        }
        if clipboard_chord && ks.key == "c" {
            if let Some(text) = terminal.selected_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.stop_propagation();
            }
            return;
        }
        if let Some(bytes) = input::encode_keystroke(ks) {
            // Repaint now only if the key itself changes what is shown; the
            // echo repaints when the poll parses it.
            let moved = terminal.has_selection() || terminal.screen().scrollback() > 0;
            terminal.clear_selection();
            terminal.scroll_to_bottom();
            self.write(&bytes, cx);
            self.input_happened(cx);
            if let Some(probe) = &self.probe {
                probe.borrow_mut().key_written();
            }
            cx.stop_propagation();
            if moved {
                cx.notify();
            }
        }
    }

    // --- mouse --------------------------------------------------------------

    /// The cell under a window position, clamped to the grid.
    fn cell_at(&self, position: Point<Pixels>, cx: &gpui::App) -> Option<(u16, u16)> {
        let (rows, cols) = self.with_terminal(cx, |t| t.screen().size())?;
        self.metrics.map(|m| m.cell_at(position, rows, cols))
    }

    /// Whether a press goes to the program: it asked for mouse reporting and
    /// Shift is not held (Shift forces a local selection, as in the TUI).
    fn forwards_mouse(&self, shift: bool, cx: &gpui::App) -> bool {
        !shift
            && self
                .with_terminal(cx, |t| t.screen().modes().wants_mouse())
                .unwrap_or(false)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        // A click on the app's terminal is the TUI's click on its pane: it
        // enters Terminal mode.
        if let TerminalSource::Host(host) = &self.source {
            let host = host.clone();
            if !host.read(cx).host().terminal_focused() {
                host.update(cx, |model, cx| model.dispatch(HostEvent::FocusTerminal, cx));
            }
        }
        if let TerminalSource::Remote(remote) = &self.source {
            let remote = remote.clone();
            if !remote.read(cx).terminal_focused() {
                remote.update(cx, |model, cx| model.dispatch(HostEvent::FocusTerminal, cx));
            }
        }
        let Some((row, col)) = self.cell_at(event.position, cx) else {
            return;
        };
        if event.button == MouseButton::Left && event.modifiers.secondary() {
            if let Some(link) = self.link_at_cell(row, col, cx) {
                cx.open_url(&link.url);
                return;
            }
        }
        if self.forwards_mouse(event.modifiers.shift, cx) {
            let Some((mode, encoding)) =
                self.with_terminal(cx, |t| (t.screen().modes().mouse_mode, t.mouse_encoding()))
            else {
                return;
            };
            if let Some(code) = input::button_code(event.button, &event.modifiers) {
                // X10 (`?9`) reports carry no modifier bits.
                let code = if mode == MouseMode::Press {
                    code & 3
                } else {
                    code
                };
                self.forwarded_button = Some(code);
                self.motion_cell = Some((row, col));
                self.write(
                    &input::mouse_button_bytes(encoding, code, col, row, true),
                    cx,
                );
            }
            return;
        }
        if event.button == MouseButton::Left {
            self.with_terminal_mut(cx, |t| t.begin_selection(row, col));
            self.drag = Some(Drag {
                position: event.position,
                cell: (row, col),
            });
            cx.notify();
        }
    }

    /// Any pointer move in the window (registered by the element each frame;
    /// `grid` is the grid's bounds): a forwarded drag's motion, any-motion
    /// reporting over the grid, or a selection drag — anything else is
    /// ignored without reading the terminal.
    pub fn window_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        grid: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.forwarded_button.is_none() && self.drag.is_none() {
            self.pointer = grid.contains(&event.position).then_some(event.position);
            self.link_modifier = event.modifiers.secondary();
            self.refresh_hover_link(cx);
        }
        if let Some(code) = self.forwarded_button {
            let Some((row, col)) = self.cell_at(event.position, cx) else {
                return;
            };
            let Some((mode, encoding)) =
                self.with_terminal(cx, |t| (t.screen().modes().mouse_mode, t.mouse_encoding()))
            else {
                return;
            };
            if matches!(mode, MouseMode::ButtonMotion | MouseMode::AnyMotion)
                && self.new_motion_cell((row, col))
            {
                self.write(&input::mouse_motion_bytes(encoding, code, col, row), cx);
            }
            return;
        }
        if let Some(drag) = self.drag {
            if event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            self.drag = Some(Drag {
                position: event.position,
                ..drag
            });
            self.extend_selection(event.position, cx);
            if self.edge(event.position, cx).is_some() && !self.autoscrolling {
                self.start_autoscroll(cx);
            }
            return;
        }
        // Hover motion with no button held, for any-motion (`?1003`) only, and
        // only over this grid.
        if event.pressed_button.is_none()
            && grid.contains(&event.position)
            && self.forwards_mouse(event.modifiers.shift, cx)
        {
            let Some((row, col)) = self.cell_at(event.position, cx) else {
                return;
            };
            let Some((mode, encoding)) =
                self.with_terminal(cx, |t| (t.screen().modes().mouse_mode, t.mouse_encoding()))
            else {
                return;
            };
            if mode == MouseMode::AnyMotion && self.new_motion_cell((row, col)) {
                // Code 3 is "no button" in xterm's encoding.
                self.write(&input::mouse_motion_bytes(encoding, 3, col, row), cx);
            }
        }
    }

    /// The modifier keys changed (Cmd / Ctrl pressed or let go over a link).
    pub fn modifiers_changed(&mut self, modifiers: &Modifiers, cx: &mut Context<Self>) {
        self.link_modifier = modifiers.secondary();
        self.refresh_hover_link(cx);
    }

    /// The link under the pointer, while the modifier is held. Recomputed on
    /// every pointer move, modifier change and frame, so the underline stays on
    /// the text even when output scrolls under a still pointer.
    pub fn refresh_hover_link(&mut self, cx: &mut Context<Self>) {
        let link = match (self.link_modifier, self.pointer) {
            (true, Some(position)) => self
                .cell_at(position, cx)
                .and_then(|(row, col)| self.link_at_cell(row, col, cx)),
            _ => None,
        };
        if link != self.hover_link {
            self.hover_link = link;
            cx.notify();
        }
    }

    /// The link underlined now, if any.
    pub fn hover_link(&self) -> Option<&Link> {
        self.hover_link.as_ref()
    }

    /// The web link covering visible cell `(row, col)`.
    fn link_at_cell(&self, row: u16, col: u16, cx: &gpui::App) -> Option<Link> {
        self.with_terminal(cx, |t| link_at(t.screen(), row, col))
            .flatten()
    }

    /// Whether a motion report for `cell` is due: xterm reports motion per
    /// cell, not per pixel, so a pointer moving within one cell sends nothing.
    fn new_motion_cell(&mut self, cell: (u16, u16)) -> bool {
        self.motion_cell.replace(cell) != Some(cell)
    }

    /// Any button release in the window: ends a forwarded drag (reporting the
    /// release, except under X10, which reports presses only) or a selection
    /// drag (copying the selection, or clearing an empty one).
    pub fn window_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if let Some(code) = self.forwarded_button.take() {
            let Some((row, col)) = self.cell_at(event.position, cx) else {
                return;
            };
            let Some((mode, encoding)) =
                self.with_terminal(cx, |t| (t.screen().modes().mouse_mode, t.mouse_encoding()))
            else {
                return;
            };
            if mode != MouseMode::Press {
                self.write(
                    &input::mouse_button_bytes(encoding, code, col, row, false),
                    cx,
                );
            }
            return;
        }
        if event.button != MouseButton::Left || self.drag.take().is_none() {
            return;
        }
        self.autoscroll = None;
        self.autoscrolling = false;
        // The TUI's release: copy a real selection (it stays highlighted as
        // the confirmation), clear a click that selected nothing.
        let copied = self
            .with_terminal_mut(cx, |t| {
                if t.has_selection() {
                    t.selected_text()
                } else {
                    t.clear_selection();
                    None
                }
            })
            .flatten();
        if let Some(text) = copied {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        cx.notify();
    }

    /// Move the selection head to the cell under `position` (clamped to the
    /// grid, so a pointer past an edge extends along it). Repaints only when
    /// the head moved to another cell.
    fn extend_selection(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(drag), Some((row, col))) = (self.drag, self.cell_at(position, cx)) else {
            return;
        };
        if drag.cell == (row, col) {
            return;
        }
        self.drag = Some(Drag {
            cell: (row, col),
            ..drag
        });
        self.with_terminal_mut(cx, |t| t.update_selection(row, col));
        cx.notify();
    }

    /// Which edge `position` is past, if any: above the first row on screen
    /// or below the last (a panned grid has rows past both, see
    /// [`super::pan`]).
    fn edge(&self, position: Point<Pixels>, cx: &gpui::App) -> Option<Edge> {
        let m = self.metrics?;
        self.with_terminal(cx, |_| ())?;
        let top = m.origin.y + m.height * f32::from(self.viewport.top);
        let bottom = top + m.height * f32::from(self.viewport.rows);
        if position.y < top {
            Some(Edge::Top)
        } else if position.y >= bottom {
            Some(Edge::Bottom)
        } else {
            None
        }
    }

    /// Scroll a line every [`AUTOSCROLL_EVERY`] while the drag stays past an
    /// edge, even with the pointer held still (no events arrive then). Ends by
    /// itself when the drag ends or the pointer comes back over the grid.
    fn start_autoscroll(&mut self, cx: &mut Context<Self>) {
        self.autoscrolling = true;
        self.autoscroll = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(AUTOSCROLL_EVERY).await;
            if !matches!(
                this.update(cx, |view, cx| view.autoscroll_step(cx)),
                Ok(true)
            ) {
                break;
            }
        }));
    }

    /// One auto-scroll step (the TUI's `autoscroll_drag`): scroll a line
    /// toward the edge and pin the selection head to the edge row, so the
    /// selection grows over the revealed line. Returns whether to go on.
    fn autoscroll_step(&mut self, cx: &mut Context<Self>) -> bool {
        let edge = self.drag.and_then(|drag| self.edge(drag.position, cx));
        let (Some(drag), Some(edge)) = (self.drag, edge) else {
            self.autoscrolling = false;
            return false;
        };
        let col = self.cell_at(drag.position, cx).map_or(0, |(_, col)| col);
        // As the wheel: a panned grid's hidden rows come before the history.
        let (panned, view) = (self.pan, self.viewport);
        let moved = self.with_terminal_mut(cx, |t| {
            let before = t.screen().scrollback();
            let pan = match edge {
                Edge::Top => {
                    let (pan, rest) = pan::scroll_up(panned, &view, 1);
                    scroll_history(t, true, rest);
                    pan
                }
                Edge::Bottom => pan::scroll_down(panned, 1, scroll_history(t, false, 1)),
            };
            let top = view.overflow_rows - pan.up.min(view.overflow_rows);
            let edge_row = match edge {
                Edge::Top => top,
                Edge::Bottom => (top + view.rows).saturating_sub(1),
            };
            t.update_selection(edge_row, col);
            let scrolled = t.screen().scrollback() != before || pan != panned;
            (scrolled, edge_row, pan)
        });
        let Some((scrolled, edge_row, pan)) = moved else {
            self.autoscrolling = false;
            return false;
        };
        self.pan = pan;
        if scrolled || drag.cell != (edge_row, col) {
            self.drag = Some(Drag {
                cell: (edge_row, col),
                ..drag
            });
            cx.notify();
        }
        true
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(metrics) = self.metrics else {
            return;
        };
        self.scroll_sideways(event, metrics, cx);
        // In notches for a wheel, in lines of this terminal for a trackpad's
        // pixels; either way, whole units are acted on and the rest is kept.
        let (units, per_notch, precise) = match event.delta {
            ScrollDelta::Lines(lines) => (lines.y, SCROLL_LINES, false),
            ScrollDelta::Pixels(pixels) => (pixels.y / metrics.height, 1, true),
        };
        // A remainder is in the last device's units: a wheel after a trackpad
        // starts from zero.
        if self.scroll_precise != precise {
            self.scroll_precise = precise;
            self.scroll_remainder = 0.0;
            self.scroll_remainder_x = 0.0;
        }
        self.scroll_remainder += units;
        let whole = self.scroll_remainder.trunc();
        if whole == 0.0 {
            return;
        }
        self.scroll_remainder -= whole;
        let up = whole > 0.0;
        let count = whole.abs() as usize;
        let Some(modes) = self.with_terminal(cx, |t| t.screen().modes()) else {
            return;
        };
        if modes.wants_mouse() {
            // As the TUI: the wheel is the program's whenever it reports the
            // mouse (Shift does not override it), one report per notch.
            let (row, col) = self.cell_at(event.position, cx).unwrap_or((0, 0));
            let bytes = input::wheel_bytes(modes.mouse_encoding, up, &event.modifiers, col, row);
            for _ in 0..count {
                self.write(&bytes, cx);
            }
            return;
        }
        // The rows of a grid taller than the element, then the history (there
        // is none on an alternate screen, as in the TUI). See [`super::pan`].
        let lines = count * per_notch;
        let before = self.pan;
        let moved = if up {
            let (pan, rest) = pan::scroll_up(self.pan, &self.viewport, lines);
            self.pan = pan;
            rest > 0 && self.with_terminal_mut(cx, |t| scroll_history(t, true, rest)) > Some(0)
        } else {
            let left = self
                .with_terminal_mut(cx, |t| scroll_history(t, false, lines))
                .unwrap_or(0);
            self.pan = pan::scroll_down(self.pan, lines, left);
            left > 0
        };
        if moved || self.pan != before {
            cx.notify();
        }
    }

    /// The wheel's sideways part pans a grid wider than the element (a
    /// trackpad swipe, or Shift-wheel where the platform turns it sideways).
    fn scroll_sideways(
        &mut self,
        event: &ScrollWheelEvent,
        metrics: CellMetrics,
        cx: &mut Context<Self>,
    ) {
        if self.viewport.overflow_cols == 0 {
            self.scroll_remainder_x = 0.0;
            return;
        }
        let columns = match event.delta {
            ScrollDelta::Lines(lines) => lines.x * SCROLL_LINES as f32,
            ScrollDelta::Pixels(pixels) => pixels.x / metrics.width,
        };
        self.scroll_remainder_x += columns;
        let whole = self.scroll_remainder_x.trunc();
        if whole == 0.0 {
            return;
        }
        self.scroll_remainder_x -= whole;
        // A positive delta moves the content right: the view goes left.
        let pan = pan::scroll_sideways(self.pan, &self.viewport, -(whole as i32));
        if pan != self.pan {
            self.pan = pan;
            cx.notify();
        }
    }
}

/// Scroll `t`'s history `lines` up or down; how many lines it moved.
fn scroll_history(t: &mut Terminal, up: bool, lines: usize) -> usize {
    let before = t.screen().scrollback();
    if up {
        t.scroll_up(lines);
    } else {
        t.scroll_down(lines);
    }
    t.screen().scrollback().abs_diff(before)
}

/// Which grid a row cache was built from: the address of the terminal's
/// boxed grid, stable for the terminal's life. See [`TerminalView::prepare_rows`]
/// for why an address reused by a later terminal is still safe.
pub(crate) fn grid_identity(terminal: &Terminal) -> usize {
    terminal.screen() as *const dyn flightdeck::terminal::grid::TerminalGrid as *const () as usize
}

pub(crate) fn channels(hex: Hex) -> (u8, u8, u8) {
    ((hex.0 >> 16) as u8, (hex.0 >> 8) as u8, hex.0 as u8)
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Text input for the app's terminal: committed text (typed characters, an
/// IME's final conversion, the platform's paste) reaches the program; a
/// composition in progress is only previewed. See [`ImeState`].
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        self.ime.text_for_range(range, adjusted_range)
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(self.ime.selected_text_range())
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.ime.marked_text_range()
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.ime.unmark();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bytes = self.ime.commit(text);
        if !bytes.is_empty() {
            self.write(&bytes, cx);
            self.input_happened(cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime.mark(new_text);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // The IME candidate window opens at the cursor cell.
        let cursor = self.with_terminal(cx, |t| {
            let cursor = t.screen().cursor();
            (cursor.row, cursor.col)
        });
        Some(match (self.metrics, cursor) {
            (Some(m), Some((row, col))) => Bounds::new(
                gpui::point(
                    m.origin.x + m.width * f32::from(col),
                    m.origin.y + m.height * f32::from(row),
                ),
                gpui::size(m.width, m.height),
            ),
            _ => element_bounds,
        })
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The cursor fills, hollows and stops blinking with the window's
        // activation, so a change of it is a frame.
        if !self.observing_activation {
            self.observing_activation = true;
            cx.observe_window_activation(window, |view, window, cx| {
                view.window_active = window.is_window_active();
                cx.notify();
            })
            .detach();
        }
        let p = Palette::global(cx);
        let status = self.exited.map(|state| match state {
            ProcessState::Exited(code) => format!("process exited ({code})"),
            _ => "process ended".to_string(),
        });
        let is_host = matches!(
            self.source,
            TerminalSource::Host(_) | TerminalSource::Remote(_)
        );
        // Press on the view (it is under the pointer then); moves and releases
        // are the element's window-level listeners, so a drag keeps working
        // past the view's edges.
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(self.palette.bg.hsla())
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::on_key_down))
            // Cmd / Ctrl pressed or let go with the pointer still over a link.
            .on_modifiers_changed(cx.listener(|view, event: &ModifiersChangedEvent, _, cx| {
                view.modifiers_changed(&event.modifiers, cx)
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    // The mockup's terminal padding (18px / 22px) in the app;
                    // the spike keeps its tighter frame.
                    .when_else(
                        is_host,
                        |d| d.py(gpui::px(18.)).px(gpui::px(22.)),
                        |d| d.p_2(),
                    )
                    .child(TerminalElement::new(cx.entity(), is_host)),
            )
            .children(status.map(|text| {
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(p.muted.hsla())
                    .bg(p.surface_window.hsla())
                    .child(text)
            }))
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
