//! Bytes that bring a fresh terminal to the state a host's grid is in: the
//! screen repaint a truncated replay ends with (remote-control-bkv9), and the
//! scan that finds where a byte stream stops in the middle of a sequence.
//!
//! A viewer that joins late replays only the tail of a terminal's output
//! (`crate::web::stream`). A program that redraws only the cells that changed
//! (OpenCode 2, any diff renderer) left most of its screen in bytes that are no
//! longer there, so replaying the tail paints a few rows and leaves the rest
//! blank or stale until the program happens to touch them. [`repaint_sequence`]
//! writes every row as the host's grid holds it, then the cursor, so the
//! viewer's screen ends exactly like the host's.
//!
//! The modes that go with it are [`super::TerminalModes::restore_sequence`].

use super::{CellAttrs, CellWidth, CursorShape, GridColor, GridView};

/// Every row of `grid`'s viewport as escape sequences and text, then its
/// cursor: position, visibility and shape.
///
/// Rows are written in place with cursor positioning, never with an erase of
/// the whole display, so a main screen's history is not pushed up by it. A
/// row's trailing blank cells are erased rather than written, and nothing is
/// ever written past the last column, so no row wraps and nothing scrolls.
/// The SGR pen is left reset: what the program prints next sets its own.
pub fn repaint_sequence(grid: &dyn GridView) -> Vec<u8> {
    let (rows, cols) = grid.size();
    let mut out = Vec::with_capacity(usize::from(rows) * usize::from(cols) * 2);
    // Hidden while it jumps from row to row; shown again at the end if the
    // program shows it.
    out.extend_from_slice(b"\x1b[?25l\x1b[0m");
    let mut cells: Vec<(String, GridColor, GridColor, CellAttrs, CellWidth)> = Vec::new();
    for row in 0..rows {
        cells.clear();
        grid.visit_row(row, &mut |_, cell| {
            cells.push((
                cell.text.to_string(),
                cell.fg,
                cell.bg,
                cell.attrs,
                cell.width,
            ));
        });
        let used = cells
            .iter()
            .rposition(|(text, _, bg, attrs, width)| !is_blank(text, *bg, attrs, *width))
            .map_or(0, |last| last + 1);
        out.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
        let mut pen: Option<(GridColor, GridColor, CellAttrs)> = None;
        for (text, fg, bg, attrs, width) in &cells[..used] {
            if *width == CellWidth::WideContinuation {
                continue;
            }
            if pen != Some((*fg, *bg, *attrs)) {
                out.extend_from_slice(&sgr(*fg, *bg, attrs));
                pen = Some((*fg, *bg, *attrs));
            }
            if text.is_empty() {
                out.push(b' ');
            } else {
                out.extend_from_slice(text.as_bytes());
            }
        }
        if pen.is_some() {
            out.extend_from_slice(b"\x1b[0m");
        }
        // The rest of the row is blank: erase it. Only when the last write
        // stopped short of the last column, where a pending wrap would make an
        // erase take the last character with it.
        if used < usize::from(cols) {
            out.extend_from_slice(b"\x1b[K");
        }
    }
    let cursor = grid.cursor();
    out.extend_from_slice(format!("\x1b[{};{}H", cursor.row + 1, cursor.col + 1).as_bytes());
    let shape = match (cursor.shape, cursor.blinking) {
        (CursorShape::Block, true) => 1,
        (CursorShape::Block, false) => 2,
        (CursorShape::Underline, true) => 3,
        (CursorShape::Underline, false) => 4,
        (CursorShape::Bar, true) => 5,
        (CursorShape::Bar, false) => 6,
    };
    out.extend_from_slice(format!("\x1b[{shape} q").as_bytes());
    if cursor.visible {
        out.extend_from_slice(b"\x1b[?25h");
    }
    out
}

/// A cell an erase reproduces: no text and nothing an erase would not paint.
fn is_blank(text: &str, bg: GridColor, attrs: &CellAttrs, width: CellWidth) -> bool {
    width != CellWidth::WideContinuation
        && (text.is_empty() || text == " ")
        && bg == GridColor::Default
        && !attrs.inverse
        && !attrs.underline
        && !attrs.strikethrough
}

/// One complete SGR sequence for a cell's colours and attributes, from a
/// reset, so it never depends on the pen before it.
fn sgr(fg: GridColor, bg: GridColor, attrs: &CellAttrs) -> Vec<u8> {
    let mut s = String::from("\x1b[0");
    for (on, code) in [
        (attrs.bold, "1"),
        (attrs.dim, "2"),
        (attrs.italic, "3"),
        (attrs.underline, "4"),
        (attrs.inverse, "7"),
        (attrs.strikethrough, "9"),
    ] {
        if on {
            s.push(';');
            s.push_str(code);
        }
    }
    color(&mut s, fg, 30, 90, 38);
    color(&mut s, bg, 40, 100, 48);
    s.push('m');
    s.into_bytes()
}

fn color(s: &mut String, c: GridColor, base: u8, bright: u8, extended: u8) {
    match c {
        GridColor::Default => {}
        GridColor::Indexed(n) if n < 8 => s.push_str(&format!(";{}", base + n)),
        GridColor::Indexed(n) if n < 16 => s.push_str(&format!(";{}", bright + n - 8)),
        GridColor::Indexed(n) => s.push_str(&format!(";{extended};5;{n}")),
        GridColor::Rgb(r, g, b) => s.push_str(&format!(";{extended};2;{r};{g};{b}")),
    }
}

/// How many bytes at the end of `bytes` are the start of something not yet
/// finished: an escape sequence (CSI, OSC, DCS, APC, PM, SOS or a two-byte
/// escape) still waiting for its last byte, or a UTF-8 character still waiting
/// for its continuation bytes. Zero when `bytes` ends between sequences.
///
/// A PTY read can stop anywhere, so a byte stream's end is often inside a
/// sequence. A parser that has read `bytes` is then waiting for the rest; any
/// other bytes fed to it before the rest would be swallowed into the sequence.
/// So bytes meant to land *between* sequences (a repaint) go before this tail.
///
/// Only the last [`TAIL_SCAN`] bytes are looked at: a sequence longer than
/// that (a huge OSC) is not seen as unfinished, which costs one garbled
/// repaint, never a wrong offset.
pub fn incomplete_tail(bytes: &[u8]) -> usize {
    let window_start = bytes.len().saturating_sub(TAIL_SCAN);
    let window = &bytes[window_start..];
    if let Some(mut esc) = window.iter().rposition(|&b| b == 0x1b) {
        // A lone ESC may be the first half of a string's terminator: then the
        // unfinished part starts at the string.
        if esc + 1 == window.len() {
            if let Some(start) = window[..esc].iter().rposition(|&b| b == 0x1b) {
                if !escape_complete(&window[start..esc]) {
                    esc = start;
                }
            }
        }
        if !escape_complete(&window[esc..]) {
            return window.len() - esc;
        }
        // The last escape is finished; a UTF-8 character after it may not be.
        return utf8_tail(&window[esc..]);
    }
    utf8_tail(window)
}

/// How far back [`incomplete_tail`] looks.
pub const TAIL_SCAN: usize = 4096;

/// Whether `seq` (starting at an ESC, with no ESC after it except as the start
/// of a string terminator) is a finished sequence.
fn escape_complete(seq: &[u8]) -> bool {
    debug_assert_eq!(seq.first(), Some(&0x1b));
    let Some(&kind) = seq.get(1) else {
        return false;
    };
    match kind {
        // CSI: parameters and intermediates, then a final byte.
        b'[' => seq[2..].iter().any(|&b| (0x40..=0x7e).contains(&b)),
        // Strings: OSC ends with BEL or ST; DCS, SOS, PM and APC with ST. The
        // ESC of an ST is the last ESC only when the string before it ended
        // here, which the `\\` case below answers.
        b']' => seq[2..].contains(&0x07),
        b'P' | b'X' | b'^' | b'_' => false,
        // The second half of an ST (`ESC \`), or any other two-byte escape.
        // Intermediates (`ESC ( B`) wait for their final byte.
        0x20..=0x2f => seq[2..].iter().any(|&b| (0x30..=0x7e).contains(&b)),
        _ => true,
    }
}

/// How many bytes at the end of `bytes` are an unfinished UTF-8 character.
fn utf8_tail(bytes: &[u8]) -> usize {
    // A character is at most 4 bytes: its lead is within the last 4.
    for back in 1..=bytes.len().min(4) {
        let b = bytes[bytes.len() - back];
        if b & 0xc0 == 0x80 {
            continue; // a continuation byte: keep looking for the lead
        }
        let needed = match b {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => return 0,
        };
        return if back < needed { back } else { 0 };
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::grid::{Emulator, TerminalGrid};

    fn fed(emulator: Emulator, rows: u16, cols: u16, bytes: &[u8]) -> Box<dyn TerminalGrid> {
        let mut grid = emulator.build(rows, cols);
        grid.process(bytes);
        grid
    }

    fn rows_of(grid: &dyn TerminalGrid) -> Vec<Vec<crate::terminal::grid::OwnedCell>> {
        let (rows, cols) = grid.size();
        (0..rows)
            .map(|r| {
                (0..cols)
                    .map(|c| grid.cell(r, c).unwrap_or_default())
                    .collect()
            })
            .collect()
    }

    /// A screen with colours, attributes, wide characters, a full last column
    /// and blanks with a background: a fresh grid fed the repaint holds the
    /// same cells and cursor, across both emulators.
    #[test]
    fn a_repaint_reproduces_the_screen_and_the_cursor() {
        let screen = "\x1b[1;31mred\x1b[0m plain \x1b[38;2;1;2;3;48;5;200mrgb\x1b[0m\r\n\
                      \x1b[7minverse\x1b[0m 中文 \x1b[44m   \x1b[0m\r\n\
                      \x1b[3;1H0123456789abcdefghij\
                      \x1b[4;3H\x1b[4;9munder\x1b[0m\x1b[2;5H\x1b[5 q";
        for host in [Emulator::Vt100, Emulator::Alacritty] {
            let source = fed(host, 4, 20, screen.as_bytes());
            let repaint = repaint_sequence(source.as_ref());
            for viewer in [Emulator::Vt100, Emulator::Alacritty] {
                // A viewer whose screen holds something else entirely.
                let mut target = fed(viewer, 4, 20, b"\x1b[42mstale stale stale!\r\nxx");
                target.process(&repaint);
                let mut want = rows_of(source.as_ref());
                let mut got = rows_of(target.as_ref());
                if host == Emulator::Vt100 || viewer == Emulator::Vt100 {
                    // vt100 does not track strikethrough.
                    for cell in want.iter_mut().chain(got.iter_mut()).flatten() {
                        cell.attrs.strikethrough = false;
                    }
                }
                // Never-written and erased cells are both blank.
                for cell in want.iter_mut().chain(got.iter_mut()).flatten() {
                    if cell.text == " " {
                        cell.text.clear();
                    }
                }
                assert_eq!(got, want, "{host:?} → {viewer:?}");
                let (s, t) = (source.cursor(), target.cursor());
                assert_eq!((t.row, t.col, t.visible), (s.row, s.col, s.visible));
                assert_eq!(t.shape, s.shape, "{host:?} → {viewer:?}");
            }
        }
    }

    /// Repainting a main screen leaves its history as it was: nothing scrolls.
    #[test]
    fn a_repaint_never_scrolls_the_main_screen() {
        let mut target = Emulator::Alacritty.build(3, 10);
        target.process(b"a\r\nb\r\nc\r\nd\r\ne");
        let history = target.scrollback_len();
        let source = fed(
            Emulator::Alacritty,
            3,
            10,
            b"0123456789\r\n0123456789\r\n0123456789",
        );
        target.process(&repaint_sequence(source.as_ref()));
        assert_eq!(target.scrollback_len(), history);
        assert_eq!(rows_of(target.as_ref()), rows_of(source.as_ref()));
    }

    /// A hidden cursor stays hidden.
    #[test]
    fn a_hidden_cursor_stays_hidden() {
        let source = fed(Emulator::Alacritty, 2, 10, b"x\x1b[?25l");
        let target = fed(
            Emulator::Alacritty,
            2,
            10,
            &repaint_sequence(source.as_ref()),
        );
        assert!(!target.cursor().visible);
    }

    #[test]
    fn an_unfinished_sequence_or_character_is_the_tail() {
        for (bytes, tail) in [
            (&b"abc"[..], 0),
            (b"abc\x1b", 1),
            (b"abc\x1b[", 2),
            (b"abc\x1b[38;2;1", 8),
            (b"abc\x1b[38;2;1m", 0),
            (b"abc\x1b[?1049h", 0),
            (b"abc\x1b]0;title", 9),
            (b"abc\x1b]0;title\x07", 0),
            (b"abc\x1b]0;title\x1b\\", 0),
            (b"abc\x1bP+q4d73", 8),
            (b"abc\x1b(", 2),
            (b"abc\x1b(B", 0),
            (b"abc\x1b=", 0),
            ("ab\u{4e2d}".as_bytes(), 0),
            (&"ab\u{4e2d}".as_bytes()[..4], 2),
            (&"ab\u{4e2d}".as_bytes()[..3], 1),
            (&"\x1b[0mx\u{1f600}".as_bytes()[..8], 3),
            (b"abc\x1b]0;title\x1b", 10),
            (b"", 0),
        ] {
            assert_eq!(incomplete_tail(bytes), tail, "{bytes:?}");
        }
    }
}
