//! [`TerminalGrid`] over the `vt100` crate: the TUI's emulator.
//!
//! Besides mapping types, this module owns the two `vt100 0.16.2` workarounds
//! that used to live on `Terminal`: the contained parser panic (and the rebuild
//! that follows it) and the DSR cursor-position reply. It also recovers the
//! cursor shape, which vt100 does not model, from DECSCUSR via its callbacks.

use super::{
    contains_cursor_position_query, cursor_position_report, CellAttrs, CellWidth, CursorShape,
    GridCell, GridColor, GridCursor, GridView, MouseEncoding, MouseMode, TerminalGrid,
    TerminalModes,
};
use crate::tui::selection::Selection;

thread_local! {
    /// Set only while [`Vt100Grid::process`] is inside `vt100`, so the
    /// process-wide panic hook can tell a panic we handle from one we do not.
    static PARSER_PANIC_EXPECTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the current thread is inside a guarded `vt100` parse whose panic is
/// already handled. The panic hook installed in `run()` uses this to stay quiet
/// and leave the terminal alone.
pub fn parser_panic_expected() -> bool {
    PARSER_PANIC_EXPECTED.with(std::cell::Cell::get)
}

/// Sets [`PARSER_PANIC_EXPECTED`] for its lifetime. Clearing happens in `Drop`,
/// so it is correct on the unwinding path too.
struct ExpectedParserPanic;

impl ExpectedParserPanic {
    fn new() -> Self {
        PARSER_PANIC_EXPECTED.with(|f| f.set(true));
        Self
    }
}

impl Drop for ExpectedParserPanic {
    fn drop(&mut self) {
        PARSER_PANIC_EXPECTED.with(|f| f.set(false));
    }
}

/// vt100 callbacks: the sequences vt100 parses but does not model, which we
/// still want. Only DECSCUSR today.
#[derive(Debug, Default)]
struct Extras {
    cursor_shape: CursorShape,
    cursor_blinking: bool,
}

impl vt100::Callbacks for Extras {
    fn unhandled_csi(
        &mut self,
        _: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        // DECSCUSR, `CSI Ps SP q`: 1 blinking block, 2 steady block, 3/4
        // underline, 5/6 bar (odd = blinking). 0 is "the terminal's default"
        // (a steady block here), as alacritty_terminal treats it, rather than
        // xterm's literal blinking block, so both grids report the same
        // cursor for the same bytes.
        if c != 'q' || i1 != Some(b' ') || i2.is_some() {
            return;
        }
        let ps = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        let (shape, blinking) = match ps {
            1 => (CursorShape::Block, true),
            0 | 2 => (CursorShape::Block, false),
            3 => (CursorShape::Underline, true),
            4 => (CursorShape::Underline, false),
            5 => (CursorShape::Bar, true),
            6 => (CursorShape::Bar, false),
            _ => return,
        };
        self.cursor_shape = shape;
        self.cursor_blinking = blinking;
    }
}

/// A `vt100` parser behind [`TerminalGrid`].
pub struct Vt100Grid {
    parser: vt100::Parser<Extras>,
    scrollback_capacity: usize,
    selection: Option<Selection>,
    replies: Vec<u8>,
    /// Filled history rows. vt100 keeps this private; measured after every
    /// change that can move it (see [`Vt100Grid::measure_history`]).
    history_len: usize,
}

impl Vt100Grid {
    /// A blank `rows` x `cols` grid keeping `scrollback` history rows.
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            parser: vt100::Parser::new_with_callbacks(rows, cols, scrollback, Extras::default()),
            scrollback_capacity: scrollback,
            selection: None,
            replies: Vec::new(),
            history_len: 0,
        }
    }

    /// Run one `vt100` parse, containing a panic from inside the library.
    ///
    /// The hook installed in `run()` checks [`parser_panic_expected`] so a panic
    /// caught here does not tear down the user's terminal modes or print a
    /// backtrace over a live UI.
    fn feed(parser: &mut vt100::Parser<Extras>, bytes: &[u8]) -> std::result::Result<(), ()> {
        let _expected = ExpectedParserPanic::new();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parser.process(bytes)))
            .map_err(|_| ())
    }

    /// Re-measure [`Vt100Grid::history_len`]. vt100 clamps `set_scrollback` to
    /// the filled history, so asking for "everything" and reading back the
    /// offset is the length; the real offset is then put back (it is never more
    /// than the length, so the round trip is exact).
    fn measure_history(&mut self) {
        let screen = self.parser.screen_mut();
        let saved = screen.scrollback();
        screen.set_scrollback(usize::MAX);
        self.history_len = screen.scrollback();
        screen.set_scrollback(saved);
    }
}

impl TerminalGrid for Vt100Grid {
    /// Guarded, because `vt100 0.16.2` can panic while printing and there is no
    /// released version that does not. `Grid::set_size` truncates each row with a
    /// plain `Vec::resize` (`grid.rs:78-80` -> `row.rs:73-76`), so shrinking the
    /// column count through the middle of a wide (width-2) character drops the
    /// continuation half and leaves the first half flagged `is_wide()` in the new
    /// last column. The next print onto that cell reaches `screen.rs:870`, which
    /// unwraps `drawing_cell_mut(col + 1)` on the library's own assumption that a
    /// wide cell is always followed by its other half — and gets `None`.
    ///
    /// `MIN_GRID_COLS` (session.rs) does not help here: the stranded cell can sit
    /// at any column, so the panic is reachable at any grid size. Rather than let
    /// a resize take the whole app down, the panic is contained and the parser is
    /// rebuilt. The rebuild is not optional: the panic unwinds out of
    /// `Screen::text` mid-mutation and the stranded cell is still there, so
    /// reusing the parser would just panic again on the next chunk.
    ///
    /// Cost when this fires: the pane's scrollback and screen contents are lost
    /// and the agent repaints. That is a far better failure than losing every
    /// pane and the user's session.
    fn process(&mut self, bytes: &[u8]) {
        if Self::feed(&mut self.parser, bytes).is_err() {
            // Start from a clean grid at the same size, then let the chunk land on it.
            let (rows, cols) = self.parser.screen().size();
            self.parser = vt100::Parser::new_with_callbacks(
                rows,
                cols,
                self.scrollback_capacity,
                Extras::default(),
            );
            self.selection = None;
            let _ = Self::feed(&mut self.parser, bytes);
        }
        self.measure_history();

        // Reply to a Device Status Report cursor-position query (`ESC[6n`) with
        // a Cursor Position Report, exactly as a real terminal emulator does.
        // ConPTY (Windows) and many interactive programs that probe the cursor
        // (PowerShell's PSReadLine, Node-based TUIs such as Claude Code) **block
        // their initial render until they receive this reply**; without it a
        // Windows pane stays blank with only a cursor in the top-left. Answered
        // after the chunk is parsed, so the position is the post-chunk cursor.
        if contains_cursor_position_query(bytes) {
            let (row, col) = self.parser.screen().cursor_position();
            self.replies
                .extend_from_slice(&cursor_position_report(row, col));
        }
    }

    fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.selection = None;
        self.parser.screen_mut().set_size(rows, cols);
        self.measure_history();
    }

    fn scrollback_len(&self) -> usize {
        self.history_len
    }

    fn set_scrollback(&mut self, rows: usize) {
        self.parser.screen_mut().set_scrollback(rows);
    }

    fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    fn set_selection(&mut self, selection: Option<Selection>) {
        self.selection = selection;
    }
}

impl GridView for Vt100Grid {
    fn size(&self) -> (u16, u16) {
        self.parser.screen().size()
    }

    fn visit_row(&self, row: u16, f: &mut dyn FnMut(u16, &GridCell<'_>)) {
        self.parser.screen().visit_row(row, f);
    }

    /// The screen's cursor, with the DECSCUSR shape the callbacks recorded
    /// (a bare [`vt100::Screen`] cannot know it).
    fn cursor(&self) -> GridCursor {
        let extras = self.parser.callbacks();
        GridCursor {
            shape: extras.cursor_shape,
            blinking: extras.cursor_blinking,
            ..self.parser.screen().cursor()
        }
    }

    fn modes(&self) -> TerminalModes {
        self.parser.screen().modes()
    }

    fn scrollback(&self) -> usize {
        self.parser.screen().scrollback()
    }
}

/// A bare parsed vt100 screen is a [`GridView`] too, so a render test can draw
/// a `vt100::Screen` snapshot directly. Its cursor is always a steady block:
/// the shape lives in [`Vt100Grid`]'s callbacks, not on the screen.
impl GridView for vt100::Screen {
    fn size(&self) -> (u16, u16) {
        vt100::Screen::size(self)
    }

    fn visit_row(&self, row: u16, f: &mut dyn FnMut(u16, &GridCell<'_>)) {
        let (rows, cols) = vt100::Screen::size(self);
        if row >= rows {
            return;
        }
        for col in 0..cols {
            let Some(cell) = self.cell(row, col) else {
                continue;
            };
            let width = if cell.is_wide() {
                CellWidth::Wide
            } else if cell.is_wide_continuation() {
                CellWidth::WideContinuation
            } else {
                CellWidth::Narrow
            };
            f(
                col,
                &GridCell {
                    text: cell.contents(),
                    fg: color(cell.fgcolor()),
                    bg: color(cell.bgcolor()),
                    attrs: CellAttrs {
                        bold: cell.bold(),
                        dim: cell.dim(),
                        italic: cell.italic(),
                        underline: cell.underline(),
                        inverse: cell.inverse(),
                        strikethrough: false,
                    },
                    width,
                },
            );
        }
    }

    fn cursor(&self) -> GridCursor {
        let (row, col) = self.cursor_position();
        GridCursor {
            row,
            col,
            shape: CursorShape::Block,
            blinking: false,
            visible: !self.hide_cursor(),
        }
    }

    fn modes(&self) -> TerminalModes {
        TerminalModes {
            alt_screen: self.alternate_screen(),
            mouse_mode: match self.mouse_protocol_mode() {
                vt100::MouseProtocolMode::None => MouseMode::None,
                vt100::MouseProtocolMode::Press => MouseMode::Press,
                vt100::MouseProtocolMode::PressRelease => MouseMode::PressRelease,
                vt100::MouseProtocolMode::ButtonMotion => MouseMode::ButtonMotion,
                vt100::MouseProtocolMode::AnyMotion => MouseMode::AnyMotion,
            },
            mouse_encoding: match self.mouse_protocol_encoding() {
                vt100::MouseProtocolEncoding::Default => MouseEncoding::Default,
                vt100::MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
                vt100::MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
            },
            bracketed_paste: self.bracketed_paste(),
            app_cursor: self.application_cursor(),
            app_keypad: self.application_keypad(),
        }
    }

    fn scrollback(&self) -> usize {
        vt100::Screen::scrollback(self)
    }
}

/// Map a vt100 colour onto the grid's colour.
fn color(c: vt100::Color) -> GridColor {
    match c {
        vt100::Color::Default => GridColor::Default,
        vt100::Color::Idx(i) => GridColor::Indexed(i),
        vt100::Color::Rgb(r, g, b) => GridColor::Rgb(r, g, b),
    }
}
