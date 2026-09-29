//! What an unbound key press does: Terminal-mode PTY bytes, text input (IME),
//! and paste.
//!
//! GPUI offers a key press to the keymap first; only a press no binding
//! claimed reaches an element's `on_key_down`, and only a press that handler
//! leaves unhandled reaches the platform's text input (the IME), which calls
//! back into the focused element's `EntityInputHandler`. The terminal element
//! wires the two ends to [`terminal_key_down`] and [`ImeState`].

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use flightdeck::app::keymap::{encode_paste, encode_pty, Key, Keymap, KeymapEntry, Mods};
use flightdeck::app::modes::InputMode;
use flightdeck::tui::platform::IS_MACOS;
use gpui::{KeyDownEvent, UTF16Selection};

use super::keystroke::chord_from_keystroke;

/// What an *unbound* Option(Alt)+key types into a terminal.
///
/// Bound chords are not affected: GPUI matches bindings on the keycap, so
/// Option+1 is `alt-1` (jump to tab 1) and Option+o is `alt-o` whatever
/// character the layout composes, under either policy. The TUI cannot do
/// that — its host terminal decides — which is why it asks macOS users to turn
/// on "Use Option as Meta"; the native app does not need that setting.
///
/// Only macOS composes with Option. On Linux and Windows Alt never produces a
/// character (AltGr is a separate key there, and Windows flags it with
/// `prefer_character_input`, honoured before this policy is consulted), so
/// both policies send `ESC` + key there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKey {
    /// Option is Meta: `ESC` + the keycap's character, byte for byte what the
    /// TUI sends under a host terminal with "Use Option as Meta" on. Keeps
    /// readline's Meta-b / Meta-f / Meta-d; loses Option-composed characters.
    Meta,
    /// Option composes: the character macOS produces (`∫` for Option-b, `@`
    /// for Option-l on a German layout, a dead-key accent) is typed, as with
    /// the TUI under a host terminal's default setting. Falls back to `Meta`
    /// when the platform reports no composed character.
    Compose,
}

/// `[ui] macos_option_as_meta`, as read once at start-up
/// ([`OptionKey::set_macos_option_as_meta`]). A process-wide setting for the
/// same reason as the leave-focus key: the terminal element asks
/// [`OptionKey::for_this_platform`] on every press, and the setting is only
/// read at launch (a change applies on the next one).
static MACOS_OPTION_AS_META: AtomicBool = AtomicBool::new(false);

impl OptionKey {
    /// The policy for an OS and the `[ui] macos_option_as_meta` setting. Pure,
    /// so both settings can be checked on any host.
    ///
    /// - macOS, setting off (default): [`OptionKey::Compose`], so non-US
    ///   layouts can type the characters they put behind Option (`@ [ ] { } |
    ///   \ ~` on German, French, Nordic layouts …) — without that a shell is
    ///   unusable for their users, while FlightDeck's own Alt chords keep
    ///   working because they are bindings.
    /// - macOS, setting on: [`OptionKey::Meta`], the TUI's bytes under a host
    ///   terminal with "Use Option as Meta" (readline's Meta-b / Meta-f).
    /// - Elsewhere: always [`OptionKey::Meta`] and the setting is ignored,
    ///   because Alt never composes a character there, so both policies are
    ///   the same and Linux/Windows behave identically whatever the file says.
    pub fn resolve(is_macos: bool, macos_option_as_meta: bool) -> OptionKey {
        if is_macos && !macos_option_as_meta {
            OptionKey::Compose
        } else {
            OptionKey::Meta
        }
    }

    /// Record the `[ui] macos_option_as_meta` setting. Call once at start-up,
    /// before the first key press.
    pub fn set_macos_option_as_meta(as_meta: bool) {
        MACOS_OPTION_AS_META.store(as_meta, Ordering::Relaxed);
    }

    /// The policy in force on this OS with the launch-time setting (see
    /// [`OptionKey::resolve`]).
    pub fn for_this_platform() -> OptionKey {
        OptionKey::resolve(IS_MACOS, MACOS_OPTION_AS_META.load(Ordering::Relaxed))
    }
}

/// What the terminal element does with a key press no binding claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalKey<'k> {
    /// A table entry the chord matches leniently (the TUI's `tolerate`, e.g.
    /// Ctrl-Shift-g opening the palette): perform it and stop propagation.
    Action(&'k KeymapEntry),
    /// Write these bytes to the PTY and stop propagation. May be empty (F13+
    /// has no encoding): still stop, so nothing else types it.
    Pty(Vec<u8>),
    /// Printable text: do nothing and let the event propagate, so the
    /// platform's input handler delivers the character (through the IME when
    /// one is composing) to [`ImeState::commit`].
    Text,
    /// Not FlightDeck's key: let it propagate untouched. Cmd/Super/Win
    /// shortcuts (Cmd-Q, Cmd-comma, Cmd-C belong to the platform and the app
    /// menu) and keys with no chord (Insert, Menu, a bare modifier).
    Ignore,
}

/// Route a key press the keymap did not claim, with a terminal focused.
///
/// Byte-identical with the TUI for every key the two can both see: chords
/// come from [`chord_from_keystroke`] (the GPUI twin of the TUI's crossterm
/// adapter) and bytes from [`encode_pty`], the encoder the TUI uses.
///
/// Order:
/// 1. The platform modifier (Cmd on macOS) is never FlightDeck's past the
///    exact bindings GPUI already tried (Cmd-V paste): [`TerminalKey::Ignore`].
/// 2. A key the platform flags as character input (Windows AltGr) is text.
/// 3. A lenient table match in Terminal mode is performed.
/// 4. A printable character with no Ctrl/Alt (Shift allowed) is text; so is
///    an Option+key under [`OptionKey::Compose`] that composed a character.
/// 5. Everything else is encoded for the PTY — including bare Esc, which
///    hosted agents use for their Esc Esc "abort" gesture.
pub fn terminal_key_down<'k>(
    keymap: &'k Keymap,
    event: &KeyDownEvent,
    option: OptionKey,
) -> TerminalKey<'k> {
    let keystroke = &event.keystroke;
    if keystroke.modifiers.platform {
        return TerminalKey::Ignore;
    }
    let composed = keystroke
        .key_char
        .as_deref()
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control));
    if event.prefer_character_input && composed.is_some() {
        return TerminalKey::Text;
    }
    let Some(chord) = chord_from_keystroke(keystroke) else {
        return TerminalKey::Ignore;
    };
    if let Some(entry) = keymap.lookup(InputMode::Terminal, chord) {
        return TerminalKey::Action(entry);
    }
    if let Key::Char(_) = chord.key {
        let beyond_shift = chord.mods.without(Mods::SHIFT);
        if beyond_shift.is_empty() {
            return TerminalKey::Text;
        }
        if beyond_shift == Mods::ALT && option == OptionKey::Compose && composed.is_some() {
            return TerminalKey::Text;
        }
    }
    TerminalKey::Pty(encode_pty(chord))
}

/// Route a key press the keymap did not claim, in app-command mode: the entry
/// it matches leniently (the TUI's `tolerate`), or `None` — an unbound key
/// does nothing in App mode. Cmd chords are left to the platform, as in
/// [`terminal_key_down`].
pub fn app_key_down<'k>(keymap: &'k Keymap, event: &KeyDownEvent) -> Option<&'k KeymapEntry> {
    if event.keystroke.modifiers.platform {
        return None;
    }
    chord_from_keystroke(&event.keystroke).and_then(|chord| keymap.lookup(InputMode::App, chord))
}

/// The bytes a paste of `text` sends to the PTY: newlines as CR, wrapped in
/// `ESC [200~` … `ESC [201~` when the hosted app turned bracketed paste mode
/// on. The TUI's encoder ([`encode_paste`]), so a paste is the same in both.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    encode_paste(text, bracketed)
}

/// The text-input state behind a terminal element's `EntityInputHandler`.
///
/// A terminal has no editable buffer, so the only "document" the platform
/// input handler sees is the IME's in-progress composition (the marked text,
/// e.g. `にほ` before it becomes `日本`). It is shown as a preview and never
/// sent: only committed text reaches the PTY, as UTF-8.
///
/// Delegation, method for method (ranges are UTF-16, as GPUI's are):
///
/// | `EntityInputHandler`             | `ImeState`                          |
/// | -------------------------------- | ----------------------------------- |
/// | `replace_text_in_range`          | [`commit`](Self::commit) → write the bytes to the PTY |
/// | `replace_and_mark_text_in_range` | [`mark`](Self::mark)                |
/// | `marked_text_range`              | [`marked_text_range`](Self::marked_text_range) |
/// | `unmark_text`                    | [`unmark`](Self::unmark)            |
/// | `text_for_range`                 | [`text_for_range`](Self::text_for_range) |
/// | `selected_text_range`            | [`selected_text_range`](Self::selected_text_range) |
/// | `paste`                          | [`paste_bytes`] with the grid's bracketed-paste flag |
///
/// `bounds_for_range` (where the IME candidate window goes) is the element's:
/// the cursor cell's bounds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImeState {
    marked: Option<String>,
}

impl ImeState {
    /// The composition in progress, if any, for the element to draw at the
    /// cursor.
    pub fn marked_text(&self) -> Option<&str> {
        self.marked.as_deref()
    }

    /// The marked range in UTF-16 units: all of the composition.
    pub fn marked_text_range(&self) -> Option<Range<usize>> {
        self.marked.as_deref().map(|m| 0..utf16_len(m))
    }

    /// The caret, at the end of the composition (or 0..0 with none).
    pub fn selected_text_range(&self) -> UTF16Selection {
        let end = self.marked.as_deref().map_or(0, utf16_len);
        UTF16Selection {
            range: end..end,
            reversed: false,
        }
    }

    /// The composition text in `range_utf16`, clamped to it; `adjusted` is
    /// set to the clamped range, as `text_for_range` requires.
    pub fn text_for_range(
        &self,
        range_utf16: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
    ) -> Option<String> {
        let marked = self.marked.as_deref()?;
        let units: Vec<u16> = marked.encode_utf16().collect();
        let start = range_utf16.start.min(units.len());
        let end = range_utf16.end.clamp(start, units.len());
        *adjusted = Some(start..end);
        Some(String::from_utf16_lossy(&units[start..end]))
    }

    /// The IME updated its composition (`replace_and_mark_text_in_range`).
    /// Empty text ends the composition without committing anything.
    pub fn mark(&mut self, text: &str) {
        self.marked = (!text.is_empty()).then(|| text.to_string());
    }

    /// The composition was abandoned (`unmark_text`): drop the preview.
    ///
    /// AppKit's contract says unmarking "accepts" the marked text, but every
    /// IME commits through `insertText` (our [`commit`](Self::commit)) first;
    /// AppKit calls `unmarkText` on its own mostly when focus moves. Dropping
    /// the preview there — as Zed's terminal does — never types a half-formed
    /// word into an agent's prompt.
    pub fn unmark(&mut self) {
        self.marked = None;
    }

    /// Text was committed (`replace_text_in_range`): typed characters, or the
    /// IME's final conversion. Ends any composition and returns the bytes for
    /// the PTY — the text as UTF-8, exactly what the TUI writes for the same
    /// characters.
    pub fn commit(&mut self, text: &str) -> Vec<u8> {
        self.marked = None;
        text.as_bytes().to_vec()
    }
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}
