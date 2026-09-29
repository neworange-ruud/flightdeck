//! One frame of a terminal, decided before anything is painted.
//!
//! [`layout`] turns a [`GridView`] into flat lists of what to draw — background
//! spans, selection spans, text spans, geometric box cells and the cursor — in
//! cell coordinates with theme colours already resolved. The GPUI element only
//! measures, shapes and paints what this returns, so everything about WHAT a
//! terminal frame shows is testable without a window (and is what the
//! `--dump-grid` debug flag prints).

use flightdeck::terminal::grid::{CellWidth, CursorShape, GridColor, GridView};
use flightdeck::tui::selection::Selection;

use super::boxdraw::{self, BoxGlyph};
use crate::theme::{Hex, Palette};

/// The colours a terminal paints with, taken from the app theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermPalette {
    pub fg: Hex,
    pub bg: Hex,
    pub ansi: [Hex; 16],
    pub cursor: Hex,
    pub cursor_ink: Hex,
    pub selection: Hex,
}

impl TermPalette {
    pub fn from_palette(p: &Palette) -> Self {
        Self {
            fg: p.terminal_ink,
            bg: p.surface_terminal,
            ansi: p.terminal_ansi,
            cursor: p.terminal_cursor,
            cursor_ink: p.terminal_cursor_ink,
            selection: p.terminal_selection,
        }
    }

    /// The colour a cell colour paints as. `Default` is the theme's
    /// foreground or background, depending on which side it is used for.
    pub fn resolve(&self, color: GridColor, foreground: bool) -> Hex {
        match color {
            GridColor::Default if foreground => self.fg,
            GridColor::Default => self.bg,
            GridColor::Indexed(i) if i < 16 => self.ansi[usize::from(i)],
            GridColor::Indexed(i) => {
                let (r, g, b) = flightdeck::terminal::grid::xterm_rgb(i);
                rgb(r, g, b)
            }
            GridColor::Rgb(r, g, b) => rgb(r, g, b),
        }
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Hex {
    Hex(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b))
}

/// `a` moved `t` (0..=1) of the way to `b`, per channel.
fn mix(a: Hex, b: Hex, t: f32) -> Hex {
    let channel = |shift: u32| {
        let x = ((a.0 >> shift) & 0xff) as f32;
        let y = ((b.0 >> shift) & 0xff) as f32;
        ((x + (y - x) * t).round() as u32) << shift
    };
    Hex(channel(16) | channel(8) | channel(0))
}

/// A horizontal run of cells filled with one colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellSpan {
    pub row: u16,
    pub col: u16,
    pub cols: u16,
    pub color: Hex,
}

/// Text to shape and paint starting at a cell. Narrow spans hold one character
/// per cell; a wide span holds one grapheme drawn across two cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSpan {
    pub row: u16,
    pub col: u16,
    pub text: String,
    pub fg: Hex,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub wide: bool,
}

/// A box-drawing / block cell painted as geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct BoxCell {
    pub row: u16,
    pub col: u16,
    pub glyph: BoxGlyph,
    pub fg: Hex,
}

/// Where and how to draw the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPaint {
    pub row: u16,
    pub col: u16,
    /// 2 on a wide character.
    pub cols: u16,
    pub shape: CursorShape,
    /// A focused block is filled; an unfocused one is an outline.
    pub filled: bool,
}

/// Everything one frame paints, back to front.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FrameLayout {
    pub rows: u16,
    pub cols: u16,
    pub backgrounds: Vec<CellSpan>,
    pub selection: Vec<CellSpan>,
    pub cursor: Option<CursorPaint>,
    pub texts: Vec<TextSpan>,
    pub boxes: Vec<BoxCell>,
}

impl FrameLayout {
    /// The text spans as one string per row (for dumps and tests): spans at
    /// their columns (a wide span takes two), gaps as spaces, box cells as
    /// `#`. Reads like the terminal: columns line up with the grid.
    pub fn text_rows(&self) -> Vec<String> {
        // (row, col, text, columns the text covers)
        let mut items: Vec<(u16, u16, String, u16)> = self
            .texts
            .iter()
            .map(|t| {
                let cols = if t.wide {
                    2
                } else {
                    t.text.chars().count() as u16
                };
                (t.row, t.col, t.text.clone(), cols)
            })
            .chain(
                self.boxes
                    .iter()
                    .map(|b| (b.row, b.col, "#".to_string(), 1)),
            )
            .collect();
        items.sort_by_key(|(r, c, _, _)| (*r, *c));
        let mut rows = vec![(String::new(), 0u16); usize::from(self.rows)];
        for (row, col, text, cols) in items {
            let (line, at) = &mut rows[usize::from(row)];
            while *at < col {
                line.push(' ');
                *at += 1;
            }
            line.push_str(&text);
            *at += cols;
        }
        rows.into_iter().map(|(line, _)| line).collect()
    }
}

/// Style that must match for two adjacent cells to share one text span.
#[derive(Clone, Copy, PartialEq, Eq)]
struct SpanStyle {
    fg: Hex,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
}

/// Lay out one frame of `grid`.
///
/// ASCII runs of one style become one span (the element shapes it with a
/// forced per-glyph advance, so it lands on the grid). Anything else that is
/// narrow gets its own span, so a fallback-font glyph with a different advance
/// cannot push its neighbours off their cells; wide graphemes get their own
/// two-cell span; blanks produce no span at all.
pub fn layout(
    grid: &dyn GridView,
    selection: Option<&Selection>,
    palette: &TermPalette,
    focused: bool,
) -> FrameLayout {
    let (rows, cols) = grid.size();
    let offset = grid.scrollback();
    let cursor = grid.cursor();
    // History has no cursor: only draw it on the live screen.
    let cursor_cell = (cursor.visible && offset == 0 && cursor.row < rows && cursor.col < cols)
        .then_some((cursor.row, cursor.col));
    let filled_cursor = focused && cursor.shape == CursorShape::Block;

    let mut out = FrameLayout {
        rows,
        cols,
        ..FrameLayout::default()
    };

    for row in 0..rows {
        if let Some((c0, c1)) = selection.and_then(|s| s.row_selection(row, rows, cols, offset)) {
            out.selection.push(CellSpan {
                row,
                col: c0,
                cols: c1 - c0 + 1,
                color: palette.selection,
            });
        }

        // The span being extended, if the next cell may join it.
        let mut open: Option<(TextSpan, SpanStyle)> = None;
        grid.visit_row(row, &mut |col, cell| {
            let attrs = cell.attrs;
            let mut fg = palette.resolve(cell.fg, true);
            let mut bg = palette.resolve(cell.bg, false);
            if attrs.inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            if attrs.dim {
                fg = mix(fg, bg, 0.5);
            }
            let under_cursor = cursor_cell == Some((row, col));
            if under_cursor && filled_cursor {
                fg = palette.cursor_ink;
            }
            if under_cursor {
                out.cursor = Some(CursorPaint {
                    row,
                    col,
                    cols: if cell.width == CellWidth::Wide { 2 } else { 1 },
                    shape: cursor.shape,
                    filled: filled_cursor,
                });
            }

            if bg != palette.bg {
                match out.backgrounds.last_mut() {
                    Some(span)
                        if span.row == row && span.col + span.cols == col && span.color == bg =>
                    {
                        span.cols += 1;
                    }
                    _ => out.backgrounds.push(CellSpan {
                        row,
                        col,
                        cols: 1,
                        color: bg,
                    }),
                }
            }

            if cell.width == CellWidth::WideContinuation {
                return;
            }
            let text = cell.text;
            if text.trim().is_empty() {
                if let Some((span, _)) = open.take() {
                    out.texts.push(span);
                }
                return;
            }
            if let Some(glyph) = boxdraw::classify_text(text) {
                if let Some((span, _)) = open.take() {
                    out.texts.push(span);
                }
                out.boxes.push(BoxCell {
                    row,
                    col,
                    glyph,
                    fg,
                });
                return;
            }

            let style = SpanStyle {
                fg,
                bold: attrs.bold,
                italic: attrs.italic,
                underline: attrs.underline,
                strikethrough: attrs.strikethrough,
            };
            let joinable = cell.width == CellWidth::Narrow && text.len() == 1 && text.is_ascii();
            if joinable {
                if let Some((span, open_style)) = open.as_mut() {
                    let next_col = span.col + span.text.len() as u16;
                    if *open_style == style && next_col == col {
                        span.text.push_str(text);
                        return;
                    }
                }
            }
            if let Some((span, _)) = open.take() {
                out.texts.push(span);
            }
            let span = TextSpan {
                row,
                col,
                text: text.to_string(),
                fg,
                bold: attrs.bold,
                italic: attrs.italic,
                underline: attrs.underline,
                strikethrough: attrs.strikethrough,
                wide: cell.width == CellWidth::Wide,
            };
            if joinable {
                open = Some((span, style));
            } else {
                out.texts.push(span);
            }
        });
        if let Some((span, _)) = open.take() {
            out.texts.push(span);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use flightdeck::terminal::grid::{Emulator, TerminalGrid};

    fn palette() -> TermPalette {
        TermPalette::from_palette(&Palette::dark())
    }

    fn laid_out(bytes: &[u8], rows: u16, cols: u16) -> (Box<dyn TerminalGrid>, FrameLayout) {
        let mut grid = Emulator::Alacritty.build(rows, cols);
        grid.process(bytes);
        let frame = layout(grid.as_ref(), None, &palette(), true);
        (grid, frame)
    }

    #[test]
    fn the_spike_fixture_lays_out_on_the_grid() {
        let (_, frame) = laid_out("\x1b[31mred\x1b[0m ┌─┐ 😀 宽\r\n".as_bytes(), 3, 20);
        let p = palette();
        // "red" in ANSI red, one span.
        assert_eq!(frame.texts[0].text, "red");
        assert_eq!(frame.texts[0].fg, p.ansi[1]);
        // The three box cells are geometry at columns 4, 5, 6.
        let box_cols: Vec<u16> = frame.boxes.iter().map(|b| b.col).collect();
        assert_eq!(box_cols, vec![4, 5, 6]);
        // The emoji and the CJK glyph are wide spans at 8 and 11.
        let wide: Vec<(u16, &str)> = frame
            .texts
            .iter()
            .filter(|t| t.wide)
            .map(|t| (t.col, t.text.as_str()))
            .collect();
        assert_eq!(wide, vec![(8, "😀"), (11, "宽")]);
        assert_eq!(frame.text_rows()[0], "red ### 😀 宽");
    }

    #[test]
    fn colours_resolve_through_the_theme() {
        let p = palette();
        assert_eq!(p.resolve(GridColor::Default, true), p.fg);
        assert_eq!(p.resolve(GridColor::Default, false), p.bg);
        assert_eq!(p.resolve(GridColor::Indexed(9), true), p.ansi[9]);
        assert_eq!(p.resolve(GridColor::Indexed(16), true), Hex(0x000000));
        assert_eq!(p.resolve(GridColor::Indexed(231), true), Hex(0xffffff));
        assert_eq!(p.resolve(GridColor::Indexed(244), true), Hex(0x808080));
        assert_eq!(p.resolve(GridColor::Rgb(1, 2, 3), true), Hex(0x010203));
    }

    #[test]
    fn inverse_swaps_and_fills_the_background() {
        let (_, frame) = laid_out(b"\x1b[7mhi\x1b[0m", 2, 10);
        let p = palette();
        assert_eq!(
            frame.backgrounds[0],
            CellSpan {
                row: 0,
                col: 0,
                cols: 2,
                color: p.fg
            }
        );
        assert_eq!(frame.texts[0].fg, p.bg);
    }

    #[test]
    fn style_changes_split_spans_and_blanks_produce_none() {
        let (_, frame) = laid_out(b"ab\x1b[1mcd\x1b[0m  ef", 2, 20);
        let spans: Vec<(u16, &str, bool)> = frame
            .texts
            .iter()
            .map(|t| (t.col, t.text.as_str(), t.bold))
            .collect();
        assert_eq!(
            spans,
            vec![(0, "ab", false), (2, "cd", true), (6, "ef", false)]
        );
    }

    #[test]
    fn a_focused_block_cursor_inks_the_cell_under_it() {
        let (_, frame) = laid_out(b"abc\x1b[1;2H", 2, 10);
        let p = palette();
        let cursor = frame.cursor.expect("cursor on the live screen");
        assert_eq!((cursor.row, cursor.col, cursor.filled), (0, 1, true));
        let b = frame.texts.iter().find(|t| t.col == 1).unwrap();
        assert_eq!((b.text.as_str(), b.fg), ("b", p.cursor_ink));
    }

    #[test]
    fn a_hidden_or_scrolled_cursor_is_not_drawn() {
        let (_, frame) = laid_out(b"abc\x1b[?25l", 2, 10);
        assert!(frame.cursor.is_none());

        let mut grid = Emulator::Alacritty.build(2, 10);
        for _ in 0..5 {
            grid.process(b"x\r\n");
        }
        grid.set_scrollback(1);
        assert!(layout(grid.as_ref(), None, &palette(), true)
            .cursor
            .is_none());
    }

    #[test]
    fn selection_becomes_one_span_per_row() {
        let mut grid = Emulator::Alacritty.build(3, 10);
        grid.process(b"one\r\ntwo\r\nthree");
        let sel = Selection {
            anchor: flightdeck::terminal::grid::point_at(grid.as_ref(), 0, 1),
            head: flightdeck::terminal::grid::point_at(grid.as_ref(), 1, 1),
        };
        let frame = layout(grid.as_ref(), Some(&sel), &palette(), true);
        let spans: Vec<(u16, u16, u16)> = frame
            .selection
            .iter()
            .map(|s| (s.row, s.col, s.cols))
            .collect();
        assert_eq!(spans, vec![(0, 1, 9), (1, 0, 2)]);
    }

    #[test]
    fn dim_text_is_blended_toward_the_background() {
        let (_, frame) = laid_out(b"\x1b[2mz", 2, 10);
        let p = palette();
        assert_eq!(frame.texts[0].fg, mix(p.fg, p.bg, 0.5));
        assert_ne!(frame.texts[0].fg, p.fg);
    }
}
