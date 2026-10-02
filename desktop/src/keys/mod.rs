//! Keyboard input for the desktop app: GPUI bindings generated from the one
//! keymap table, and the Terminal-mode adapter that types into a PTY.
//!
//! Nothing here holds a chord list. Every binding comes from
//! [`flightdeck::app::keymap::Keymap`], the same table the TUI dispatches from
//! and the help screen documents, so the two front-ends cannot drift.
//!
//! ## How a key press is routed
//!
//! ```text
//! KeyDownEvent
//!   1. GPUI keymap: exact chords from the table, bound as `KeymapAction`s
//!      under the key contexts "Global" / "Terminal" / "App".
//!      -> the view's `on_action::<KeymapAction>` performs the entry.
//!   2. Unbound, focused element's `on_key_down`:
//!        Terminal mode -> `terminal_key_down`: a lenient table match (the
//!                         TUI's tolerance), PTY bytes via `encode_pty`, or
//!                         "text" (step 3), or "not ours" (Cmd shortcuts).
//!        App mode      -> `app_key_down`: a lenient table match or nothing.
//!   3. Text: the platform's input handler (`EntityInputHandler`) receives the
//!      committed characters — IME composition included — and `ImeState`
//!      turns them into UTF-8 for the PTY.
//! ```
//!
//! ## Key contexts
//!
//! A table [`Context`](flightdeck::app::keymap::Context) is a GPUI key context
//! of the same name ([`Context::name`](flightdeck::app::keymap::Context::name)):
//!
//! - `"Global"` goes on the window's root element, so its bindings apply in
//!   both modes but lose to anything deeper (a text field in an overlay keeps
//!   its own Alt-Left). A context-less GPUI binding would do the opposite: GPUI
//!   ranks it above every context, so the table never binds one.
//! - `"Terminal"` goes on the focused terminal element.
//! - `"App"` goes on the element focused in app-command mode. It must never be
//!   an ancestor of a `"Terminal"` element: GPUI matches a context anywhere on
//!   the focus path, so an enclosing `"App"` would make App-only chords (bare
//!   Up, Ctrl-n) fire with a terminal focused.
//!
//! ## macOS Option
//!
//! See [`OptionKey`]: bound chords always treat Option as Alt; what an
//! *unbound* Option+key types into a terminal is a policy.

mod action;
mod keystroke;
mod terminal;

#[cfg(test)]
mod tests;

pub use action::{binding_specs, key_bindings, BindingSpec, KeymapAction};
pub use keystroke::{chord_from_keystroke, chord_to_gpui, PLATFORM_MODIFIER};
pub use terminal::{
    app_key_down, paste_bytes, terminal_key_down, ImeState, OptionKey, TerminalKey,
};

use flightdeck::app::keymap::Keymap;
use gpui::App;

/// Bind every chord in `keymap` as a GPUI key binding.
///
/// Call once at start-up, after `gpui_component::init` (so on equal context
/// depth our bindings, added later, win ties the way GPUI orders them).
pub fn register(cx: &mut App, keymap: &Keymap) {
    cx.bind_keys(key_bindings(keymap));
}
