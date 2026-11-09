//! Feeding a terminal what its PTY produced, a bounded amount at a time.
//!
//! Moved here from FlightDeck Desktop (`remote-control-ki05`) so every
//! front-end that mirrors a remote host's terminals
//! ([`crate::web::client::session`]) parses them the same way: the Desktop's
//! window and its headless `--dump-grid` probe, and the terminal UI's
//! `flightdeck connect`.

use crate::terminal::session::Terminal;

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
/// queries it raised, then advance the emulator's timers.
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
