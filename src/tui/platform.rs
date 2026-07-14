//! Compile-time OS detection for platform-specific key bindings.
//!
//! Some key handling differs per platform (e.g. macOS terminals report the
//! Command key as `SUPER`, so the paste shortcut accepts `Cmd-V` there).
//! Expressing that with scattered `#[cfg(...)]` attributes makes the logic hard
//! to read, so we expose one boolean constant per OS and branch on it as an
//! ordinary check (`IS_MACOS`) rather than an attribute gate.
//!
//! These are `cfg!(...)` constants folded at compile time, so branching on
//! them costs nothing at runtime and the dead arm is optimized away.

/// Whether the target OS is Windows.
pub const IS_WINDOWS: bool = cfg!(target_os = "windows");

/// Whether the target OS is Linux.
pub const IS_LINUX: bool = cfg!(target_os = "linux");

/// Whether the target OS is macOS.
pub const IS_MACOS: bool = cfg!(target_os = "macos");
