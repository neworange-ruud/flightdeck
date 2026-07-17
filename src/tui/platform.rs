//! Compile-time OS detection for platform-specific key bindings.
//!
//! Key bindings differ per platform (e.g. the default leave-terminal-focus key,
//! and macOS terminals report Command as `SUPER` for the paste shortcut).
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

/// Whether the default leave-terminal-focus binding is `Shift+Esc` rather than
/// `Alt+Esc`. Windows and Linux reserve `Alt+Esc` for cycling windows.
pub const LEAVE_FOCUS_USES_SHIFT: bool = IS_WINDOWS || IS_LINUX;

/// User-facing label for the configured leave-terminal-focus binding.
pub fn leave_focus_key(use_f2: bool) -> &'static str {
    if use_f2 {
        "F2"
    } else if LEAVE_FOCUS_USES_SHIFT {
        "Shift+Esc"
    } else {
        "Alt+Esc"
    }
}
