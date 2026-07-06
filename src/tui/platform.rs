//! Compile-time OS detection for platform-specific key bindings.
//!
//! Key bindings differ per platform (e.g. the leave-terminal-focus key), and
//! expressing that with scattered `#[cfg(...)]` attributes makes the binding
//! logic hard to read. Instead we expose one boolean constant per OS so the
//! bindings read as ordinary checks (`IS_LINUX`) rather than attribute gates.
//!
//! These are `cfg!(...)` constants folded at compile time, so branching on
//! them costs nothing at runtime and the dead arm is optimized away.

/// Whether the target OS is Windows.
pub const IS_WINDOWS: bool = cfg!(target_os = "windows");

/// Whether the target OS is Linux.
pub const IS_LINUX: bool = cfg!(target_os = "linux");

/// Whether the target OS is macOS.
pub const IS_MACOS: bool = cfg!(target_os = "macos");
