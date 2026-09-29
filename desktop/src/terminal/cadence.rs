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
#[derive(Debug, Clone, Copy)]
pub struct Cadence {
    last_input: Option<Instant>,
    last_output: Option<Instant>,
    /// The rate while output flows: [`ACTIVE_POLL`] unless set otherwise.
    active_poll: Duration,
    /// The rate when idle: [`IDLE_POLL`] unless set otherwise.
    idle_poll: Duration,
}

impl Default for Cadence {
    fn default() -> Self {
        Cadence::with_rates(ACTIVE_POLL, IDLE_POLL)
    }
}

impl Cadence {
    /// A cadence that polls every `active_poll` while output flows and every
    /// `idle_poll` when idle. The app's host uses slower ones than a single
    /// terminal: its turn services every project (PTYs, status files, the
    /// repository status refresh), and output from any of them keeps it
    /// active.
    pub fn with_rates(active_poll: Duration, idle_poll: Duration) -> Self {
        Cadence {
            last_input: None,
            last_output: None,
            active_poll,
            idle_poll,
        }
    }

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
            BACKLOG_POLL.max(ECHO_POLL).min(self.active_poll)
        } else if within(self.last_input, ACTIVE_WINDOW) || within(self.last_output, ACTIVE_WINDOW)
        {
            self.active_poll
        } else {
            self.idle_poll
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
    fn the_rates_can_be_slower() {
        let t = Instant::now();
        let mut c = Cadence::with_rates(Duration::from_millis(16), Duration::from_millis(50));
        assert_eq!(c.next_delay(t, false), Duration::from_millis(50));
        c.output(t);
        assert_eq!(
            c.next_delay(t + Duration::from_millis(100), false),
            Duration::from_millis(16)
        );
        c.input(t);
        assert_eq!(c.next_delay(t, false), ECHO_POLL, "echoes stay fast");
    }

    #[test]
    fn a_waiting_backlog_or_held_update_is_polled_soon() {
        let t = Instant::now();
        assert_eq!(Cadence::default().next_delay(t, true), BACKLOG_POLL);
    }
}
