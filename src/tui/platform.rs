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

/// Whether `Shift+Esc` (rather than `Alt+Esc`) is the leave-terminal-focus key.
///
/// True on Windows and Linux, where the OS/window manager reserves `Alt+Esc`
/// for cycling windows so FlightDeck never receives it. This is the single
/// source of truth for the platform split: the input mapping, the
/// `LEAVE_FOCUS_KEY` label, and the help overlay all derive from it.
pub const LEAVE_FOCUS_USES_SHIFT: bool = IS_WINDOWS || IS_LINUX;
