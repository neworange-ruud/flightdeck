//! xterm mouse-report encoding, shared by every front-end that forwards mouse
//! events to a mouse-aware program (SPECS §20).
//!
//! Lives next to [`super::TerminalGrid`] because the encoding to use is a mode
//! the grid reports ([`super::TerminalModes::mouse_encoding`]); the TUI and the
//! desktop app both encode through here so an agent sees the same bytes from
//! either window.

use super::MouseEncoding;

/// Encode a mouse report for the hosted application, matching its active mouse
/// encoding. `cb` is the xterm protocol button code; `col`/`row` are 0-based
/// cell coordinates within the terminal viewport (protocol coordinates are
/// 1-based).
pub fn encode_mouse_report(encoding: MouseEncoding, cb: u8, col: u16, row: u16) -> Vec<u8> {
    let cx = col.saturating_add(1);
    let cy = row.saturating_add(1);
    match encoding {
        MouseEncoding::Sgr => format!("\x1b[<{cb};{cx};{cy}M").into_bytes(),
        // Default (X10) and, approximately, the legacy UTF-8 encoding: one
        // printable byte per field, offset by 32 and clamped to a single byte.
        _ => {
            let bx = cx.saturating_add(32).min(255) as u8;
            let by = cy.saturating_add(32).min(255) as u8;
            vec![0x1b, b'[', b'M', cb.saturating_add(32), bx, by]
        }
    }
}

/// Encode a mouse button press/drag/release report for a mouse-aware hosted
/// application. `cb` is the xterm button code (0 = left, +32 = motion/drag);
/// `pressed` distinguishes press/drag (`true`) from release (`false`). `col`/
/// `row` are 0-based viewport cells (protocol coordinates are 1-based).
pub fn encode_mouse_button(
    encoding: MouseEncoding,
    cb: u8,
    col: u16,
    row: u16,
    pressed: bool,
) -> Vec<u8> {
    let cx = col.saturating_add(1);
    let cy = row.saturating_add(1);
    match encoding {
        // SGR reports the same button code for release but terminate with 'm'.
        MouseEncoding::Sgr => {
            let end = if pressed { 'M' } else { 'm' };
            format!("\x1b[<{cb};{cx};{cy}{end}").into_bytes()
        }
        // X10 has no release button code — release is reported as button 3.
        _ => {
            let code = if pressed { cb } else { 3 };
            let bx = cx.saturating_add(32).min(255) as u8;
            let by = cy.saturating_add(32).min(255) as u8;
            vec![0x1b, b'[', b'M', code.saturating_add(32), bx, by]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_button_press_and_release_differ_only_in_the_final_byte() {
        assert_eq!(
            encode_mouse_button(MouseEncoding::Sgr, 0, 9, 4, true),
            b"\x1b[<0;10;5M".to_vec()
        );
        assert_eq!(
            encode_mouse_button(MouseEncoding::Sgr, 0, 9, 4, false),
            b"\x1b[<0;10;5m".to_vec()
        );
    }

    #[test]
    fn x10_release_is_button_three() {
        assert_eq!(
            encode_mouse_button(MouseEncoding::Default, 0, 0, 0, false),
            vec![0x1b, b'[', b'M', 32 + 3, 33, 33]
        );
    }
}
