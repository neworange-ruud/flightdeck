//! The desktop terminal: a GPUI element that draws a live PTY's cell grid
//! (beads `remote-control-bmej.1.2`, M0 spike S2).
//!
//! The pipeline is the core's, unchanged: a [`Terminal`] from
//! `flightdeck::terminal::session` owns the PTY session (any
//! [`flightdeck::contracts::PtyBackend`]) and an emulator grid reached only
//! through `flightdeck::terminal::grid`. The desktop builds its terminals on
//! [`EMULATOR`]; see desktop/NOTES-M0.md, "Terminal emulator", for why that is
//! alacritty_terminal while the TUI stays on vt100.
//!
//! - [`layout`] decides what a frame shows (pure, tested headlessly),
//! - [`element`] measures, shapes and paints it,
//! - [`view`] owns the terminal, polls the PTY and routes input,
//! - [`input`] is the spike's small keyboard/paste/mouse encoder,
//! - [`spike`] is the `--spike-terminal` window and the `--dump-grid` probe.

pub mod boxdraw;
pub mod element;
pub mod input;
pub mod layout;
pub mod spike;
pub mod view;

use flightdeck::terminal::grid::Emulator;
use flightdeck::terminal::session::Terminal;

/// The emulator every desktop terminal is built on.
pub const EMULATOR: Emulator = Emulator::Alacritty;

/// The most PTY reads one [`pump`] folds in, so a program flooding output
/// cannot starve the UI thread of frames.
const MAX_READS_PER_PUMP: usize = 64;

/// Move whatever the PTY has produced into the grid and answer the queries it
/// raised; then advance the emulator's timers. Returns whether any output was
/// parsed. Shared by the window's poll loop and the headless `--dump-grid`
/// probe, so the probe exercises exactly what the window draws.
pub fn pump(terminal: &mut Terminal) -> bool {
    let mut parsed = false;
    for _ in 0..MAX_READS_PER_PUMP {
        match terminal.session_mut().try_read_output() {
            Ok(bytes) if !bytes.is_empty() => {
                terminal.process_output(&bytes);
                terminal.answer_cursor_position_query(&bytes);
                parsed = true;
            }
            _ => break,
        }
    }
    terminal.tick();
    parsed
}

/// Environment a desktop terminal's process starts with. A GUI app launched
/// from the Finder, Explorer or a desktop launcher has no `TERM` of its own to
/// pass down (a TUI inherits its host terminal's), and without one most
/// programs fall back to dumb-terminal output. `COLORTERM` advertises
/// truecolour, which the grid and element support.
pub fn terminal_env() -> Vec<(String, String)> {
    vec![
        ("TERM".to_string(), "xterm-256color".to_string()),
        ("COLORTERM".to_string(), "truecolor".to_string()),
    ]
}
