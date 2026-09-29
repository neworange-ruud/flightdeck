//! Crate [`Chord`] ↔ GPUI keystroke, both directions, as pure functions.
//!
//! GPUI spells a keystroke `[ctrl-][alt-][cmd-][shift-]key` with lowercase
//! key names (`ctrl-g`, `alt-up`, `shift-escape`, `f2`) and reports a typed
//! key as a [`Keystroke`]: the key *on the keycap* plus the modifiers held,
//! and separately `key_char`, the character the press would type. On macOS,
//! Option+o arrives as `key: "o"`, `alt`, `key_char: Some("ø")` — so lifting
//! from `key` (never `key_char`) is what makes Option act as Alt for chords.

use flightdeck::app::keymap::{Chord, Key, Mods};
use flightdeck::tui::platform::{IS_MACOS, IS_WINDOWS};
use gpui::Keystroke;

/// How GPUI spells the platform modifier (Command / Super / Windows key) on
/// this OS, as `Keystroke::unparse` prints it. Parsing accepts all three.
/// (Linux and any other target get "super", as GPUI's `unparse` prints there.)
pub const PLATFORM_MODIFIER: &str = if IS_MACOS {
    "cmd"
} else if IS_WINDOWS {
    "win"
} else {
    "super"
};

/// The GPUI keystroke string for `chord`, e.g. `Ctrl-g` → `"ctrl-g"`.
///
/// Modifiers come in GPUI's own order (ctrl, alt, platform, shift) so the
/// result equals `Keystroke::parse(s).unparse()`. An uppercase letter is
/// spelled as Shift + the lowercase key, which is how GPUI reports it, and
/// [`Key::BackTab`] is `shift-tab`.
///
/// `None` when GPUI has no spelling: Hyper and Meta have no GPUI modifier.
/// Every chord in the keymap table has one (a test enumerates them).
pub fn chord_to_gpui(chord: Chord) -> Option<String> {
    if chord.mods.intersects(Mods::HYPER | Mods::META) {
        return None;
    }
    let mut shift = chord.mods.contains(Mods::SHIFT);
    let key = match chord.key {
        Key::Char(' ') => "space".to_string(),
        Key::Char(c) if c.is_ascii_uppercase() => {
            shift = true;
            c.to_ascii_lowercase().to_string()
        }
        Key::Char(c) => c.to_string(),
        Key::Enter => "enter".into(),
        Key::Esc => "escape".into(),
        Key::Tab => "tab".into(),
        Key::BackTab => {
            shift = true;
            "tab".into()
        }
        Key::Backspace => "backspace".into(),
        Key::Delete => "delete".into(),
        Key::Up => "up".into(),
        Key::Down => "down".into(),
        Key::Left => "left".into(),
        Key::Right => "right".into(),
        Key::Home => "home".into(),
        Key::End => "end".into(),
        Key::PageUp => "pageup".into(),
        Key::PageDown => "pagedown".into(),
        Key::F(n) => format!("f{n}"),
    };

    let mut out = String::new();
    if chord.mods.contains(Mods::CTRL) {
        out.push_str("ctrl-");
    }
    if chord.mods.contains(Mods::ALT) {
        out.push_str("alt-");
    }
    if chord.mods.contains(Mods::SUPER) {
        out.push_str(PLATFORM_MODIFIER);
        out.push('-');
    }
    if shift {
        out.push_str("shift-");
    }
    out.push_str(&key);
    Some(out)
}

/// Lift a typed GPUI keystroke into the crate's [`Chord`], the vocabulary
/// [`flightdeck::app::keymap::Keymap::lookup`] and
/// [`flightdeck::app::keymap::encode_pty`] speak.
///
/// Mirrors the TUI's crossterm adapter (`tui::input::chord_from_key_event`) so
/// the same physical key becomes the same chord in both front-ends:
///
/// - The chord is built from `key` (the keycap), not `key_char`: macOS
///   Option+1 is `Alt-1`, not `¡`.
/// - Shift + a letter is the **uppercase** character with Shift set, as
///   crossterm reports it (`Char('A')` + SHIFT; Alt-Shift-a is `ESC A`).
///   With Ctrl held it stays lowercase (`Char('g')` + CTRL + SHIFT): that is
///   how the TUI sees Ctrl-Shift-g from a legacy terminal (which cannot send
///   Shift with Ctrl) and from the kitty protocol, so the table's lenient
///   Ctrl-g still matches it. The bytes are the same either way (Ctrl folds
///   case). Other shifted characters are whatever GPUI reports as `key` (on
///   macOS the shifted glyph, `!`).
/// - Shift+Tab is [`Key::BackTab`] (with Shift), the key that encodes to
///   `ESC [ Z`.
/// - Cmd / Super / Win is [`Mods::SUPER`]. GPUI's `fn` modifier is dropped:
///   crossterm never reports it, and on macOS it is set for keys (Home, F-keys
///   on a laptop) whose chord does not depend on it.
///
/// `None` for keys the table and the encoder have no name for (Insert, Menu,
/// a bare modifier, a media key): they match nothing and type nothing, as in
/// the TUI.
pub fn chord_from_keystroke(keystroke: &Keystroke) -> Option<Chord> {
    let m = &keystroke.modifiers;
    let mut mods = Mods::NONE;
    for (held, ours) in [
        (m.shift, Mods::SHIFT),
        (m.control, Mods::CTRL),
        (m.alt, Mods::ALT),
        (m.platform, Mods::SUPER),
    ] {
        if held {
            mods = mods | ours;
        }
    }

    let key = match keystroke.key.as_str() {
        "enter" => Key::Enter,
        "escape" => Key::Esc,
        "tab" if m.shift => Key::BackTab,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "space" => Key::Char(' '),
        name => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                // A single character: the keycap.
                (Some(c), None) if m.shift && !m.control && c.is_ascii_lowercase() => {
                    Key::Char(c.to_ascii_uppercase())
                }
                (Some(c), None) => Key::Char(c),
                // `f1` .. `f35`.
                (Some('f'), Some(_)) => match name[1..].parse::<u8>() {
                    Ok(n) if n >= 1 => Key::F(n),
                    _ => return None,
                },
                _ => return None,
            }
        }
    };
    Some(Chord::new(key, mods))
}
