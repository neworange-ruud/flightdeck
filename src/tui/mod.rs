//! TUI layer (T8): Ratatui layout, rendering, input mapping, and command
//! palette (SPECS §19–§24).
//!
//! The TUI renders `AppState` and emits `Command`s; it never executes git/fs/pty
//! directly (SPECS §27).

pub mod clipboard;
pub mod config_manager;
pub mod input;
pub mod layout;
pub mod mode_style;
pub mod palette;
pub mod platform;
pub mod render;
pub mod selection;
