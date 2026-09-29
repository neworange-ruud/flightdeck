//! xterm mouse-report encoding, shared by every front-end that forwards mouse
//! events to a mouse-aware program (SPECS §20).
//!
//! Lives next to [`super::TerminalGrid`] because the encoding to use is a mode
//! the grid reports ([`super::TerminalModes::mouse_encoding`]); the TUI and the
//! desktop app both encode through here so an agent sees the same bytes from
//! either window.
//!
//! Three encodings, as the grid reports them: SGR (`?1006`), UTF-8 (`?1005`)
//! and the legacy X10 bytes. URXVT (`?1015`) is not an encoding either
//! emulator reports, so it has no case here.

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
        MouseEncoding::Utf8 => legacy_utf8(cb, cx, cy),
        MouseEncoding::Default => legacy(cb, cx, cy),
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
        // The legacy encodings have no release button code — a release is
        // reported as button 3.
        MouseEncoding::Utf8 => legacy_utf8(if pressed { cb } else { 3 }, cx, cy),
        MouseEncoding::Default => legacy(if pressed { cb } else { 3 }, cx, cy),
    }
}

/// The legacy `ESC [ M Cb Cx Cy` report: one byte per field, offset by 32. A
/// coordinate past 223 cannot be expressed in one byte and is clamped to the
/// last one (the encoding's own limit).
fn legacy(cb: u8, cx: u16, cy: u16) -> Vec<u8> {
    let byte = |v: u16| v.saturating_add(32).min(255) as u8;
    vec![0x1b, b'[', b'M', cb.saturating_add(32), byte(cx), byte(cy)]
}

/// The `?1005` form of [`legacy`]: each field, offset by 32, is written as a
/// UTF-8 character, so values from 128 up take two bytes and coordinates reach
/// 2015 (the largest two-byte code point, 2047, minus the offset; xterm's
/// limit too). Values below 128 are the same single byte as [`legacy`].
fn legacy_utf8(cb: u8, cx: u16, cy: u16) -> Vec<u8> {
    let mut out = vec![0x1b, b'[', b'M'];
    for v in [u16::from(cb), cx, cy] {
        let code = u32::from(v.saturating_add(32).min(0x7ff));
        // Every value up to 0x7ff is a scalar value (surrogates start at
        // 0xd800), so this never falls back.
        let c = char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER);
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    out
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

    #[test]
    fn utf8_matches_the_legacy_bytes_below_128_and_widens_above() {
        // Near the origin the two encodings are the same bytes.
        for (cb, col, row) in [(0, 0, 0), (2, 40, 20), (64, 94, 94)] {
            assert_eq!(
                encode_mouse_report(MouseEncoding::Utf8, cb, col, row),
                encode_mouse_report(MouseEncoding::Default, cb, col, row)
            );
        }
        // Column 100 is protocol 101, +32 = 133 = U+0085: two UTF-8 bytes,
        // where the legacy encoding sends the lone byte 133.
        assert_eq!(
            encode_mouse_report(MouseEncoding::Utf8, 0, 100, 0),
            vec![0x1b, b'[', b'M', 32, 0xc2, 0x85, 33]
        );
        // Past the one-byte limit the legacy encoding clamps; UTF-8 does not.
        assert_eq!(
            encode_mouse_report(MouseEncoding::Default, 0, 300, 0)[4],
            255
        );
        let wide = encode_mouse_report(MouseEncoding::Utf8, 0, 300, 0);
        // 301 + 32 = 333 = U+014D.
        assert_eq!(std::str::from_utf8(&wide[4..6]).unwrap(), "\u{14d}");
        // Releases are button 3, as in the legacy encoding.
        assert_eq!(
            encode_mouse_button(MouseEncoding::Utf8, 0, 0, 0, false),
            vec![0x1b, b'[', b'M', 32 + 3, 33, 33]
        );
    }
}
