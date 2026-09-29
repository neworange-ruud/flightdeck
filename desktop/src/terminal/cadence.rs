//! How often a terminal view polls its PTY (beads `remote-control-bmej.3.9`).
//!
//! The PTY session is poll-based, so the poll rate trades latency against idle
//! cost. One fixed rate cannot serve both: the spike polled every 8 ms, which
//! kept every idle terminal waking the UI thread 125 times a second, and still
//! left a keystroke's echo waiting up to 8 ms before it was even read. So the
//! rate follows what the terminal is doing (measurements in desktop/NOTES-M0.md,
//! "Performance (S4)"):
//!
//! - **just after input** ([`ECHO_WINDOW`]): every [`ECHO_POLL`]. A tty echo or
//!   a shell's redraw comes back within a millisecond or two, and reading it at
//!   once lets it make the next frame;
//! - **while output flows** or shortly after input ([`ACTIVE_WINDOW`]): every
//!   [`ACTIVE_POLL`], about one poll per frame at 120 Hz;
//! - **busy** (a backlog to parse, or a synchronized update to release on
//!   time): [`ACTIVE_POLL`] or faster;
//! - **idle**: every [`IDLE_POLL`], which is also the most that output a
//!   program starts on its own (a ticker, an agent waking up) waits before it
//!   is read: half the TUI's 50 ms loop.

use std::time::{Duration, Instant};

pub const ECHO_POLL: Duration = Duration::from_millis(1);
pub const ECHO_WINDOW: Duration = Duration::from_millis(40);
pub const ACTIVE_POLL: Duration = Duration::from_millis(8);
pub const ACTIVE_WINDOW: Duration = Duration::from_millis(250);
pub const IDLE_POLL: Duration = Duration::from_millis(25);
/// With a backlog waiting, poll again almost at once: the budget already
/// bounds how long each poll holds the UI thread.
pub const BACKLOG_POLL: Duration = Duration::from_millis(1);

/// The last input and output times one view has seen.
#[derive(Debug, Clone, Copy, Default)]
pub struct Cadence {
    last_input: Option<Instant>,
    last_output: Option<Instant>,
}

impl Cadence {
    /// Input was written to the PTY.
    pub fn input(&mut self, now: Instant) {
        self.last_input = Some(now);
    }

    /// A poll parsed output.
    pub fn output(&mut self, now: Instant) {
        self.last_output = Some(now);
    }

    /// How long to sleep before the next poll. `busy` is set while a backlog
    /// or a held synchronized update is waiting.
    pub fn next_delay(&self, now: Instant, busy: bool) -> Duration {
        let within = |at: Option<Instant>, window: Duration| {
            at.is_some_and(|at| now.saturating_duration_since(at) < window)
        };
        if within(self.last_input, ECHO_WINDOW) {
            ECHO_POLL
        } else if busy {
            BACKLOG_POLL.max(ECHO_POLL).min(ACTIVE_POLL)
        } else if within(self.last_input, ACTIVE_WINDOW) || within(self.last_output, ACTIVE_WINDOW)
        {
            ACTIVE_POLL
        } else {
            IDLE_POLL
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_terminal_polls_at_the_idle_rate() {
        assert_eq!(
            Cadence::default().next_delay(Instant::now(), false),
            IDLE_POLL
        );
    }

    #[test]
    fn input_polls_fast_for_the_echo_then_settles() {
        let t = Instant::now();
        let mut c = Cadence::default();
        c.input(t);
        assert_eq!(c.next_delay(t + Duration::from_millis(5), false), ECHO_POLL);
        assert_eq!(
            c.next_delay(t + Duration::from_millis(100), false),
            ACTIVE_POLL
        );
        assert_eq!(
            c.next_delay(t + Duration::from_millis(300), false),
            IDLE_POLL
        );
    }

    #[test]
    fn flowing_output_keeps_the_active_rate() {
        let t = Instant::now();
        let mut c = Cadence::default();
        c.output(t);
        assert_eq!(
            c.next_delay(t + Duration::from_millis(200), false),
            ACTIVE_POLL
        );
        c.output(t + Duration::from_millis(200));
        assert_eq!(
            c.next_delay(t + Duration::from_millis(400), false),
            ACTIVE_POLL
        );
        assert_eq!(
            c.next_delay(t + Duration::from_millis(500), false),
            IDLE_POLL
        );
    }

    #[test]
    fn a_waiting_backlog_or_held_update_is_polled_soon() {
        let t = Instant::now();
        assert_eq!(Cadence::default().next_delay(t, true), BACKLOG_POLL);
    }
}
