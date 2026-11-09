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
//! - [`pan`] moves the view over a grid larger than the element (a remote
//!   host's grid),
//! - [`rowcache`] keeps each row's layout between frames and redoes only the
//!   rows the emulator reports changed (damage tracking),
//! - [`element`] measures, shapes and paints it,
//! - [`view`] owns the terminal, polls the PTY and routes input,
//! - [`cadence`] decides how often the view polls and when it repaints,
//! - [`input`] is the spike's small keyboard encoder and the mouse reports,
//! - [`zoom`] is the text size: the `[ui]` setting plus Cmd +/-/0 on macOS,
//! - [`spike`] is the `--spike-terminal` window and the `--dump-grid` probe,
//! - [`bench`] is `--bench`, the latency/throughput/idle/scroll measurements.

pub mod bench;
pub mod boxdraw;
pub mod cadence;
pub mod element;
pub mod input;
pub mod layout;
pub mod pan;
pub mod rowcache;
pub mod spike;
pub mod view;
pub mod zoom;

use flightdeck::terminal::grid::Emulator;
use flightdeck::terminal::session::TerminalProfile;

/// The emulator every desktop terminal is built on.
pub const EMULATOR: Emulator = Emulator::Alacritty;

/// How the app's tab terminals are built (`Env::terminal`): on [`EMULATOR`],
/// with the theme's terminal ink and background as the emulator's default
/// colours, so the answers to OSC 10/11 colour queries (which agents use to
/// choose a light or dark theme) are the colours actually painted. From
/// `Palette::dark()`, the palette `theme::init` installs: the app is
/// dark-only. Every agent, shell and child terminal starts with
/// [`TERMINAL_ENV`]. The TUI keeps `TerminalProfile::TUI` (vt100).
pub fn desktop_profile() -> TerminalProfile {
    let palette = layout::TermPalette::from_palette(&crate::theme::Palette::dark());
    TerminalProfile {
        emulator: EMULATOR,
        default_colors: Some((view::channels(palette.fg), view::channels(palette.bg))),
        env: TERMINAL_ENV,
    }
}

// The bounded PTY-to-grid pump is the core's, shared with the terminal UI's
// remote client.
pub use flightdeck::terminal::pump::pump;
#[cfg(test)]
use flightdeck::terminal::pump::{Pumped, PARSE_BUDGET};
#[cfg(test)]
use flightdeck::terminal::session::Terminal;

/// Environment a desktop terminal's process starts with. A GUI app launched
/// from the Finder, Explorer or a desktop launcher has no `TERM` of its own to
/// pass down (a TUI inherits its host terminal's), and without one most
/// programs fall back to dumb-terminal output: no colour, no bold. Started
/// from a terminal, the app would pass down *that* terminal's description,
/// which is not the emulator these programs run in. `COLORTERM` advertises
/// truecolour, which the grid and element support.
pub const TERMINAL_ENV: &[(&str, &str)] = &[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")];

/// [`TERMINAL_ENV`] as a PTY takes it, for the terminals spawned outside a
/// tab session (the spike and the benchmark).
pub fn terminal_env() -> Vec<(String, String)> {
    TERMINAL_ENV
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_terminals_say_what_they_are() {
        // Launched from Finder the app has no TERM to pass down; without one
        // agents print without colour or bold.
        let profile = desktop_profile();
        assert!(profile.env.contains(&("TERM", "xterm-256color")));
        assert!(profile.env.contains(&("COLORTERM", "truecolor")));
    }
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
