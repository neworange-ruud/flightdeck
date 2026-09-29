//! Keyboard input for the spike terminal and mouse reports for every desktop
//! terminal, as PTY bytes.
//!
//! Deliberately small: keys are lifted into the core's front-end-neutral
//! [`Chord`] and encoded by [`encode_pty`], the one encoder every FlightDeck
//! front-end types through (arrows are always CSI). The full desktop keymap —
//! bindings, leave-focus chords, IME — is wired elsewhere (`keys.rs`); this only
//! makes the spike window usable. Mouse reports are the core's
//! `encode_mouse_button` / `encode_mouse_report`, the TUI's own encoders, and a
//! paste is the core's `encode_paste`.

use flightdeck::app::keymap::{encode_pty, Chord, Key, Mods};
use flightdeck::terminal::grid::{encode_mouse_button, encode_mouse_report, MouseEncoding};
use gpui::{Keystroke, Modifiers, MouseButton};

/// The bytes a key press sends, or `None` for a key the terminal does not
/// consume (anything with Cmd/Super held, which belongs to the app, and keys
/// with no chord such as a bare modifier).
pub fn encode_keystroke(keystroke: &Keystroke) -> Option<Vec<u8>> {
    let m = &keystroke.modifiers;
    if m.platform {
        return None;
    }
    let mods = chord_mods(m);
    let named = match keystroke.key.as_str() {
        "enter" => Some(Key::Enter),
        "backspace" => Some(Key::Backspace),
        "delete" => Some(Key::Delete),
        "escape" => Some(Key::Esc),
        // Shift+Tab encodes only as BackTab (see `Key::BackTab`).
        "tab" if m.shift => Some(Key::BackTab),
        "tab" => Some(Key::Tab),
        "up" => Some(Key::Up),
        "down" => Some(Key::Down),
        "left" => Some(Key::Left),
        "right" => Some(Key::Right),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" => Some(Key::PageUp),
        "pagedown" => Some(Key::PageDown),
        "space" if m.control => Some(Key::Char(' ')),
        key => key
            .strip_prefix('f')
            .and_then(|n| n.parse::<u8>().ok())
            .filter(|n| (1..=24).contains(n))
            .map(Key::F),
    };
    if let Some(key) = named {
        return non_empty(encode_pty(Chord::new(key, mods)));
    }

    // Ctrl+letter (and Alt-as-Meta off macOS) encode from the key itself: the
    // typed character is absent or already transformed by the OS.
    let key_char = single_char(&keystroke.key);
    if m.control || (m.alt && !flightdeck::tui::platform::IS_MACOS) {
        if let Some(c) = key_char {
            return non_empty(encode_pty(Chord::new(Key::Char(c), mods)));
        }
    }

    // Everything else is the text the key produced (Option+e on macOS is "é"),
    // sent as UTF-8.
    match keystroke.key_char.as_deref() {
        Some(text) if !text.is_empty() => Some(text.as_bytes().to_vec()),
        _ => key_char.map(|c| c.to_string().into_bytes()),
    }
}

fn chord_mods(m: &Modifiers) -> Mods {
    let mut mods = Mods::NONE;
    if m.shift {
        mods = mods | Mods::SHIFT;
    }
    if m.control {
        mods = mods | Mods::CTRL;
    }
    if m.alt {
        mods = mods | Mods::ALT;
    }
    mods
}

fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

fn non_empty(bytes: Vec<u8>) -> Option<Vec<u8>> {
    (!bytes.is_empty()).then_some(bytes)
}

/// The xterm button code for `button`, with the modifier bits a report
/// carries (Shift +4, Alt +8, Ctrl +16). `None` for buttons xterm has no code
/// for (back/forward).
pub fn button_code(button: MouseButton, m: &Modifiers) -> Option<u8> {
    let base = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        _ => return None,
    };
    Some(base | modifier_bits(m))
}

fn modifier_bits(m: &Modifiers) -> u8 {
    (if m.shift { 4 } else { 0 }) | (if m.alt { 8 } else { 0 }) | (if m.control { 16 } else { 0 })
}

/// A press (`pressed`) or release report for a mouse-aware program.
pub fn mouse_button_bytes(
    encoding: MouseEncoding,
    code: u8,
    col: u16,
    row: u16,
    pressed: bool,
) -> Vec<u8> {
    encode_mouse_button(encoding, code, col, row, pressed)
}

/// A motion report: `code` is the held button's code (3 for none), plus the
/// motion bit.
pub fn mouse_motion_bytes(encoding: MouseEncoding, code: u8, col: u16, row: u16) -> Vec<u8> {
    encode_mouse_report(encoding, code + 32, col, row)
}

/// One wheel notch as a report: button 64 (up) or 65 (down).
pub fn wheel_bytes(
    encoding: MouseEncoding,
    up: bool,
    m: &Modifiers,
    col: u16,
    row: u16,
) -> Vec<u8> {
    let code = if up { 64 } else { 65 } | modifier_bits(m);
    encode_mouse_report(encoding, code, col, row)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(key: &str, key_char: Option<&str>, m: Modifiers) -> Keystroke {
        Keystroke {
            modifiers: m,
            key: key.to_string(),
            key_char: key_char.map(str::to_string),
        }
    }

    fn none() -> Modifiers {
        Modifiers::default()
    }

    fn ctrl() -> Modifiers {
        Modifiers {
            control: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn typed_text_is_sent_as_utf8() {
        assert_eq!(
            encode_keystroke(&ks("a", Some("a"), none())),
            Some(b"a".to_vec())
        );
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(
            encode_keystroke(&ks("a", Some("A"), shift)),
            Some(b"A".to_vec())
        );
        assert_eq!(
            encode_keystroke(&ks("e", Some("é"), none())),
            Some("é".as_bytes().to_vec())
        );
        assert_eq!(
            encode_keystroke(&ks("space", Some(" "), none())),
            Some(b" ".to_vec())
        );
    }

    #[test]
    fn named_keys_use_the_core_encoder_and_arrows_are_csi() {
        assert_eq!(
            encode_keystroke(&ks("enter", None, none())),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            encode_keystroke(&ks("backspace", None, none())),
            Some(vec![0x7f])
        );
        assert_eq!(
            encode_keystroke(&ks("up", None, none())),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode_keystroke(&ks("left", None, none())),
            Some(b"\x1b[D".to_vec())
        );
        assert_eq!(
            encode_keystroke(&ks("escape", None, none())),
            Some(vec![0x1b])
        );
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(
            encode_keystroke(&ks("tab", None, shift)),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            encode_keystroke(&ks("f5", None, none())),
            Some(b"\x1b[15~".to_vec())
        );
    }

    #[test]
    fn ctrl_letters_are_control_bytes() {
        assert_eq!(encode_keystroke(&ks("c", None, ctrl())), Some(vec![0x03]));
        assert_eq!(
            encode_keystroke(&ks("a", Some("a"), ctrl())),
            Some(vec![0x01])
        );
    }

    #[test]
    fn command_chords_belong_to_the_app() {
        let cmd = Modifiers {
            platform: true,
            ..Modifiers::default()
        };
        assert_eq!(encode_keystroke(&ks("q", Some("q"), cmd)), None);
    }

    #[test]
    fn mouse_reports_carry_modifier_bits() {
        let m = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        assert_eq!(button_code(MouseButton::Left, &m), Some(16));
        assert_eq!(
            mouse_button_bytes(MouseEncoding::Sgr, 0, 2, 3, true),
            b"\x1b[<0;3;4M".to_vec()
        );
        assert_eq!(
            mouse_motion_bytes(MouseEncoding::Sgr, 0, 2, 3),
            b"\x1b[<32;3;4M".to_vec()
        );
        assert_eq!(
            wheel_bytes(MouseEncoding::Sgr, true, &none(), 0, 0),
            b"\x1b[<64;1;1M".to_vec()
        );
    }
}
