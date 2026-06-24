//! Application core (T7): headless app state, events, commands, and input modes
//! (SPECS §3, §4, §18, §22, §23, §24, §25).
//!
//! This layer performs **no** terminal I/O and never executes git/fs/pty
//! directly — it dispatches commands into the services (SPECS §27).

pub mod commands;
pub mod events;
pub mod modes;
pub mod state;
