//! Shared contracts: error type, domain types, service traits, and the trivial
//! real implementations. Main-agent-owned (SPECS §26/§27).

pub mod domain;
pub mod error;
pub mod real;
pub mod traits;

pub use domain::*;
pub use error::{FlightDeckError, Result};
pub use real::{RealClock, RealFs, SystemCommandRunner};
pub use traits::{
    Clock, CommandRunner, ContainerRuntime, FileSystem, GitExecutor, Notifier, PtyBackend,
    PtySession,
};
