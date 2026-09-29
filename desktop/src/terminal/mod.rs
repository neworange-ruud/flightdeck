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
//! - [`rowcache`] keeps each row's layout between frames and redoes only the
//!   rows the emulator reports changed (damage tracking),
//! - [`element`] measures, shapes and paints it,
//! - [`view`] owns the terminal, polls the PTY and routes input,
//! - [`cadence`] decides how often the view polls and when it repaints,
//! - [`input`] is the spike's small keyboard/paste/mouse encoder,
//! - [`spike`] is the `--spike-terminal` window and the `--dump-grid` probe,
//! - [`bench`] is `--bench`, the latency/throughput/idle/scroll measurements.

pub mod bench;
pub mod boxdraw;
pub mod cadence;
pub mod element;
pub mod input;
pub mod layout;
pub mod rowcache;
pub mod spike;
pub mod view;

use flightdeck::terminal::grid::Emulator;
use flightdeck::terminal::session::{Terminal, TerminalProfile};

/// The emulator every desktop terminal is built on.
pub const EMULATOR: Emulator = Emulator::Alacritty;

/// How the app's tab terminals are built (`Env::terminal`): on [`EMULATOR`],
/// with the theme's terminal ink and background as the emulator's default
/// colours, so the answers to OSC 10/11 colour queries (which agents use to
/// choose a light or dark theme) are the colours actually painted. From
/// `Palette::dark()`, the palette `theme::init` installs: the app is
/// dark-only. The TUI keeps `TerminalProfile::TUI` (vt100).
pub fn desktop_profile() -> TerminalProfile {
    let palette = layout::TermPalette::from_palette(&crate::theme::Palette::dark());
    TerminalProfile {
        emulator: EMULATOR,
        default_colors: Some((view::channels(palette.fg), view::channels(palette.bg))),
    }
}

/// The most bytes one [`pump`] parses. alacritty_terminal parses roughly
/// 150 MB/s on an M2 Pro, so this is a few milliseconds of UI-thread time:
/// output that piled up while the thread was busy (a window resize, a slow
/// frame) is worked off over several polls with frames in between, instead of
/// in one long stall.
pub const PARSE_BUDGET: usize = 1 << 20;

/// What one [`pump`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pumped {
    /// Bytes fed to the emulator.
    pub parsed: usize,
    /// The emulator released output it had been holding (a synchronized
    /// update that ended or timed out): the screen changed without new bytes.
    pub released: bool,
    /// Bytes read but not yet parsed are waiting in the backlog.
    pub backlog: bool,
}

impl Pumped {
    /// Whether the screen may look different now.
    pub fn changed(&self) -> bool {
        self.parsed > 0 || self.released
    }
}

/// Move what the PTY has produced into the grid, at most [`PARSE_BUDGET`]
/// bytes of it (the rest waits in `backlog` for the next call), answer the
/// queries it raised, then advance the emulator's timers. Shared by the
/// window's poll loop and the headless `--dump-grid` probe, so the probe
/// exercises exactly what the window draws.
pub fn pump(terminal: &mut Terminal, backlog: &mut Vec<u8>) -> Pumped {
    let held = terminal.screen().holds_output();
    let fresh = terminal.session_mut().try_read_output().unwrap_or_default();
    let parsed = if backlog.is_empty() && fresh.len() <= PARSE_BUDGET {
        // The usual case: parse the read as is, no copy.
        feed(terminal, &fresh);
        fresh.len()
    } else {
        backlog.extend_from_slice(&fresh);
        let chunk: Vec<u8> = backlog.drain(..backlog.len().min(PARSE_BUDGET)).collect();
        feed(terminal, &chunk);
        chunk.len()
    };
    terminal.tick();
    Pumped {
        parsed,
        released: held && !terminal.screen().holds_output(),
        backlog: !backlog.is_empty(),
    }
}

fn feed(terminal: &mut Terminal, bytes: &[u8]) {
    if !bytes.is_empty() {
        terminal.process_output(bytes);
        terminal.answer_cursor_position_query(bytes);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use flightdeck::contracts::PtySize;
    use flightdeck::testing::{FakePty, FakePtyHandle};

    fn fake_terminal() -> (Terminal, FakePtyHandle) {
        let backend = FakePty::new();
        let handle = backend.queue_session();
        let terminal = Terminal::spawn(
            &backend,
            EMULATOR,
            "sh",
            &[],
            &[],
            std::path::Path::new("."),
            PtySize { rows: 5, cols: 20 },
        )
        .expect("fake spawn");
        (terminal, handle)
    }

    #[test]
    fn a_pump_parses_at_most_the_budget_and_keeps_the_rest() {
        let (mut terminal, pty) = fake_terminal();
        let mut backlog = Vec::new();
        let mut flood = vec![b'x'; PARSE_BUDGET + 10];
        flood.extend_from_slice(b"\r\nend");
        pty.push_output(flood);

        let first = pump(&mut terminal, &mut backlog);
        assert_eq!(first.parsed, PARSE_BUDGET);
        assert!(first.backlog && first.changed());
        let second = pump(&mut terminal, &mut backlog);
        assert_eq!(second.parsed, 15);
        assert!(!second.backlog);
        assert!(terminal.screen().contents().contains("end"));
        // Nothing new: nothing parsed, nothing to repaint.
        assert_eq!(pump(&mut terminal, &mut backlog), Pumped::default());
    }

    #[test]
    fn a_pump_reports_a_released_synchronized_update() {
        let (mut terminal, pty) = fake_terminal();
        let mut backlog = Vec::new();
        pty.push_output(b"\x1b[?2026hframe".to_vec());
        let held = pump(&mut terminal, &mut backlog);
        assert!(held.parsed > 0 && !held.released);
        assert!(terminal.screen().holds_output());
        pty.push_output(b"\x1b[?2026l".to_vec());
        let done = pump(&mut terminal, &mut backlog);
        assert!(!terminal.screen().holds_output());
        assert!(done.changed());
        assert!(terminal.screen().contents().contains("frame"));
    }
}
