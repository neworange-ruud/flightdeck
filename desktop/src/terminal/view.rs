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
//!   handed to the host, which resizes every project's PTYs on its next turn.
//!
//! In both, the element redraws only the rows the emulator reports changed
//! ([`super::rowcache`], fed by `TerminalGrid::damage_since`), and nothing
//! repaints an idle terminal: there is no periodic repaint and the cursor does
//! not blink. GPUI draws at most one frame per display refresh however often a
//! view notifies, so output arriving over many polls lands in one frame.
//!
//! In both, mouse events are forwarded to the program when it asked for mouse
//! reporting (Shift overrides, as in most terminals), and otherwise drive
//! local selection (copied on release) and scrollback.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::time::Instant;

use gpui::prelude::FluentBuilder;
use gpui::{
    div, Bounds, ClipboardItem, Context, Entity, EntityInputHandler, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, Render, ScrollWheelEvent, ShapedLine, Styled, Task,
    UTF16Selection, Window,
};

use flightdeck::contracts::{ProcessState, PtySize};
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
use super::rowcache::RowCache;
use crate::host::HostModel;
use crate::theme::{Hex, Palette};

/// Where the terminal a [`TerminalView`] draws lives.
pub enum TerminalSource {
    /// The view owns it (the spike window).
    Owned(Terminal),
    /// The host owns it: whichever terminal is on screen in the active
    /// project (the app).
    Host(Entity<HostModel>),
}

pub struct TerminalView {
    source: TerminalSource,
    focus_handle: FocusHandle,
    palette: TermPalette,
    metrics: Option<CellMetrics>,
    /// Laid-out and shaped rows from earlier frames (see [`super::rowcache`]).
    /// The element borrows it for each frame.
    row_cache: RowCache<Vec<ShapedLine>>,
    /// The button held while forwarding a drag to a mouse-aware program.
    forwarded_button: Option<u8>,
    /// A local selection drag is in progress.
    selecting: bool,
    /// Fractional wheel lines not yet sent.
    scroll_remainder: f32,
    exited: Option<ProcessState>,
    /// The composition in progress (Host source; the spike types keys).
    ime: ImeState,
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

    fn with_source(source: TerminalSource, palette: TermPalette, cx: &mut Context<Self>) -> Self {
        Self {
            source,
            focus_handle: cx.focus_handle(),
            palette,
            metrics: None,
            forwarded_button: None,
            selecting: false,
            scroll_remainder: 0.0,
            exited: None,
            ime: ImeState::default(),
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
            TerminalSource::Host(_) => None,
        }
    }

    /// The owned terminal, mutably, for the bench driver.
    pub fn owned_terminal_mut(&mut self) -> Option<&mut Terminal> {
        match &mut self.source {
            TerminalSource::Owned(terminal) => Some(terminal),
            TerminalSource::Host(_) => None,
        }
    }

    /// Read the terminal on screen, if there is one.
    fn with_terminal<R>(&self, cx: &gpui::App, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
        match &self.source {
            TerminalSource::Owned(terminal) => Some(f(terminal)),
            TerminalSource::Host(host) => host.read(cx).host().active_terminal().map(f),
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
        }
    }

    /// Bring the row cache up to date for this frame and hand it to the
    /// element (which gives it back through [`Self::put_row_cache`]), with the
    /// selection overlay. An empty cache when no terminal is on screen.
    ///
    /// The cache belongs to one grid: when the terminal on screen changes (the
    /// host's selected agent or child shell), every row is laid out again. A
    /// grid's first damage report is always `Full`, so a new terminal is never
    /// drawn from another's rows either.
    pub fn prepare_rows(
        &mut self,
        focused: bool,
        cx: &mut gpui::App,
    ) -> (RowCache<Vec<ShapedLine>>, Vec<CellSpan>) {
        let mut cache = std::mem::take(&mut self.row_cache);
        let palette = self.palette;
        let prepared = self.with_terminal(cx, |terminal| {
            let identity = grid_identity(terminal);
            let laid_out = cache.refresh(identity, terminal.screen(), &palette, focused);
            let selection =
                layout::selection_spans(terminal.screen(), terminal.selection(), &palette);
            (laid_out, selection)
        });
        let Some((laid_out, selection)) = prepared else {
            return (RowCache::default(), Vec::new());
        };
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
        }
    }

    /// The cell under a window position.
    fn cell_at(&self, position: Point<Pixels>, cx: &gpui::App) -> Option<(u16, u16)> {
        let (rows, cols) = self.with_terminal(cx, |t| t.screen().size())?;
        self.metrics.map(|m| m.cell_at(position, rows, cols))
    }

    fn forwards_mouse(&self, shift: bool, cx: &gpui::App) -> bool {
        !shift
            && self
                .with_terminal(cx, |t| t.screen().modes().wants_mouse())
                .unwrap_or(false)
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(probe) = &self.probe {
            probe.borrow_mut().key_received();
        }
        match &self.source {
            TerminalSource::Owned(_) => self.owned_key_down(event, cx),
            TerminalSource::Host(_) => self.host_key_down(event, window, cx),
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
                let bytes = input::paste_bytes(&text, terminal.bracketed_paste());
                terminal.scroll_to_bottom();
                self.write(&bytes, cx);
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
            if let Some(probe) = &self.probe {
                probe.borrow_mut().key_written();
            }
            cx.stop_propagation();
            if moved {
                cx.notify();
            }
        }
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
        let Some((row, col)) = self.cell_at(event.position, cx) else {
            return;
        };
        if self.forwards_mouse(event.modifiers.shift, cx) {
            if let Some(code) = input::button_code(event.button, &event.modifiers) {
                let encoding = self
                    .with_terminal(cx, |t| t.mouse_encoding())
                    .unwrap_or_default();
                self.forwarded_button = Some(code);
                self.write(
                    &input::mouse_button_bytes(encoding, code, col, row, true),
                    cx,
                );
            }
            return;
        }
        if event.button == MouseButton::Left {
            self.with_terminal_mut(cx, |t| t.begin_selection(row, col));
            self.selecting = true;
            cx.notify();
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some((row, col)) = self.cell_at(event.position, cx) else {
            return;
        };
        if self.forwards_mouse(event.modifiers.shift, cx) {
            let Some((mode, encoding)) =
                self.with_terminal(cx, |t| (t.screen().modes().mouse_mode, t.mouse_encoding()))
            else {
                return;
            };
            match (self.forwarded_button, mode) {
                (Some(code), MouseMode::ButtonMotion | MouseMode::AnyMotion) => {
                    self.write(&input::mouse_motion_bytes(encoding, code, col, row), cx);
                }
                // No button held: code 3 is "no button" in xterm's encoding.
                (None, MouseMode::AnyMotion) => {
                    self.write(&input::mouse_motion_bytes(encoding, 3, col, row), cx);
                }
                _ => {}
            }
            return;
        }
        if self.selecting && event.pressed_button == Some(MouseButton::Left) {
            self.with_terminal_mut(cx, |t| t.update_selection(row, col));
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(code) = self.forwarded_button.take() {
            if let Some((row, col)) = self.cell_at(event.position, cx) {
                let Some((mode, encoding)) =
                    self.with_terminal(cx, |t| (t.screen().modes().mouse_mode, t.mouse_encoding()))
                else {
                    return;
                };
                // X10 (`?9`) reports presses only.
                if mode != MouseMode::Press {
                    self.write(
                        &input::mouse_button_bytes(encoding, code, col, row, false),
                        cx,
                    );
                }
            }
            return;
        }
        if self.selecting {
            self.selecting = false;
            if let Some(text) = self.with_terminal_mut(cx, |t| t.selected_text()).flatten() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            cx.notify();
        }
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(metrics) = self.metrics else {
            return;
        };
        let delta = event.delta.pixel_delta(metrics.height);
        self.scroll_remainder += delta.y / metrics.height;
        let lines = self.scroll_remainder.trunc();
        if lines == 0.0 {
            return;
        }
        self.scroll_remainder -= lines;
        let up = lines > 0.0;
        let count = lines.abs() as usize;
        let Some(modes) = self.with_terminal(cx, |t| t.screen().modes()) else {
            return;
        };
        if self.forwards_mouse(event.modifiers.shift, cx) {
            let (row, col) = self.cell_at(event.position, cx).unwrap_or((0, 0));
            let bytes = input::wheel_bytes(modes.mouse_encoding, up, &event.modifiers, col, row);
            for _ in 0..count {
                self.write(&bytes, cx);
            }
        } else if modes.alt_screen {
            // A full-screen program without mouse reporting has no scrollback
            // here: send arrow keys, as xterm's alternate-scroll mode does.
            let arrow: &[u8] = if up { b"\x1b[A" } else { b"\x1b[B" };
            for _ in 0..count {
                self.write(arrow, cx);
            }
        } else if up {
            self.with_terminal_mut(cx, |t| t.scroll_up(count));
        } else {
            self.with_terminal_mut(cx, |t| t.scroll_down(count));
        }
        cx.notify();
    }
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::global(cx);
        let status = self.exited.map(|state| match state {
            ProcessState::Exited(code) => format!("process exited ({code})"),
            _ => "process ended".to_string(),
        });
        let is_host = matches!(self.source, TerminalSource::Host(_));
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(self.palette.bg.hsla())
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
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
