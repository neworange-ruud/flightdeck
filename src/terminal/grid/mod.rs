//! The terminal-grid seam: one trait between "bytes a PTY produced" and "cells a
//! front-end draws" (beads `remote-control-bmej.2.3`).
//!
//! Every front-end that paints a terminal — the ratatui TUI
//! ([`crate::tui::render`]) and the GPUI desktop app (`desktop/src/terminal/`) —
//! reads the emulator through [`TerminalGrid`] and nothing else. No front-end
//! names a `vt100` or `alacritty_terminal` type, so which emulator backs a
//! terminal is decided in exactly one place: [`Emulator`], with the TUI's pick
//! in [`TUI_EMULATOR`]. Swapping the TUI to the other emulator is a one-line
//! change there; adding a third emulator is one new module plus one
//! [`Emulator`] variant.
//!
//! What the trait covers, and deliberately does not:
//!
//! - **In:** feeding PTY output, resizing, reading visible cells (grapheme,
//!   colours, attributes, wide-character halves), the cursor (position, shape,
//!   visibility), the input modes a front-end must honour (alternate screen,
//!   mouse reporting mode + encoding, bracketed paste, application cursor and
//!   keypad), scrollback (offset + filled length), the active mouse selection,
//!   and any bytes the emulator must write back to the PTY (query replies).
//! - **Out:** the PTY itself (that is [`crate::contracts::PtySession`]) and the
//!   web replay stream, which carries the **raw** PTY bytes, never a parsed
//!   grid (specs/WEB_INTERFACE.md D2). Nothing here is on that path, so no
//!   emulator choice can change what a browser replays.
//!
//! [`crate::testing::FakeGrid`] is the in-memory implementation for tests that
//! want to pin exact cells without going through a parser.

pub mod alacritty_grid;
pub mod mouse;
pub mod vt100_grid;

#[cfg(test)]
mod fixtures;

use crate::tui::selection::{Point, Selection};

pub use alacritty_grid::AlacrittyGrid;
pub use mouse::{encode_mouse_button, encode_mouse_report};
pub use vt100_grid::Vt100Grid;

/// Scrollback lines kept by each terminal's emulator.
pub const SCROLLBACK: usize = 2000;

/// A cell colour as the hosted program asked for it, before any theme applies.
///
/// `Indexed(0..=15)` are the ANSI 16 (a front-end maps them onto its theme),
/// `Indexed(16..=255)` the xterm 256-colour cube and greys, `Rgb` truecolour.
/// `Default` is "the terminal's default foreground/background", which each
/// front-end paints with its own theme colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GridColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// How much horizontal space a cell's grapheme takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CellWidth {
    /// One column.
    #[default]
    Narrow,
    /// The first (left) half of a double-width grapheme (CJK, most emoji). The
    /// grapheme is drawn from here across two columns.
    Wide,
    /// The right half of a [`CellWidth::Wide`] cell. Carries no text of its own;
    /// a front-end draws nothing here but the background.
    WideContinuation,
}

/// Text attributes of one cell (SGR state at the time it was printed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellAttrs {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    /// SGR 9. vt100 does not track it and always reports `false`.
    pub strikethrough: bool,
}

/// One visible cell, borrowed for the duration of a [`TerminalGrid::visit_row`]
/// callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridCell<'a> {
    /// The grapheme: a base character plus any combining / zero-width
    /// characters. Empty for a never-written cell and for a
    /// [`CellWidth::WideContinuation`]; a front-end draws a blank there.
    pub text: &'a str,
    pub fg: GridColor,
    pub bg: GridColor,
    pub attrs: CellAttrs,
    pub width: CellWidth,
}

/// An owned copy of a [`GridCell`], for tests and one-off lookups.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OwnedCell {
    pub text: String,
    pub fg: GridColor,
    pub bg: GridColor,
    pub attrs: CellAttrs,
    pub width: CellWidth,
}

impl GridCell<'_> {
    /// An owned copy of this cell.
    pub fn to_owned_cell(&self) -> OwnedCell {
        OwnedCell {
            text: self.text.to_string(),
            fg: self.fg,
            bg: self.bg,
            attrs: self.attrs,
            width: self.width,
        }
    }
}

/// The cursor shape the hosted program asked for with DECSCUSR (`CSI Ps SP q`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    /// A thin vertical bar (a "beam").
    Bar,
}

/// Where the cursor is and how to draw it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GridCursor {
    /// Row on the **live** screen (0 = top), unaffected by the scrollback
    /// offset. A front-end scrolled into history should not draw it.
    pub row: u16,
    pub col: u16,
    pub shape: CursorShape,
    /// Whether the program asked for a blinking cursor. Front-ends may ignore it.
    pub blinking: bool,
    /// DECTCEM (`CSI ? 25 h/l`). `false` means "do not draw a cursor".
    pub visible: bool,
}

/// Which mouse events the hosted program has asked to receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseMode {
    /// No reporting: the front-end owns the mouse (local selection, scrollback).
    #[default]
    None,
    /// X10 (`?9`): presses only.
    Press,
    /// VT200 (`?1000`): presses and releases.
    PressRelease,
    /// `?1002`: presses, releases, and motion while a button is held.
    ButtonMotion,
    /// `?1003`: presses, releases, and all motion.
    AnyMotion,
}

/// How mouse reports are encoded on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseEncoding {
    /// The legacy one-byte-per-field `ESC [ M` encoding.
    #[default]
    Default,
    /// `?1005`: like `Default` but fields above 95 are UTF-8 encoded.
    Utf8,
    /// `?1006`: `ESC [ < b ; x ; y M/m`.
    Sgr,
}

/// The terminal modes a front-end must honour when it encodes input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TerminalModes {
    /// The program is drawing on the alternate screen (`?1049`, `?47`): a
    /// full-screen app such as vim, less or an agent TUI. There is no
    /// scrollback while it is active.
    pub alt_screen: bool,
    pub mouse_mode: MouseMode,
    pub mouse_encoding: MouseEncoding,
    /// `?2004`: wrap pastes in `ESC [200~` … `ESC [201~`.
    pub bracketed_paste: bool,
    /// DECCKM (`?1`). Arrow keys *could* be sent as SS3; FlightDeck always sends
    /// CSI (see [`crate::app::keymap::encode_pty`]), so this is informational.
    pub app_cursor: bool,
    /// DECKPAM (`ESC =`).
    pub app_keypad: bool,
}

impl TerminalModes {
    /// Whether the program wants mouse events forwarded instead of handled
    /// locally.
    pub fn wants_mouse(&self) -> bool {
        self.mouse_mode != MouseMode::None
    }
}

/// Which visible rows may have changed since the last
/// [`TerminalGrid::take_damage`], so a front-end that caches per-row layout can
/// redo only those.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GridDamage {
    /// Treat every row as changed: a resize, a scrollback move, a whole-screen
    /// change, or an emulator that does not track damage.
    Full,
    /// Only these viewport rows (ascending, no duplicates), possibly none.
    Rows(Vec<u16>),
}

/// A read-only view of a terminal emulator's visible screen: everything a
/// front-end needs to paint one frame.
///
/// Coordinates are 0-based `(row, col)` on the **visible** viewport, which is
/// the live screen when [`GridView::scrollback`] is 0 and a window into
/// history otherwise.
///
/// Split from [`TerminalGrid`] so a renderer takes the narrowest thing it
/// needs, and so a bare parsed screen (a `vt100::Screen` snapshot in a render
/// test) can be drawn without a live emulator around it.
pub trait GridView {
    /// `(rows, cols)` of the viewport.
    fn size(&self) -> (u16, u16);

    /// Call `f(col, cell)` for every cell of visible `row`, left to right. Rows
    /// outside the viewport visit nothing.
    fn visit_row(&self, row: u16, f: &mut dyn FnMut(u16, &GridCell<'_>));

    fn cursor(&self) -> GridCursor;

    fn modes(&self) -> TerminalModes;

    /// How many rows the viewport is scrolled up into history (0 = live).
    fn scrollback(&self) -> usize;

    // --- provided -----------------------------------------------------------

    /// An owned copy of one visible cell.
    fn cell(&self, row: u16, col: u16) -> Option<OwnedCell> {
        let mut found = None;
        self.visit_row(row, &mut |c, cell| {
            if c == col {
                found = Some(cell.to_owned_cell());
            }
        });
        found
    }

    /// The text of visible `row` from column `c0` to `c1` inclusive, blanks for
    /// empty cells, trailing whitespace trimmed.
    fn row_text(&self, row: u16, c0: u16, c1: u16) -> String {
        let mut s = String::new();
        self.visit_row(row, &mut |c, cell| {
            if c < c0 || c > c1 || cell.width == CellWidth::WideContinuation {
                return;
            }
            if cell.text.is_empty() {
                s.push(' ');
            } else {
                s.push_str(cell.text);
            }
        });
        s.truncate(s.trim_end().len());
        s
    }

    /// Every visible row as text, joined with `\n`, each trimmed (for dumps and
    /// tests).
    fn contents(&self) -> String {
        let (rows, cols) = self.size();
        (0..rows)
            .map(|r| self.row_text(r, 0, cols.saturating_sub(1)))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A live terminal emulator: a [`GridView`] that can also be fed, resized,
/// scrolled and selected in.
pub trait TerminalGrid: GridView + Send {
    /// Feed raw PTY output. Must never panic, whatever the bytes.
    fn process(&mut self, bytes: &[u8]);

    /// Advance time-based emulator state. Call periodically even when no output
    /// arrives: an emulator that buffers a synchronized update (`?2026`) flushes
    /// it here once its timeout passes. A no-op for emulators without timers.
    fn tick(&mut self) {}

    /// Bytes the emulator wants written back to the PTY (replies to cursor
    /// position / device attribute / colour queries), drained on read.
    fn take_replies(&mut self) -> Vec<u8>;

    /// Resize the grid. The caller resizes the PTY to match. Drops any
    /// selection, since content may reflow under a new width.
    fn resize(&mut self, rows: u16, cols: u16);

    /// How many history rows exist above the live screen (the most
    /// [`TerminalGrid::set_scrollback`] can scroll to).
    fn scrollback_len(&self) -> usize;

    /// Scroll the viewport to `rows` above the live screen, clamped to
    /// [`TerminalGrid::scrollback_len`].
    fn set_scrollback(&mut self, rows: usize);

    /// The active mouse selection, if any.
    fn selection(&self) -> Option<&Selection>;

    fn set_selection(&mut self, selection: Option<Selection>);

    /// Tell the emulator the colours its default foreground and background are
    /// painted with, so it can answer OSC 10/11 colour queries truthfully.
    /// Agents use the answer to choose a light or dark theme. A no-op for an
    /// emulator that does not answer those queries.
    fn set_default_colors(&mut self, _fg: (u8, u8, u8), _bg: (u8, u8, u8)) {}

    /// The rows that may have changed since the previous call, which starts a
    /// new tracking interval. Covers everything a [`GridView`] reports except
    /// the selection, which a front-end draws from [`TerminalGrid::selection`]
    /// on its own. The first call after creation reports [`GridDamage::Full`].
    ///
    /// Always `Full` unless an emulator overrides it, which is correct, only
    /// slower: vt100 keeps no damage information.
    fn take_damage(&mut self) -> GridDamage {
        GridDamage::Full
    }

    /// Whether the emulator is holding parsed output back until a later
    /// [`TerminalGrid::tick`] or more input releases it (a synchronized
    /// update, `?2026`, still waiting for its end or its timeout). A front-end
    /// that only repaints on change keeps ticking while this is true and
    /// repaints once it turns false. Always `false` for an emulator that never
    /// buffers.
    fn holds_output(&self) -> bool {
        false
    }

    // --- provided -----------------------------------------------------------

    /// The text under the active selection, reading history as needed. Lines
    /// are joined with `\n` and trailing whitespace is trimmed per line. The
    /// viewport's scrollback offset is restored before returning.
    fn selected_text(&mut self) -> Option<String> {
        let sel = *self.selection()?;
        if sel.is_empty() {
            return None;
        }
        let (rows, cols) = self.size();
        let saved = self.scrollback();
        let (first, last) = sel.first_last();

        let mut lines: Vec<String> = Vec::new();
        let mut rfb = first.rows_from_bottom;
        while rfb >= last.rows_from_bottom {
            if let Some((c0, c1)) = sel.col_range_for_rfb(rfb, cols) {
                // Bring this content line into view at the bottom-most row (the
                // offset clamps for lines older than the history).
                self.set_scrollback(rfb.max(0) as usize);
                let actual = self.scrollback();
                let screen_row = (rows as i64 - 1) - rfb + actual as i64;
                if (0..rows as i64).contains(&screen_row) {
                    lines.push(self.row_text(
                        screen_row as u16,
                        c0,
                        c1.min(cols.saturating_sub(1)),
                    ));
                }
            }
            rfb -= 1;
        }

        self.set_scrollback(saved);
        if lines.is_empty() {
            None
        } else {
            Some(lines.join("\n"))
        }
    }
}

/// Which emulator backs a terminal. The only place the choice is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emulator {
    /// The `vt100` crate. The TUI's emulator since the first release.
    Vt100,
    /// `alacritty_terminal`, the emulator core of Alacritty and Zed. The
    /// desktop app's emulator (see desktop/NOTES-M0.md, "Terminal emulator").
    Alacritty,
}

/// The emulator the TUI's terminals are built with. Switching the TUI to
/// alacritty_terminal is this one line.
pub const TUI_EMULATOR: Emulator = Emulator::Vt100;

impl Emulator {
    /// A fresh grid of this emulator at `rows` x `cols`.
    pub fn build(self, rows: u16, cols: u16) -> Box<dyn TerminalGrid> {
        match self {
            Emulator::Vt100 => Box::new(Vt100Grid::new(rows, cols, SCROLLBACK)),
            Emulator::Alacritty => Box::new(AlacrittyGrid::new(rows, cols, SCROLLBACK)),
        }
    }
}

/// Map a visible `(row, col)` to a scroll-stable selection [`Point`], clamping
/// to the viewport.
pub fn point_at(grid: &dyn GridView, screen_row: u16, col: u16) -> Point {
    let (rows, cols) = grid.size();
    let offset = grid.scrollback();
    let row = screen_row.min(rows.saturating_sub(1));
    let col = col.min(cols.saturating_sub(1));
    Point {
        rows_from_bottom: crate::tui::selection::screen_row_to_rfb(row, rows, offset).max(0),
        col,
    }
}

/// The xterm default RGB for a 256-colour palette index.
///
/// 0..=15 are xterm's stock ANSI colours (a front-end normally replaces them
/// with its theme); 16..=231 the 6x6x6 cube; 232..=255 the grey ramp. Shared so
/// the desktop app paints, and alacritty answers OSC 4 queries with, the same
/// values.
pub fn xterm_rgb(index: u8) -> (u8, u8, u8) {
    const ANSI: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    match index {
        0..=15 => ANSI[usize::from(index)],
        16..=231 => {
            // Each axis steps 0, 95, 135, 175, 215, 255.
            let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            let i = index - 16;
            (step(i / 36), step((i / 6) % 6), step(i % 6))
        }
        _ => {
            let v = 8 + (index - 232) * 10;
            (v, v, v)
        }
    }
}

/// The Cursor Position Report (`ESC [ row ; col R`, 1-based) for a 0-based
/// cursor position — the reply to a DSR 6 query.
pub(crate) fn cursor_position_report(row: u16, col: u16) -> Vec<u8> {
    format!("\x1b[{};{}R", u32::from(row) + 1, u32::from(col) + 1).into_bytes()
}

/// Whether `bytes` contains a DSR cursor-position query (`ESC [ 6 n`).
///
/// Per chunk: a query split across two reads is missed (not observed from
/// ConPTY, which emits `ESC[6n` atomically).
pub(crate) fn contains_cursor_position_query(bytes: &[u8]) -> bool {
    const DSR_CPR_QUERY: &[u8] = b"\x1b[6n";
    bytes
        .windows(DSR_CPR_QUERY.len())
        .any(|w| w == DSR_CPR_QUERY)
}
