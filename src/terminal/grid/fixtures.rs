//! Byte-fixture tests run against every [`Emulator`], so both backends are held
//! to the same contract, followed by the tests that pin down where they differ
//! (the evidence behind desktop/NOTES-M0.md's emulator comparison).

use super::*;

/// A fresh grid of `emulator`, fed `bytes`.
fn fed(emulator: Emulator, rows: u16, cols: u16, bytes: &[u8]) -> Box<dyn TerminalGrid> {
    let mut grid = emulator.build(rows, cols);
    grid.process(bytes);
    grid
}

/// The shared contract, instantiated once per emulator.
macro_rules! contract {
    ($name:ident, $emulator:expr) => {
        mod $name {
            use super::*;
            const EMU: Emulator = $emulator;

            #[test]
            fn plain_text_lands_on_the_grid() {
                let grid = fed(EMU, 3, 20, b"hello\r\nworld");
                assert_eq!(grid.size(), (3, 20));
                assert_eq!(grid.contents(), "hello\nworld\n");
                assert_eq!(grid.cell(0, 0).unwrap().text, "h");
            }

            #[test]
            fn sgr_colours_map_to_indexed_and_rgb() {
                let grid = fed(
                    EMU,
                    2,
                    20,
                    b"\x1b[31mR\x1b[0m\x1b[38;5;208mX\x1b[38;2;1;2;3mT\x1b[0;44mB\x1b[0mD",
                );
                assert_eq!(grid.cell(0, 0).unwrap().fg, GridColor::Indexed(1));
                assert_eq!(grid.cell(0, 1).unwrap().fg, GridColor::Indexed(208));
                assert_eq!(grid.cell(0, 2).unwrap().fg, GridColor::Rgb(1, 2, 3));
                let b = grid.cell(0, 3).unwrap();
                assert_eq!((b.fg, b.bg), (GridColor::Default, GridColor::Indexed(4)));
                let d = grid.cell(0, 4).unwrap();
                assert_eq!((d.fg, d.bg), (GridColor::Default, GridColor::Default));
            }

            #[test]
            fn bright_ansi_colours_are_indexed_8_to_15() {
                let grid = fed(EMU, 2, 20, b"\x1b[91mr\x1b[104mb");
                assert_eq!(grid.cell(0, 0).unwrap().fg, GridColor::Indexed(9));
                assert_eq!(grid.cell(0, 1).unwrap().bg, GridColor::Indexed(12));
            }

            #[test]
            fn attributes_are_reported_per_cell() {
                let grid = fed(
                    EMU,
                    2,
                    20,
                    b"\x1b[1mb\x1b[0;2md\x1b[0;3mi\x1b[0;4mu\x1b[0;7mv\x1b[0mn",
                );
                let attrs = |c| grid.cell(0, c).unwrap().attrs;
                assert!(attrs(0).bold && !attrs(0).dim);
                assert!(attrs(1).dim && !attrs(1).bold);
                assert!(attrs(2).italic);
                assert!(attrs(3).underline);
                assert!(attrs(4).inverse);
                assert_eq!(attrs(5), CellAttrs::default());
            }

            #[test]
            fn wide_glyphs_take_two_cells() {
                let grid = fed(EMU, 2, 20, "a宽b😀c".as_bytes());
                let cell = |c| grid.cell(0, c).unwrap();
                assert_eq!(cell(0).text, "a");
                assert_eq!(
                    (cell(1).text.as_str(), cell(1).width),
                    ("宽", CellWidth::Wide)
                );
                assert_eq!(cell(2).width, CellWidth::WideContinuation);
                assert_eq!(cell(2).text, "");
                assert_eq!(cell(3).text, "b");
                assert_eq!(
                    (cell(4).text.as_str(), cell(4).width),
                    ("😀", CellWidth::Wide)
                );
                assert_eq!(cell(5).width, CellWidth::WideContinuation);
                assert_eq!(cell(6).text, "c");
                assert_eq!(grid.row_text(0, 0, 19), "a宽b😀c");
            }

            #[test]
            fn combining_marks_join_their_base_cell() {
                // "e" + COMBINING ACUTE ACCENT is one cell holding both.
                let grid = fed(EMU, 2, 20, "e\u{301}x".as_bytes());
                assert_eq!(grid.cell(0, 0).unwrap().text, "e\u{301}");
                assert_eq!(grid.cell(0, 1).unwrap().text, "x");
            }

            #[test]
            fn box_drawing_is_narrow() {
                let grid = fed(EMU, 2, 20, "┌─┐".as_bytes());
                for (col, ch) in ["┌", "─", "┐"].into_iter().enumerate() {
                    let cell = grid.cell(0, col as u16).unwrap();
                    assert_eq!((cell.text.as_str(), cell.width), (ch, CellWidth::Narrow));
                }
            }

            #[test]
            fn cursor_position_and_visibility() {
                let mut grid = fed(EMU, 10, 20, b"abc");
                let c = grid.cursor();
                assert_eq!((c.row, c.col, c.visible), (0, 3, true));
                grid.process(b"\x1b[5;10H");
                assert_eq!((grid.cursor().row, grid.cursor().col), (4, 9));
                grid.process(b"\x1b[?25l");
                assert!(!grid.cursor().visible);
                grid.process(b"\x1b[?25h");
                assert!(grid.cursor().visible);
            }

            #[test]
            fn cursor_shape_follows_decscusr() {
                let mut grid = fed(EMU, 4, 20, b"\x1b[6 q");
                assert_eq!(
                    (grid.cursor().shape, grid.cursor().blinking),
                    (CursorShape::Bar, false)
                );
                grid.process(b"\x1b[3 q");
                assert_eq!(
                    (grid.cursor().shape, grid.cursor().blinking),
                    (CursorShape::Underline, true)
                );
                grid.process(b"\x1b[2 q");
                assert_eq!(
                    (grid.cursor().shape, grid.cursor().blinking),
                    (CursorShape::Block, false)
                );
            }

            #[test]
            fn alternate_screen_enters_and_restores() {
                let mut grid = fed(EMU, 4, 20, b"main text");
                assert!(!grid.modes().alt_screen);
                grid.process(b"\x1b[?1049h\x1b[2J\x1b[Hfull screen app");
                assert!(grid.modes().alt_screen);
                assert_eq!(grid.row_text(0, 0, 19), "full screen app");
                grid.process(b"\x1b[?1049l");
                assert!(!grid.modes().alt_screen);
                assert_eq!(grid.row_text(0, 0, 19), "main text");
            }

            #[test]
            fn bracketed_paste_mode_is_tracked() {
                let mut grid = fed(EMU, 4, 20, b"");
                assert!(!grid.modes().bracketed_paste);
                grid.process(b"\x1b[?2004h");
                assert!(grid.modes().bracketed_paste);
                grid.process(b"\x1b[?2004l");
                assert!(!grid.modes().bracketed_paste);
            }

            #[test]
            fn mouse_modes_and_encodings_are_tracked() {
                let mut grid = fed(EMU, 4, 20, b"");
                assert!(!grid.modes().wants_mouse());
                grid.process(b"\x1b[?1000h\x1b[?1006h");
                let m = grid.modes();
                assert_eq!(
                    (m.mouse_mode, m.mouse_encoding),
                    (MouseMode::PressRelease, MouseEncoding::Sgr)
                );
                grid.process(b"\x1b[?1002h");
                assert_eq!(grid.modes().mouse_mode, MouseMode::ButtonMotion);
                grid.process(b"\x1b[?1003h");
                assert_eq!(grid.modes().mouse_mode, MouseMode::AnyMotion);
                grid.process(b"\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1006l");
                assert!(!grid.modes().wants_mouse());
                assert_eq!(grid.modes().mouse_encoding, MouseEncoding::Default);
                grid.process(b"\x1b[?1000h\x1b[?1005h");
                assert_eq!(grid.modes().mouse_encoding, MouseEncoding::Utf8);
            }

            #[test]
            fn application_cursor_and_keypad_are_tracked() {
                let mut grid = fed(EMU, 4, 20, b"\x1b[?1h\x1b=");
                assert!(grid.modes().app_cursor);
                assert!(grid.modes().app_keypad);
                grid.process(b"\x1b[?1l\x1b>");
                assert!(!grid.modes().app_cursor);
                assert!(!grid.modes().app_keypad);
            }

            #[test]
            fn scrollback_fills_and_scrolls() {
                let mut grid = EMU.build(5, 20);
                for i in 0..30 {
                    grid.process(format!("line {i:02}\r\n").as_bytes());
                }
                // 30 newlines on a 5-row screen: 4 move the cursor down, 26
                // scroll a line into history.
                assert_eq!(grid.scrollback_len(), 26);
                assert_eq!(grid.scrollback(), 0);
                assert_eq!(grid.row_text(0, 0, 19), "line 26");

                grid.set_scrollback(3);
                assert_eq!(grid.scrollback(), 3);
                assert_eq!(grid.row_text(0, 0, 19), "line 23");
                grid.set_scrollback(10_000);
                assert_eq!(grid.scrollback(), 26, "clamped to the history");
                assert_eq!(grid.row_text(0, 0, 19), "line 00");
                grid.set_scrollback(0);
                assert_eq!(grid.row_text(0, 0, 19), "line 26");
            }

            #[test]
            fn cursor_position_query_gets_a_report() {
                let mut grid = fed(EMU, 4, 20, b"ab\x1b[6n");
                assert_eq!(grid.take_replies(), b"\x1b[1;3R".to_vec());
                assert!(grid.take_replies().is_empty(), "drained on read");
                grid.process(b"no query here");
                assert!(grid.take_replies().is_empty());
            }

            #[test]
            fn resize_changes_the_size_and_drops_the_selection() {
                let mut grid = fed(EMU, 4, 20, b"hello");
                grid.set_selection(Some(Selection::new(point_at(grid.as_ref(), 0, 0))));
                grid.resize(10, 40);
                assert_eq!(grid.size(), (10, 40));
                assert!(grid.selection().is_none());
                assert_eq!(grid.row_text(0, 0, 39), "hello");
            }

            #[test]
            fn selection_extracts_text_across_history() {
                let mut grid = EMU.build(3, 20);
                grid.process(b"alpha\r\nbravo\r\ncharlie\r\ndelta");
                // History: alpha. Screen: bravo / charlie / delta.
                let mut sel = Selection::new(point_at(grid.as_ref(), 0, 0));
                grid.set_scrollback(1); // alpha is now the top visible row
                sel.anchor = point_at(grid.as_ref(), 0, 2);
                sel.head = point_at(grid.as_ref(), 2, 3);
                grid.set_selection(Some(sel));
                assert_eq!(grid.selected_text().as_deref(), Some("pha\nbravo\nchar"));
                assert_eq!(grid.scrollback(), 1, "offset restored");
            }

            #[test]
            fn hostile_bytes_do_not_panic() {
                let mut grid = EMU.build(4, 20);
                let mut noise = Vec::new();
                for i in 0..4096u32 {
                    noise.push((i.wrapping_mul(2654435761) >> 13) as u8);
                }
                grid.process(&noise);
                grid.resize(4, 20);
                grid.process("\x1b[1;20H漢🎉".as_bytes());
                grid.resize(4, 3);
                grid.process("漢🎉x".as_bytes());
                let _ = grid.contents();
            }
        }
    };
}

contract!(vt100, Emulator::Vt100);
contract!(alacritty, Emulator::Alacritty);

// --- where the emulators differ ----------------------------------------------
// Each test asserts BOTH behaviours, so a library upgrade that changes one
// fails here and the comparison in desktop/NOTES-M0.md gets revisited.

/// Reflow on resize: alacritty rewraps a soft-wrapped line into the new width
/// and back; vt100 truncates rows and the cut text is gone.
#[test]
fn reflow_on_resize() {
    for emulator in [Emulator::Vt100, Emulator::Alacritty] {
        // A soft-wrapped 10-column line with the prompt on the line below it.
        let mut grid = fed(emulator, 4, 10, b"abcdefghij\r\nnext");
        grid.resize(4, 5);
        let narrow = grid.contents();
        let history = grid.scrollback_len();
        grid.resize(4, 10);
        let wide_again = grid.contents();
        match emulator {
            Emulator::Alacritty => {
                // The rewrap keeps the cursor's line in place, so the first half
                // of the long line moves up into history.
                assert_eq!(narrow, "fghij\nnext\n\n");
                assert_eq!(history, 1);
                assert_eq!(wide_again, "abcdefghij\nnext\n\n");
            }
            Emulator::Vt100 => {
                assert_eq!(narrow, "abcde\nnext\n\n");
                assert_eq!(history, 0);
                assert_eq!(wide_again, "abcde\nnext\n\n", "the cut text is gone");
            }
        }
    }
}

/// `?47` / `?1047` (the older alternate-screen modes some programs still use):
/// vt100 honours `?47`; alacritty only implements `?1049`.
#[test]
fn legacy_alternate_screen_modes() {
    let vt = fed(Emulator::Vt100, 4, 20, b"\x1b[?47h");
    assert!(vt.modes().alt_screen);
    let ala = fed(Emulator::Alacritty, 4, 20, b"\x1b[?47h");
    assert!(!ala.modes().alt_screen);
}

/// X10 mouse (`?9`, presses only): vt100 tracks it; alacritty ignores it.
#[test]
fn x10_mouse_mode() {
    let vt = fed(Emulator::Vt100, 4, 20, b"\x1b[?9h");
    assert_eq!(vt.modes().mouse_mode, MouseMode::Press);
    let ala = fed(Emulator::Alacritty, 4, 20, b"\x1b[?9h");
    assert_eq!(ala.modes().mouse_mode, MouseMode::None);
}

/// Colour queries (OSC 11 "what is your background?"), which agents use to pick
/// a light or dark theme: alacritty answers with the front-end's colours; vt100
/// has no reply channel for them and stays silent.
#[test]
fn background_colour_query() {
    let mut ala = Emulator::Alacritty.build(4, 20);
    ala.set_default_colors((0xe8, 0xe2, 0xd8), (0x16, 0x13, 0x11));
    ala.process(b"\x1b]11;?\x07");
    assert_eq!(
        String::from_utf8(ala.take_replies()).unwrap(),
        "\x1b]11;rgb:1616/1313/1111\x07"
    );
    let mut vt = Emulator::Vt100.build(4, 20);
    vt.process(b"\x1b]11;?\x07");
    assert!(vt.take_replies().is_empty());
}

/// Primary device attributes (`CSI c`): alacritty replies as a VT102-class
/// terminal; vt100 does not answer.
#[test]
fn device_attributes_query() {
    let mut ala = fed(Emulator::Alacritty, 4, 20, b"\x1b[c");
    assert!(!ala.take_replies().is_empty());
    let mut vt = fed(Emulator::Vt100, 4, 20, b"\x1b[c");
    assert!(vt.take_replies().is_empty());
}

/// Palette overrides (OSC 4): alacritty applies them to cells that use the
/// index; vt100 ignores the sequence.
#[test]
fn palette_override() {
    let bytes = b"\x1b]4;1;rgb:12/34/56\x07\x1b[31mR";
    let ala = fed(Emulator::Alacritty, 4, 20, bytes);
    assert_eq!(ala.cell(0, 0).unwrap().fg, GridColor::Rgb(0x12, 0x34, 0x56));
    let vt = fed(Emulator::Vt100, 4, 20, bytes);
    assert_eq!(vt.cell(0, 0).unwrap().fg, GridColor::Indexed(1));
}

/// Strikethrough (SGR 9): only alacritty tracks it.
#[test]
fn strikethrough() {
    assert!(
        fed(Emulator::Alacritty, 2, 10, b"\x1b[9mx")
            .cell(0, 0)
            .unwrap()
            .attrs
            .strikethrough
    );
    assert!(
        !fed(Emulator::Vt100, 2, 10, b"\x1b[9mx")
            .cell(0, 0)
            .unwrap()
            .attrs
            .strikethrough
    );
}

/// Synchronized output (`?2026`): alacritty holds the frame until the end
/// marker, so a half-drawn frame is never visible; vt100 draws as bytes arrive.
#[test]
fn synchronized_update() {
    let bsu = b"\x1b[?2026hhalf";
    let ala = fed(Emulator::Alacritty, 2, 10, bsu);
    assert_eq!(ala.row_text(0, 0, 9), "", "held until ESU");
    let mut ala = ala;
    ala.process(b" done\x1b[?2026l");
    assert_eq!(ala.row_text(0, 0, 9), "half done");

    let vt = fed(Emulator::Vt100, 2, 10, bsu);
    assert_eq!(vt.row_text(0, 0, 9), "half");
}

/// Emoji presentation selector: "❤️" is U+2764 (narrow by Unicode width) +
/// VS16. Both emulators measure the base character alone and give it ONE
/// column, while most fonts draw the emoji two columns wide — the known gap the
/// `*-alacritty-terminal` forks on crates.io patch. A front-end must not assume
/// every emoji cell is Wide.
#[test]
fn emoji_presentation_selector_width() {
    for emulator in [Emulator::Vt100, Emulator::Alacritty] {
        let grid = fed(emulator, 2, 10, "\u{2764}\u{fe0f}x".as_bytes());
        let heart = grid.cell(0, 0).unwrap();
        assert_eq!(heart.width, CellWidth::Narrow, "{emulator:?}");
        assert_eq!(heart.text, "\u{2764}\u{fe0f}", "{emulator:?}");
        assert_eq!(grid.cell(0, 1).unwrap().text, "x", "{emulator:?}");
    }
}
