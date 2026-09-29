//! Chord-to-bytes and paste encoding for Terminal-mode passthrough.
//!
//! Every front-end that types into a PTY encodes through [`encode_pty`] (keys)
//! and [`encode_paste`] (pasted text), so the bytes an agent receives do not
//! depend on which window the user typed in.

use super::chord::{Chord, Key, Mods};

/// Encode a chord to the bytes the active PTY receives (Terminal mode).
///
/// A best-effort VT100/ANSI encoding, byte for byte what the TUI has always
/// sent:
///
/// - Characters are UTF-8. Ctrl+letter is `0x01..=0x1a`; Ctrl with anything
///   else sends the character unchanged. Alt prefixes `ESC`. Shift changes
///   nothing (the character already carries it).
/// - **Arrows are always CSI (`ESC [ A..D`), never SS3 (`ESC O A..D`),
///   whatever the hosted application's DECCKM (application cursor keys) state.**
///   This is deliberate and load-bearing: the remote-control keystroke
///   injection in [`crate::remote::commands`] drives agents' list prompts with
///   the same CSI bytes and relies on them matching the physical keyboard
///   (remote-control-dc9). Do not make this DECCKM-aware in one front-end only.
/// - Home/End are `ESC [ H` / `ESC [ F`; F1–F4 are SS3 (`ESC O P..S`), F5–F12
///   CSI `~`; F13 and up send nothing.
/// - Modifiers other than Ctrl/Alt on characters are **not** encoded on named
///   keys: Ctrl-Up sends the same bytes as Up.
pub fn encode_pty(chord: Chord) -> Vec<u8> {
    let ctrl = chord.mods.contains(Mods::CTRL);
    let alt = chord.mods.contains(Mods::ALT);

    match chord.key {
        Key::Char(c) => {
            let mut bytes = Vec::new();
            if alt {
                bytes.push(0x1b); // ESC prefix for Alt
            }
            if ctrl {
                // Ctrl+letter → 0x01..0x1a. The `as u8` truncation is the
                // historical behaviour and is kept byte for byte.
                let b = c.to_ascii_uppercase() as u8;
                if b.is_ascii_uppercase() {
                    bytes.push(b - b'A' + 1);
                } else {
                    bytes.extend_from_slice(c.encode_utf8(&mut [0u8; 4]).as_bytes());
                }
            } else {
                bytes.extend_from_slice(c.encode_utf8(&mut [0u8; 4]).as_bytes());
            }
            bytes
        }
        Key::Enter => vec![b'\r'],
        Key::Backspace => vec![0x7f],
        Key::Delete => vec![0x1b, b'[', b'3', b'~'],
        Key::Tab => vec![b'\t'],
        Key::BackTab => vec![0x1b, b'[', b'Z'],
        Key::Esc => vec![0x1b],
        Key::Up => vec![0x1b, b'[', b'A'],
        Key::Down => vec![0x1b, b'[', b'B'],
        Key::Right => vec![0x1b, b'[', b'C'],
        Key::Left => vec![0x1b, b'[', b'D'],
        Key::Home => vec![0x1b, b'[', b'H'],
        Key::End => vec![0x1b, b'[', b'F'],
        Key::PageUp => vec![0x1b, b'[', b'5', b'~'],
        Key::PageDown => vec![0x1b, b'[', b'6', b'~'],
        Key::F(n) => match n {
            // F1-F4 use SS3; F5+ use CSI ~ sequences.
            1 => vec![0x1b, b'O', b'P'],
            2 => vec![0x1b, b'O', b'Q'],
            3 => vec![0x1b, b'O', b'R'],
            4 => vec![0x1b, b'O', b'S'],
            5 => vec![0x1b, b'[', b'1', b'5', b'~'],
            6 => vec![0x1b, b'[', b'1', b'7', b'~'],
            7 => vec![0x1b, b'[', b'1', b'8', b'~'],
            8 => vec![0x1b, b'[', b'1', b'9', b'~'],
            9 => vec![0x1b, b'[', b'2', b'0', b'~'],
            10 => vec![0x1b, b'[', b'2', b'1', b'~'],
            11 => vec![0x1b, b'[', b'2', b'3', b'~'],
            12 => vec![0x1b, b'[', b'2', b'4', b'~'],
            _ => vec![],
        },
    }
}

/// Encode pasted text for the PTY, as every front-end sends a paste.
///
/// Newlines are normalised to carriage returns (the line break a terminal
/// sends for Enter). When `bracketed` is set — the hosted application enabled
/// bracketed paste mode (DECSET 2004), as Claude Code, OpenCode and modern
/// shells do — the payload is wrapped in the `ESC [200~` / `ESC [201~` guards
/// so the app treats it as one atomic insert rather than executing it line by
/// line. Without the mode the app gets the raw text, exactly as a real
/// terminal emulator forwards a paste.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        let mut bytes = Vec::with_capacity(normalized.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(normalized.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        normalized.into_bytes()
    }
}
