//! The terminal view: owns one [`Terminal`], polls its PTY, routes input.
//!
//! Output: a foreground task wakes every [`POLL_INTERVAL`], drains the PTY into
//! the grid through [`super::pump`] and asks for a frame when anything
//! changed. [`PtySession`](flightdeck::contracts::PtySession) is poll-based
//! (the portable-pty backend fills a buffer from its own reader thread), so
//! polling costs one lock per tick and never blocks the UI thread.
//!
//! Input: keys go through [`super::input`]; mouse events are forwarded to the
//! program when it asked for mouse reporting (Shift overrides, as in most
//! terminals), and otherwise drive local selection (copied on release) and
//! scrollback; paste honours bracketed-paste mode.

use std::time::{Duration, Instant};

use gpui::{
    div, ClipboardItem, Context, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Render,
    ScrollWheelEvent, Styled, Task, Window,
};

use flightdeck::contracts::{ProcessState, PtySize};
use flightdeck::terminal::grid::MouseMode;
use flightdeck::terminal::session::Terminal;
use flightdeck::tui::platform;

use super::element::{CellMetrics, TerminalElement};
use super::input;
use super::layout::TermPalette;
use crate::theme::{Hex, Palette};

/// How often the PTY is polled. ~120 Hz: output shows up within a frame on a
/// 60 Hz display without spinning.
const POLL_INTERVAL: Duration = Duration::from_millis(8);
/// Repaint at least this often while nothing arrives, so an emulator timer
/// (a synchronized update that timed out) is never left undrawn.
const IDLE_REPAINT: Duration = Duration::from_millis(150);

pub struct TerminalView {
    terminal: Terminal,
    focus_handle: FocusHandle,
    palette: TermPalette,
    metrics: Option<CellMetrics>,
    /// The button held while forwarding a drag to a mouse-aware program.
    forwarded_button: Option<u8>,
    /// A local selection drag is in progress.
    selecting: bool,
    /// Fractional wheel lines not yet sent.
    scroll_remainder: f32,
    exited: Option<ProcessState>,
    _poll: Task<()>,
}

impl TerminalView {
    pub fn new(mut terminal: Terminal, cx: &mut Context<Self>) -> Self {
        let palette = TermPalette::from_palette(Palette::global(cx));
        terminal
            .screen_mut()
            .set_default_colors(channels(palette.fg), channels(palette.bg));
        let poll = cx.spawn(async move |this, cx| {
            let mut last_paint = Instant::now();
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let alive = this.update(cx, |view, cx| {
                    let changed = super::pump(&mut view.terminal);
                    let state = view.terminal.process_state();
                    let ended = !matches!(state, ProcessState::Running | ProcessState::Starting);
                    if ended && view.exited.is_none() {
                        view.exited = Some(state);
                    }
                    if changed || ended || last_paint.elapsed() >= IDLE_REPAINT {
                        last_paint = Instant::now();
                        cx.notify();
                    }
                    !ended
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }
        });
        Self {
            terminal,
            focus_handle: cx.focus_handle(),
            palette,
            metrics: None,
            forwarded_button: None,
            selecting: false,
            scroll_remainder: 0.0,
            exited: None,
            _poll: poll,
        }
    }

    pub fn terminal(&self) -> &Terminal {
        &self.terminal
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
    /// every frame.
    pub fn resize(&mut self, size: PtySize) {
        if self.terminal.screen().size() != (size.rows, size.cols) {
            let _ = self.terminal.resize(size);
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        let _ = self.terminal.session_mut().write_input(bytes);
    }

    /// The cell under a window position.
    fn cell_at(&self, position: gpui::Point<gpui::Pixels>) -> Option<(u16, u16)> {
        let (rows, cols) = self.terminal.screen().size();
        self.metrics.map(|m| m.cell_at(position, rows, cols))
    }

    fn forwards_mouse(&self, shift: bool) -> bool {
        !shift && self.terminal.screen().modes().wants_mouse()
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
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
                let bracketed = self.terminal.bracketed_paste();
                self.write(&input::paste_bytes(&text, bracketed));
                self.terminal.scroll_to_bottom();
                cx.stop_propagation();
            }
            return;
        }
        if clipboard_chord && ks.key == "c" {
            if let Some(text) = self.terminal.selected_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                cx.stop_propagation();
            }
            return;
        }
        if let Some(bytes) = input::encode_keystroke(ks) {
            self.terminal.clear_selection();
            self.terminal.scroll_to_bottom();
            self.write(&bytes);
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let Some((row, col)) = self.cell_at(event.position) else {
            return;
        };
        if self.forwards_mouse(event.modifiers.shift) {
            if let Some(code) = input::button_code(event.button, &event.modifiers) {
                let encoding = self.terminal.mouse_encoding();
                self.forwarded_button = Some(code);
                self.write(&input::mouse_button_bytes(encoding, code, col, row, true));
            }
            return;
        }
        if event.button == MouseButton::Left {
            self.terminal.begin_selection(row, col);
            self.selecting = true;
            cx.notify();
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some((row, col)) = self.cell_at(event.position) else {
            return;
        };
        if self.forwards_mouse(event.modifiers.shift) {
            let mode = self.terminal.screen().modes().mouse_mode;
            let encoding = self.terminal.mouse_encoding();
            match (self.forwarded_button, mode) {
                (Some(code), MouseMode::ButtonMotion | MouseMode::AnyMotion) => {
                    self.write(&input::mouse_motion_bytes(encoding, code, col, row));
                }
                // No button held: code 3 is "no button" in xterm's encoding.
                (None, MouseMode::AnyMotion) => {
                    self.write(&input::mouse_motion_bytes(encoding, 3, col, row));
                }
                _ => {}
            }
            return;
        }
        if self.selecting && event.pressed_button == Some(MouseButton::Left) {
            self.terminal.update_selection(row, col);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(code) = self.forwarded_button.take() {
            if let Some((row, col)) = self.cell_at(event.position) {
                let encoding = self.terminal.mouse_encoding();
                let mode = self.terminal.screen().modes().mouse_mode;
                // X10 (`?9`) reports presses only.
                if mode != MouseMode::Press {
                    self.write(&input::mouse_button_bytes(encoding, code, col, row, false));
                }
            }
            return;
        }
        if self.selecting {
            self.selecting = false;
            if let Some(text) = self.terminal.selected_text() {
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
        let modes = self.terminal.screen().modes();
        if self.forwards_mouse(event.modifiers.shift) {
            let (row, col) = self.cell_at(event.position).unwrap_or((0, 0));
            let bytes = input::wheel_bytes(modes.mouse_encoding, up, &event.modifiers, col, row);
            for _ in 0..count {
                self.write(&bytes);
            }
        } else if modes.alt_screen {
            // A full-screen program without mouse reporting has no scrollback
            // here: send arrow keys, as xterm's alternate-scroll mode does.
            let arrow: &[u8] = if up { b"\x1b[A" } else { b"\x1b[B" };
            for _ in 0..count {
                self.write(arrow);
            }
        } else if up {
            self.terminal.scroll_up(count);
        } else {
            self.terminal.scroll_down(count);
        }
        cx.notify();
    }
}

fn channels(hex: Hex) -> (u8, u8, u8) {
    ((hex.0 >> 16) as u8, (hex.0 >> 8) as u8, hex.0 as u8)
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::global(cx);
        let status = self.exited.map(|state| match state {
            ProcessState::Exited(code) => format!("process exited ({code})"),
            _ => "process ended".to_string(),
        });
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
                    .p_2()
                    .child(TerminalElement::new(cx.entity())),
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
