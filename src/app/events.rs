//! Application events (T7, SPECS §23).
//!
//! Models the inputs the event loop (T9) feeds the app core. Key→[`Command`]
//! mapping lives in the TUI layer (T8); the events here are deliberately minimal
//! and carry no terminal-library types (no crossterm/ratatui), keeping this
//! layer headless and unit-testable.
//!
//! [`Command`]: crate::app::commands::Command

use crate::contracts::TabId;

/// A raw key press, library-agnostic. The TUI layer (T8) translates its own
/// key type into this before handing it to the core, and maps it onward to a
/// [`Command`](crate::app::commands::Command). The core itself does not
/// interpret keys — it is included so the event loop has a single channel type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPress {
    /// The logical key (e.g. `"g"`, `"Enter"`, `"Esc"`, `"Left"`).
    pub key: String,
    /// Whether Ctrl was held.
    pub ctrl: bool,
    /// Whether Alt was held.
    pub alt: bool,
    /// Whether Shift was held.
    pub shift: bool,
}

/// The events the event loop feeds the application core (SPECS §23, §24).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    /// A key was pressed. Mapping to a [`Command`](crate::app::commands::Command)
    /// is the TUI layer's responsibility (T8).
    Key(KeyPress),
    /// Output bytes drained from a tab's terminal PTY. The core ingests these to
    /// update interpreted status (SPECS §24).
    PtyOutput {
        /// The tab whose terminal produced the output.
        tab: TabId,
        /// The raw bytes read from the PTY.
        bytes: Vec<u8>,
    },
    /// A periodic tick (drives status refresh / process-state polling).
    Tick,
    /// The host terminal was resized.
    Resize {
        /// New row count.
        rows: u16,
        /// New column count.
        cols: u16,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_press_equality() {
        let a = KeyPress {
            key: "g".to_string(),
            ctrl: true,
            alt: false,
            shift: false,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn pty_output_event_carries_tab_and_bytes() {
        let ev = AppEvent::PtyOutput {
            tab: TabId("t1".to_string()),
            bytes: b"hello".to_vec(),
        };
        match ev {
            AppEvent::PtyOutput { tab, bytes } => {
                assert_eq!(tab, TabId("t1".to_string()));
                assert_eq!(bytes, b"hello");
            }
            _ => panic!("wrong variant"),
        }
    }
}
