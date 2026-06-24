//! Input modes (SPECS §23).

/// The two FlightDeck input modes (SPECS §23). In [`InputMode::Terminal`] most
/// keystrokes go to the active terminal; in [`InputMode::App`] keystrokes control
/// FlightDeck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Terminal,
    App,
}

impl Default for InputMode {
    fn default() -> Self {
        InputMode::App
    }
}
