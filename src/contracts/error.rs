//! Shared error type for all FlightDeck services.
//!
//! Every fallible service returns [`Result<T>`]. The UI must never panic on a
//! user-reachable error (SPECS §26); it surfaces the message instead.

/// The single error type used across every FlightDeck service.
#[derive(thiserror::Error, Debug)]
pub enum FlightDeckError {
    /// A git operation failed.
    #[error("git error: {0}")]
    Git(String),
    /// A filesystem operation failed.
    #[error("io error: {0}")]
    Io(String),
    /// Configuration could not be parsed, serialized, or validated.
    #[error("config error: {0}")]
    Config(String),
    /// Runtime state (`state.json`) could not be loaded or saved.
    #[error("state error: {0}")]
    State(String),
    /// The selected agent's command was not found in `PATH` (SPECS §16).
    #[error("agent command not found: {0}")]
    AgentMissing(String),
    /// A guarded operation was refused for safety (SPECS §5/§13/§15).
    #[error("operation refused: {0}")]
    Refused(String),
    /// Any other error.
    #[error("{0}")]
    Other(String),
}

/// Result alias used throughout FlightDeck.
pub type Result<T> = std::result::Result<T, FlightDeckError>;

impl From<std::io::Error> for FlightDeckError {
    fn from(e: std::io::Error) -> Self {
        FlightDeckError::Io(e.to_string())
    }
}

impl From<serde_json::Error> for FlightDeckError {
    fn from(e: serde_json::Error) -> Self {
        FlightDeckError::State(e.to_string())
    }
}
