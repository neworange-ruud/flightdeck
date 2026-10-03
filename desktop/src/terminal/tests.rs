//! The production terminal element (beads `remote-control-bmej.3.2`), driven
//! through GPUI's headless test platform.
//!
//! Two harnesses: a view that owns a terminal over the core's `FakePty` (the
//! spike path; its input bytes are read back from the fake), and a view of the
//! host's terminal over a real `AppHost` on the fakes (the app path). Mouse
//! events go through the window, so they reach the element's window-level
//! listeners exactly as a real pointer does; the clipboard is the test
//! platform's. Every expected byte string is built with the core's own
//! encoders (`encode_mouse_button`, `encode_mouse_report`, `encode_paste`),
//! which are the TUI's.

use std::path::Path;

use flightdeck::app::keymap::encode_paste;
use flightdeck::contracts::PtySize;
use flightdeck::host::testing::{self, TestProject};
use flightdeck::host::HostEvent;
use flightdeck::terminal::grid::{
    encode_mouse_button, encode_mouse_report, CursorShape, Emulator, MouseEncoding,
};
use flightdeck::terminal::session::Terminal;
use flightdeck::testing::{
    FakeClock, FakeCommandRunner, FakeContainerRuntime, FakeFs, FakeNotifier, FakePty,
    FakePtyHandle,
};
use flightdeck::tui::platform;
use flightdeck::Env;
use gpui::{
    point, px, size, AppContext, ClipboardItem, Entity, Modifiers, MouseButton, Pixels, Point,
    ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext,
};

use super::*;
use crate::host::HostModel;
use crate::terminal::layout::CursorPaint;

// --- harnesses ---------------------------------------------------------------

/// A view owning a terminal on a fake PTY, focused, drawn once.
fn owned(
    app: &mut TestAppContext,
    emulator: Emulator,
) -> (Entity<TerminalView>, FakePtyHandle, &mut VisualTestContext) {
    let backend = FakePty::new();
    let pty = backend.queue_session();
    let terminal = Terminal::spawn(
        &backend,
        emulator,
        "sh",
        &[],
        &[],
        Path::new("."),
        PtySize { rows: 24, cols: 80 },
    )
    .expect("fake spawn");
    init(app);
    let (view, cx) = app.add_window_view(|_, cx| TerminalView::new(terminal, cx));
    focus(&view, cx);
    (view, pty, cx)
}

/// The widget layer and the theme, once per test app (a test may open several
/// windows).
fn init(app: &mut TestAppContext) {
    app.update(|cx| {
        if !cx.has_global::<crate::theme::Palette>() {
            gpui_component::init(cx);
            crate::theme::init(cx);
        }
    });
}

fn focus(view: &Entity<TerminalView>, cx: &mut VisualTestContext) {
    let handle = view.read_with(cx, |v, _| v.focus_handle().clone());
    // A test window starts inactive; the app's window is the active one.
    cx.update(|window, cx| {
        window.activate_window();
        window.focus(&handle, cx);
    });
    cx.run_until_parked();
}

/// Feed `bytes` to the owned terminal as PTY output and parse them now.
fn output(
    view: &Entity<TerminalView>,
    pty: &FakePtyHandle,
    cx: &mut VisualTestContext,
    bytes: &[u8],
) {
    pty.push_output(bytes.to_vec());
    view.update(cx, |v, cx| {
        v.poll_once(cx);
    });
    cx.run_until_parked();
}

/// The centre of cell `(row, col)` in window coordinates.
fn cell(
    view: &Entity<TerminalView>,
    cx: &mut VisualTestContext,
    row: u16,
    col: u16,
) -> Point<Pixels> {
    let m = view
        .read_with(cx, |v, _| v.metrics())
        .expect("drawn at least once");
    point(
        m.origin.x + m.width * (f32::from(col) + 0.5),
        m.origin.y + m.height * (f32::from(row) + 0.5),
    )
}

/// Everything written to the fake PTY since the last call.
fn written(pty: &FakePtyHandle, seen: &mut usize) -> Vec<u8> {
    let all = pty.input();
    let new = all[(*seen).min(all.len())..].to_vec();
    *seen = all.len();
    new
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::default()
    }
}

fn read_terminal<R>(
    view: &Entity<TerminalView>,
    cx: &mut VisualTestContext,
    f: impl FnOnce(&Terminal) -> R,
) -> R {
    view.read_with(cx, |v, cx| v.with_terminal(cx, f))
        .expect("a terminal on screen")
}

/// The cursor the last frame laid out, if any.
fn drawn_cursor(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> Option<CursorPaint> {
    view.read_with(cx, |v, _| {
        v.row_cache.rows().iter().find_map(|r| r.layout.cursor)
    })
}

/// `n` numbered lines, so there is history to scroll.
fn lines(n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| format!("line {i}\r\n"))
        .collect::<String>()
        .into_bytes()
}

// --- host harness --------------------------------------------------------------

struct Fakes {
    fs: FakeFs,
    pty: FakePty,
    clock: FakeClock,
    container: FakeContainerRuntime,
    command: FakeCommandRunner,
    notifier: FakeNotifier,
}

fn fakes() -> &'static Fakes {
    Box::leak(Box::new(Fakes {
        fs: FakeFs::new(),
        pty: FakePty::new(),
        clock: FakeClock::default(),
        container: FakeContainerRuntime::new(),
        command: FakeCommandRunner::new(),
        notifier: FakeNotifier::new(),
    }))
}

/// A view of the host's terminal: one project, one agent tab whose primary
/// terminal runs on a fake PTY. `tweak` edits the project's config first.
fn hosted(
    app: &mut TestAppContext,
    tweak: impl FnOnce(&mut flightdeck::contracts::UiConfig),
) -> (
    FakePtyHandle,
    Entity<HostModel>,
    Entity<TerminalView>,
    &mut VisualTestContext,
) {
    let f = fakes();
    let env = Env {
        fs: &f.fs,
        pty: &f.pty,
        clock: &f.clock,
        container: &f.container,
        command: &f.command,
        terminal: crate::terminal::desktop_profile(),
    };
    let mut project = TestProject::new("alpha", &["a1"]);
    tweak(&mut project.state.config.ui);
    let host = testing::host(env, &f.notifier, vec![project], 0);
    init(app);
    let model = app.new(|_| HostModel::new(host));
    let pty = f.pty.queue_session();
    model.update(app, |m, _| {
        m.host_mut()
            .active_state_mut()
            .selected_mut()
            .expect("a tab")
            .session
            .spawn_primary(
                &f.pty,
                "sh",
                &[],
                Path::new("/alpha"),
                PtySize { rows: 24, cols: 80 },
            )
            .expect("fake spawn");
    });
    let for_view = model.clone();
    let (view, cx) = app.add_window_view(|_, cx| TerminalView::for_host(for_view, cx));
    focus(&view, cx);
    (pty, model, view, cx)
}

fn dispatch(model: &Entity<HostModel>, cx: &mut VisualTestContext, event: HostEvent) {
    model.update(cx, |m, cx| m.dispatch(event, cx));
    cx.run_until_parked();
}

// --- selection -----------------------------------------------------------------

#[gpui::test]
fn a_drag_selects_and_release_copies_to_the_clipboard(app: &mut TestAppContext) {
    let (view, pty, cx) = owned(app, Emulator::Alacritty);
    output(&view, &pty, cx, b"hello world\r\nsecond line");
    let (from, to) = (cell(&view, cx, 0, 0), cell(&view, cx, 1, 5));
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    // Nothing is copied until the button comes up (the TUI's rule).
    assert_eq!(cx.read_from_clipboard(), None);
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("hello world\nsecond".to_string())
    );
    // The selection stays highlighted as the confirmation.
    assert!(read_terminal(&view, cx, |t| t.has_selection()));

    // A click that selects nothing clears it and copies nothing.
    cx.write_to_clipboard(ClipboardItem::new_string("kept".into()));
    let at = cell(&view, cx, 0, 3);
    cx.simulate_click(at, Modifiers::none());
    assert!(!read_terminal(&view, cx, |t| t.has_selection()));
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("kept".to_string())
    );
}

#[gpui::test]
fn a_drag_keeps_selecting_outside_the_view(app: &mut TestAppContext) {
    let (view, pty, cx) = owned(app, Emulator::Alacritty);
    output(&view, &pty, cx, b"abcdef");
    let start = cell(&view, cx, 0, 1);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    // Far to the right of the window: the element's window-level listener
    // still extends the selection, clamped to the last column.
    let outside = point(start.x + px(5000.), start.y);
    cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::none());
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("bcdef".to_string())
    );
}

#[gpui::test]
fn a_drag_past_the_top_edge_scrolls_history_and_extends_the_selection(app: &mut TestAppContext) {
    let (view, pty, cx) = owned(app, Emulator::Alacritty);
    output(&view, &pty, cx, &lines(200));
    let rows = read_terminal(&view, cx, |t| t.screen().size().0);
    let top_line = read_terminal(&view, cx, |t| t.screen().row_text(0, 0, 20));
    let start = cell(&view, cx, 2, 0);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    let above = point(start.x, px(-40.));
    cx.simulate_mouse_move(above, MouseButton::Left, Modifiers::none());
    assert_eq!(read_terminal(&view, cx, |t| t.screen().scrollback()), 0);
    // Held still past the edge: a line per tick, with no further events.
    for _ in 0..3 {
        cx.executor().advance_clock(AUTOSCROLL_EVERY);
        cx.run_until_parked();
    }
    assert_eq!(read_terminal(&view, cx, |t| t.screen().scrollback()), 3);
    cx.simulate_mouse_up(above, MouseButton::Left, Modifiers::none());
    let copied = cx.read_from_clipboard().and_then(|c| c.text()).unwrap();
    let first = copied.lines().next().unwrap().to_string();
    // The copy starts three lines above what was the top row, and runs down
    // to where the drag began.
    let top_n: usize = top_line.trim_start_matches("line ").parse().unwrap();
    assert_eq!(first, format!("line {}", top_n - 3));
    assert_eq!(copied.lines().count(), 3 + 3);
    assert!(rows > 3);
    // The loop ended with the drag: time passing scrolls nothing more.
    cx.executor().advance_clock(AUTOSCROLL_EVERY * 4);
    cx.run_until_parked();
    assert_eq!(read_terminal(&view, cx, |t| t.screen().scrollback()), 3);

    // Past the bottom edge it scrolls back toward the live screen.
    let at = cell(&view, cx, 1, 0);
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    let below = point(start.x, px(5000.));
    cx.simulate_mouse_move(below, MouseButton::Left, Modifiers::none());
    for _ in 0..2 {
        cx.executor().advance_clock(AUTOSCROLL_EVERY);
        cx.run_until_parked();
    }
    assert_eq!(read_terminal(&view, cx, |t| t.screen().scrollback()), 1);
    cx.simulate_mouse_up(below, MouseButton::Left, Modifiers::none());
}

#[gpui::test]
fn shift_drag_selects_even_when_the_program_reports_the_mouse(app: &mut TestAppContext) {
    let (view, pty, cx) = owned(app, Emulator::Alacritty);
    output(&view, &pty, cx, b"\x1b[?1000h\x1b[?1006hmouse app");
    let mut seen = pty.input().len();
    let (from, to) = (cell(&view, cx, 0, 0), cell(&view, cx, 0, 4));

    // Without Shift the press is the program's, and nothing is selected.
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(from, MouseButton::Left, Modifiers::none());
    let mut expected = encode_mouse_button(MouseEncoding::Sgr, 0, 0, 0, true);
    expected.extend(encode_mouse_button(MouseEncoding::Sgr, 0, 0, 0, false));
    assert_eq!(written(&pty, &mut seen), expected);
    assert!(!read_terminal(&view, cx, |t| t.has_selection()));

    // With Shift it selects, and the program hears nothing.
    cx.simulate_mouse_down(from, MouseButton::Left, shift());
    cx.simulate_mouse_move(to, MouseButton::Left, shift());
    cx.simulate_mouse_up(to, MouseButton::Left, shift());
    assert!(written(&pty, &mut seen).is_empty());
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("mouse".to_string())
    );
}

#[gpui::test]
fn cmd_c_copies_the_selection_on_macos(app: &mut TestAppContext) {
    let (view, pty, cx) = owned(app, Emulator::Alacritty);
    output(&view, &pty, cx, b"copy me");
    let at = cell(&view, cx, 0, 0);
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    let at = cell(&view, cx, 0, 3);
    cx.simulate_mouse_move(at, MouseButton::Left, Modifiers::none());
    let at = cell(&view, cx, 0, 3);
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    cx.write_to_clipboard(ClipboardItem::new_string("other".into()));
    let chord = if platform::IS_MACOS {
        "cmd-c"
    } else {
        "ctrl-shift-c"
    };
    cx.simulate_keystrokes(chord);
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("copy".to_string())
    );
}

// --- mouse reporting -----------------------------------------------------------

/// For one mode and encoding: press, a drag, a hover and a release, and the
/// bytes each must produce (the core encoders; `None` = nothing sent).
fn check_reporting(
    app: &mut TestAppContext,
    emulator: Emulator,
    mode: &str,
    encoding: (&str, MouseEncoding),
) {
    let (view, pty, cx) = owned(app, emulator);
    let setup = format!("\x1b[?{mode}h{}", encoding.0);
    output(&view, &pty, cx, setup.as_bytes());
    let mut seen = pty.input().len();
    let enc = encoding.1;
    let press_only = mode == "9";
    let button_motion = mode == "1002" || mode == "1003";
    let any_motion = mode == "1003";

    let at = cell(&view, cx, 2, 3);

    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
    assert_eq!(
        written(&pty, &mut seen),
        encode_mouse_button(enc, 0, 3, 2, true),
        "press, ?{mode}"
    );
    let at = cell(&view, cx, 2, 7);
    cx.simulate_mouse_move(at, MouseButton::Left, Modifiers::none());
    let drag = if button_motion {
        encode_mouse_report(enc, 32, 7, 2)
    } else {
        Vec::new()
    };
    assert_eq!(written(&pty, &mut seen), drag, "drag, ?{mode}");
    // Motion is reported per cell: a pixel within the same cell sends nothing.
    let at = cell(&view, cx, 2, 7);
    cx.simulate_mouse_move(
        point(at.x + px(1.), at.y),
        MouseButton::Left,
        Modifiers::none(),
    );
    assert!(written(&pty, &mut seen).is_empty(), "same cell, ?{mode}");
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    let release = if press_only {
        Vec::new()
    } else {
        encode_mouse_button(enc, 0, 7, 2, false)
    };
    assert_eq!(written(&pty, &mut seen), release, "release, ?{mode}");
    let at = cell(&view, cx, 4, 1);
    cx.simulate_mouse_move(at, None, Modifiers::none());
    let hover = if any_motion {
        encode_mouse_report(enc, 35, 1, 4)
    } else {
        Vec::new()
    };
    assert_eq!(written(&pty, &mut seen), hover, "hover, ?{mode}");
    // Right button: code 2.
    let at = cell(&view, cx, 0, 0);
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
    assert_eq!(
        written(&pty, &mut seen),
        encode_mouse_button(enc, 2, 0, 0, true)
    );
    let at = cell(&view, cx, 0, 0);
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
    let _ = written(&pty, &mut seen);
    // Never a selection while the program owns the mouse.
    assert!(!read_terminal(&view, cx, |t| t.has_selection()));
}

const ENCODINGS: [(&str, MouseEncoding); 3] = [
    ("", MouseEncoding::Default),
    ("\x1b[?1005h", MouseEncoding::Utf8),
    ("\x1b[?1006h", MouseEncoding::Sgr),
];

#[gpui::test]
fn press_release_reports_match_the_core_encoders(app: &mut TestAppContext) {
    for encoding in ENCODINGS {
        check_reporting(app, Emulator::Alacritty, "1000", encoding);
    }
}

#[gpui::test]
fn button_motion_reports_match_the_core_encoders(app: &mut TestAppContext) {
    for encoding in ENCODINGS {
        check_reporting(app, Emulator::Alacritty, "1002", encoding);
    }
}

#[gpui::test]
fn any_motion_reports_match_the_core_encoders(app: &mut TestAppContext) {
    for encoding in ENCODINGS {
        check_reporting(app, Emulator::Alacritty, "1003", encoding);
    }
}

/// X10 (`?9`) is a mode only vt100 implements (alacritty_terminal does not).
#[gpui::test]
fn x10_reports_presses_only(app: &mut TestAppContext) {
    for encoding in ENCODINGS {
        check_reporting(app, Emulator::Vt100, "9", encoding);
    }
}

// --- wheel ---------------------------------------------------------------------

fn wheel(cx: &mut VisualTestContext, at: Point<Pixels>, delta: ScrollDelta) {
    cx.simulate_event(ScrollWheelEvent {
        position: at,
        delta,
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
}

#[gpui::test]
fn the_wheel_scrolls_history_three_lines_a_notch_and_trackpad_pixels_exactly(
    app: &mut TestAppContext,
) {
    let (view, pty, cx) = owned(app, Emulator::Alacritty);
    output(&view, &pty, cx, &lines(200));
    let at = cell(&view, cx, 3, 3);
    let offset = |view: &Entity<TerminalView>, cx: &mut VisualTestContext| {
        read_terminal(view, cx, |t| t.screen().scrollback())
    };
    wheel(cx, at, ScrollDelta::Lines(point(0., 1.)));
    assert_eq!(offset(&view, cx), SCROLL_LINES);
    // A trackpad: pixels accumulate into whole lines of this terminal.
    let height = view.read_with(cx, |v, _| v.metrics().unwrap().height);
    wheel(cx, at, ScrollDelta::Pixels(point(px(0.), height * 0.6)));
    assert_eq!(offset(&view, cx), SCROLL_LINES, "under a line: kept");
    wheel(cx, at, ScrollDelta::Pixels(point(px(0.), height * 0.6)));
    assert_eq!(offset(&view, cx), SCROLL_LINES + 1);
    wheel(cx, at, ScrollDelta::Lines(point(0., -1.)));
    assert_eq!(offset(&view, cx), 1);
    // Nothing is sent to a program that does not report the mouse.
    assert_eq!(pty.input(), Vec::<u8>::new());
    // Typing returns to the live screen and drops the selection.
    cx.simulate_keystrokes("a");
    assert_eq!(offset(&view, cx), 0);
}

#[gpui::test]
fn the_wheel_is_the_programs_when_it_reports_the_mouse(app: &mut TestAppContext) {
    let (view, pty, cx) = owned(app, Emulator::Alacritty);
    output(&view, &pty, cx, &lines(100));
    output(&view, &pty, cx, b"\x1b[?1000h\x1b[?1006h");
    let mut seen = pty.input().len();
    let at = cell(&view, cx, 3, 5);
    wheel(cx, at, ScrollDelta::Lines(point(0., 2.)));
    let up = encode_mouse_report(MouseEncoding::Sgr, 64, 5, 3);
    assert_eq!(written(&pty, &mut seen), [up.clone(), up].concat());
    wheel(cx, at, ScrollDelta::Lines(point(0., -1.)));
    assert_eq!(
        written(&pty, &mut seen),
        encode_mouse_report(MouseEncoding::Sgr, 65, 5, 3)
    );
    // The history did not move.
    assert_eq!(read_terminal(&view, cx, |t| t.screen().scrollback()), 0);
}

#[gpui::test]
fn the_wheel_does_nothing_on_an_alternate_screen_without_reporting(app: &mut TestAppContext) {
    let (view, pty, cx) = owned(app, Emulator::Alacritty);
    output(&view, &pty, cx, &lines(100));
    output(&view, &pty, cx, b"\x1b[?1049hfull screen");
    let at = cell(&view, cx, 1, 1);
    wheel(cx, at, ScrollDelta::Lines(point(0., 3.)));
    // As in the TUI: no arrow keys sent, and no history there to scroll.
    assert_eq!(pty.input(), Vec::<u8>::new());
    assert_eq!(read_terminal(&view, cx, |t| t.screen().scrollback()), 0);
}

// --- paste ---------------------------------------------------------------------

#[gpui::test]
fn a_paste_is_the_core_encoders_bytes_bracketed_when_asked(app: &mut TestAppContext) {
    let (view, pty, cx) = owned(app, Emulator::Alacritty);
    let chord = if platform::IS_MACOS {
        "cmd-v"
    } else {
        "ctrl-shift-v"
    };
    let text = "echo one\necho two";
    cx.write_to_clipboard(ClipboardItem::new_string(text.into()));
    let mut seen = 0;
    cx.simulate_keystrokes(chord);
    assert_eq!(written(&pty, &mut seen), encode_paste(text, false));
    output(&view, &pty, cx, b"\x1b[?2004h");
    cx.simulate_keystrokes(chord);
    assert_eq!(written(&pty, &mut seen), encode_paste(text, true));
}

// --- cursor --------------------------------------------------------------------

#[gpui::test]
fn the_cursor_takes_the_decscusr_shape_and_hollows_when_unfocused(app: &mut TestAppContext) {
    let (view, pty, cx) = owned(app, Emulator::Alacritty);
    output(&view, &pty, cx, b"$ ");
    let c = drawn_cursor(&view, cx).expect("a cursor");
    assert_eq!((c.shape, c.filled), (CursorShape::Block, true));
    for (seq, shape) in [
        ("\x1b[4 q", CursorShape::Underline),
        ("\x1b[6 q", CursorShape::Bar),
        ("\x1b[2 q", CursorShape::Block),
    ] {
        output(&view, &pty, cx, seq.as_bytes());
        assert_eq!(
            drawn_cursor(&view, cx).map(|c| c.shape),
            Some(shape),
            "{seq:?}"
        );
    }
    // Focus elsewhere: the block is an outline.
    cx.update(|window, cx| window.blur(cx));
    cx.run_until_parked();
    assert_eq!(drawn_cursor(&view, cx).map(|c| c.filled), Some(false));
    focus(&view, cx);
    assert_eq!(drawn_cursor(&view, cx).map(|c| c.filled), Some(true));
    // Another window active: an outline too.
    cx.deactivate_window();
    cx.run_until_parked();
    assert_eq!(drawn_cursor(&view, cx).map(|c| c.filled), Some(false));
}

#[gpui::test]
fn a_steady_cursor_never_blinks(app: &mut TestAppContext) {
    let (view, pty, cx) = owned(app, Emulator::Alacritty);
    output(&view, &pty, cx, b"\x1b[2 q$ ");
    for _ in 0..4 {
        cx.executor().advance_clock(BLINK_INTERVAL);
        cx.run_until_parked();
        assert!(drawn_cursor(&view, cx).is_some());
    }
    assert!(!view.read_with(cx, |v, _| v.blinking));
}

#[gpui::test]
fn a_blinking_cursor_flips_twice_a_second_and_stops_in_an_inactive_window(
    app: &mut TestAppContext,
) {
    let (view, pty, cx) = owned(app, Emulator::Alacritty);
    output(&view, &pty, cx, b"\x1b[1 q$ ");
    assert!(drawn_cursor(&view, cx).is_some(), "starts shown");
    cx.executor().advance_clock(BLINK_INTERVAL);
    cx.run_until_parked();
    assert!(drawn_cursor(&view, cx).is_none(), "off phase");
    cx.executor().advance_clock(BLINK_INTERVAL);
    cx.run_until_parked();
    assert!(drawn_cursor(&view, cx).is_some(), "on again");
    // A key shows it at once and restarts the phase.
    cx.executor().advance_clock(BLINK_INTERVAL);
    cx.run_until_parked();
    assert!(drawn_cursor(&view, cx).is_none());
    cx.simulate_keystrokes("x");
    assert!(drawn_cursor(&view, cx).is_some(), "typing shows the cursor");

    // An inactive window: steady, and the loop ends (no more frames).
    cx.deactivate_window();
    cx.run_until_parked();
    cx.executor().advance_clock(BLINK_INTERVAL * 3);
    cx.run_until_parked();
    let (blinking, on) = view.read_with(cx, |v, _| (v.blinking, v.blink_on));
    assert!(!blinking && on, "no blink loop while inactive");
    assert!(drawn_cursor(&view, cx).is_some());
}

// --- font size -----------------------------------------------------------------

#[gpui::test]
fn the_font_size_comes_from_the_setting_and_zooms_on_macos(app: &mut TestAppContext) {
    let (_pty, _model, view, cx) = hosted(app, |ui| ui.desktop_terminal_font_size = 16);
    let size_now = |view: &Entity<TerminalView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| v.font_size(cx))
    };
    assert_eq!(size_now(&view, cx), 16.0);
    let cell_16 = view.read_with(cx, |v, _| v.metrics().unwrap());
    if platform::IS_MACOS {
        cx.simulate_keystrokes("cmd-= cmd-=");
        assert_eq!(size_now(&view, cx), 18.0);
        let cell_18 = view.read_with(cx, |v, _| v.metrics().unwrap());
        assert!(cell_18.width > cell_16.width && cell_18.height > cell_16.height);
        cx.simulate_keystrokes("cmd--");
        assert_eq!(size_now(&view, cx), 17.0);
        cx.simulate_keystrokes("cmd-0");
        assert_eq!(size_now(&view, cx), 16.0);
        assert_eq!(view.read_with(cx, |v, _| v.metrics().unwrap()), cell_16);
    } else {
        // No zoom chord off macOS: Ctrl-= is the terminal's.
        cx.simulate_keystrokes("ctrl-=");
        assert_eq!(size_now(&view, cx), 16.0);
    }
}

#[gpui::test]
fn the_default_font_size_is_the_old_fixed_one(app: &mut TestAppContext) {
    let (view, _pty, cx) = owned(app, Emulator::Alacritty);
    assert_eq!(view.read_with(cx, |v, cx| v.font_size(cx)), 13.0);
}

// --- dimming -------------------------------------------------------------------

#[gpui::test]
fn the_terminal_dims_in_app_mode_as_the_tui_does(app: &mut TestAppContext) {
    let (pty, model, view, cx) = hosted(app, |_| {});
    pty.push_output(b"agent output".to_vec());
    dispatch(&model, cx, HostEvent::FocusTerminal);
    model.update(cx, |m, cx| {
        m.turn(cx);
    });
    cx.run_until_parked();
    let texts = |view: &Entity<TerminalView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| {
            let dim = v.frame_palette(cx).dimmed;
            let fg: Vec<_> = v
                .row_cache
                .rows()
                .iter()
                .flat_map(|r| r.layout.texts.iter().map(|t| t.fg))
                .collect();
            (dim, fg)
        })
    };
    let (dim, fg) = texts(&view, cx);
    assert!(!dim, "TERMINAL mode: not dimmed");
    let dim_ink = view.read_with(cx, |v, _| v.palette().dim_ink);
    assert!(!fg.is_empty() && fg.iter().all(|c| *c != dim_ink));
    dispatch(&model, cx, HostEvent::FocusApp);
    let (dim, fg) = texts(&view, cx);
    assert!(dim, "APP mode: dimmed");
    assert!(fg.iter().all(|c| *c == dim_ink));
}

#[gpui::test]
fn dimming_follows_the_setting(app: &mut TestAppContext) {
    let (_pty, model, view, cx) = hosted(app, |ui| ui.dim_terminal_in_app_mode = false);
    dispatch(&model, cx, HostEvent::FocusApp);
    assert!(!view.read_with(cx, |v, cx| v.dimmed(cx)));
}

// --- scrollbar -----------------------------------------------------------------

#[test]
fn the_scrollbar_shows_only_in_history_and_tracks_the_offset() {
    use crate::terminal::element::scrollbars;
    use crate::terminal::pan::Pan;
    let track = gpui::Bounds::new(point(px(0.), px(0.)), size(px(400.), px(240.)));
    let fits = Pan::default().viewport((24, 80), (24, 80));
    let thumb = |offset, history| scrollbars(track, &fits, offset, history).0;
    assert_eq!(thumb(0, 1000), None, "live screen");
    assert_eq!(thumb(5, 0), None, "no history");
    assert_eq!(
        scrollbars(track, &fits, 10, 1000).1,
        None,
        "nothing sideways"
    );
    let oldest = thumb(1000, 1000).unwrap();
    let newer = thumb(10, 1000).unwrap();
    assert_eq!(oldest.origin.y, px(0.), "at the top of the history");
    assert!(newer.origin.y > oldest.origin.y);
    assert!(newer.origin.y + newer.size.height <= px(240.) + px(0.01));
    // At the right edge, thin, never shorter than the minimum.
    assert!(oldest.origin.x > px(390.) && oldest.size.width <= px(4.));
    assert!(oldest.size.height >= px(16.));
    // A short history: the thumb is the share of the content on screen.
    let half = thumb(24, 24).unwrap();
    assert_eq!(half.size.height, px(120.));
}

#[test]
fn a_grid_larger_than_the_element_always_shows_its_scrollbars() {
    use crate::terminal::element::scrollbars;
    use crate::terminal::pan::Pan;
    let track = gpui::Bounds::new(point(px(0.), px(0.)), size(px(400.), px(240.)));
    // 48 rows in 24, 160 columns in 80, on the live screen.
    let view = Pan::default().viewport((48, 160), (24, 80));
    let (vertical, horizontal) = scrollbars(track, &view, 0, 0);
    let vertical = vertical.expect("rows are out of view");
    assert_eq!(vertical.size.height, px(120.), "half the rows show");
    assert_eq!(vertical.origin.y, px(120.), "the bottom half");
    let horizontal = horizontal.expect("columns are out of view");
    assert_eq!(horizontal.size.width, px(200.));
    assert_eq!(horizontal.origin.x, px(0.), "the left half");
    assert!(horizontal.origin.y > px(230.), "along the bottom edge");
}

// --- resize and the web ----------------------------------------------------------

#[gpui::test]
fn a_resize_sets_the_pty_and_what_the_web_is_told(app: &mut TestAppContext) {
    let (pty, model, view, cx) = hosted(app, |_| {});
    let grid_of = |view: &Entity<TerminalView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| v.with_terminal(cx, |t| t.screen().size()))
            .unwrap()
    };
    let check = |model: &Entity<HostModel>,
                 view: &Entity<TerminalView>,
                 pty: &FakePtyHandle,
                 cx: &mut VisualTestContext| {
        model.update(cx, |m, cx| {
            m.turn(cx);
        });
        cx.run_until_parked();
        let m = view.read_with(cx, |v, _| v.metrics().unwrap());
        let (rows, cols) = grid_of(view, cx);
        // The grid is what the element measured...
        let bounds = cx.update(|window, _| window.viewport_size());
        assert!(f32::from(m.width) * f32::from(cols) <= f32::from(bounds.width));
        // ...the PTY was told...
        assert_eq!(pty.resizes().last(), Some(&PtySize { rows, cols }));
        // ...and so is FlightDeck Web (D4: the desktop owns the geometry).
        let web = model.read_with(cx, |m, _| m.host().web_host_state());
        assert_eq!((web.geometry.rows, web.geometry.cols), (rows, cols));
        let terminal = &web.projects[0].sessions[0].terminals[0];
        assert_eq!(
            (terminal.geometry.rows, terminal.geometry.cols),
            (rows, cols)
        );
        (rows, cols)
    };
    let before = check(&model, &view, &pty, cx);
    assert_ne!(before, (24, 80), "sized to the window, not the spawn size");
    cx.simulate_resize(size(px(500.), px(300.)));
    cx.run_until_parked();
    let after = check(&model, &view, &pty, cx);
    assert!(
        after.0 < before.0 || after.1 < before.1,
        "{before:?} -> {after:?}"
    );
}
