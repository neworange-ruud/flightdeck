//! `FakeGrid` — an in-memory [`TerminalGrid`] with no parser behind it.
//!
//! For tests of code that *reads* a terminal (renderers, selection, input
//! encoding that depends on modes) where going through a real emulator would
//! only add noise: cells, cursor, modes and history are set directly, and fed
//! bytes are recorded rather than parsed.

use crate::terminal::grid::{
    CellWidth, GridCell, GridCursor, GridView, OwnedCell, TerminalGrid, TerminalModes,
};
use crate::tui::selection::Selection;

/// An in-memory [`TerminalGrid`] for tests.
#[derive(Debug, Clone, Default)]
pub struct FakeGrid {
    rows: u16,
    cols: u16,
    /// The live screen, `rows` x `cols`.
    screen: Vec<Vec<OwnedCell>>,
    /// History rows above the live screen, oldest first.
    history: Vec<Vec<OwnedCell>>,
    offset: usize,
    cursor: GridCursor,
    modes: TerminalModes,
    selection: Option<Selection>,
    /// Every chunk passed to [`TerminalGrid::process`], in order.
    fed: Vec<Vec<u8>>,
    replies: Vec<u8>,
}

impl FakeGrid {
    /// A blank `rows` x `cols` grid with a visible block cursor at the origin.
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            rows,
            cols,
            screen: blank_rows(rows, cols),
            cursor: GridCursor {
                visible: true,
                ..GridCursor::default()
            },
            ..Self::default()
        }
    }

    /// Overwrite one live cell.
    pub fn set_cell(&mut self, row: u16, col: u16, cell: OwnedCell) {
        if let Some(slot) = self
            .screen
            .get_mut(usize::from(row))
            .and_then(|r| r.get_mut(usize::from(col)))
        {
            *slot = cell;
        }
    }

    /// Write `text` into live `row` from `col`, one narrow character per cell,
    /// default colours and attributes.
    pub fn set_text(&mut self, row: u16, col: u16, text: &str) {
        for (i, ch) in text.chars().enumerate() {
            let col = col.saturating_add(i as u16);
            self.set_cell(
                row,
                col,
                OwnedCell {
                    text: ch.to_string(),
                    ..OwnedCell::default()
                },
            );
        }
    }

    /// Append one history row (becomes the newest history line) holding `text`.
    pub fn push_history(&mut self, text: &str) {
        let mut row = blank_row(self.cols);
        for (cell, ch) in row.iter_mut().zip(text.chars()) {
            cell.text = ch.to_string();
        }
        self.history.push(row);
    }

    pub fn set_cursor(&mut self, cursor: GridCursor) {
        self.cursor = cursor;
    }

    pub fn set_modes(&mut self, modes: TerminalModes) {
        self.modes = modes;
    }

    /// Queue bytes for the next [`TerminalGrid::take_replies`].
    pub fn queue_reply(&mut self, bytes: &[u8]) {
        self.replies.extend_from_slice(bytes);
    }

    /// Every chunk fed so far.
    pub fn fed(&self) -> &[Vec<u8>] {
        &self.fed
    }

    /// The row the viewport shows at visible `row`: history while scrolled up.
    fn visible_row(&self, row: u16) -> Option<&Vec<OwnedCell>> {
        if row >= self.rows {
            return None;
        }
        // Visible row r shows content line (r - offset) counted from the top of
        // the live screen; negative lines are history, newest last.
        let line = i64::from(row) - self.offset as i64;
        if line >= 0 {
            self.screen.get(line as usize)
        } else {
            let from_end = (-line) as usize;
            self.history
                .len()
                .checked_sub(from_end)
                .map(|i| &self.history[i])
        }
    }
}

fn blank_row(cols: u16) -> Vec<OwnedCell> {
    vec![OwnedCell::default(); usize::from(cols)]
}

fn blank_rows(rows: u16, cols: u16) -> Vec<Vec<OwnedCell>> {
    vec![blank_row(cols); usize::from(rows)]
}

impl GridView for FakeGrid {
    fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    fn visit_row(&self, row: u16, f: &mut dyn FnMut(u16, &GridCell<'_>)) {
        let Some(cells) = self.visible_row(row) else {
            return;
        };
        for (col, cell) in cells.iter().enumerate() {
            f(
                col as u16,
                &GridCell {
                    text: if cell.width == CellWidth::WideContinuation {
                        ""
                    } else {
                        &cell.text
                    },
                    fg: cell.fg,
                    bg: cell.bg,
                    attrs: cell.attrs,
                    width: cell.width,
                },
            );
        }
    }

    fn cursor(&self) -> GridCursor {
        self.cursor
    }

    fn modes(&self) -> TerminalModes {
        self.modes
    }

    fn scrollback(&self) -> usize {
        self.offset
    }
}

impl TerminalGrid for FakeGrid {
    fn process(&mut self, bytes: &[u8]) {
        self.fed.push(bytes.to_vec());
    }

    fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    /// Truncates or pads each row; no reflow.
    fn resize(&mut self, rows: u16, cols: u16) {
        self.selection = None;
        self.rows = rows;
        self.cols = cols;
        self.screen.resize(usize::from(rows), blank_row(cols));
        for row in self.screen.iter_mut().chain(self.history.iter_mut()) {
            row.resize(usize::from(cols), OwnedCell::default());
        }
    }

    fn scrollback_len(&self) -> usize {
        self.history.len()
    }

    fn set_scrollback(&mut self, rows: usize) {
        self.offset = rows.min(self.history.len());
    }

    fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    fn set_selection(&mut self, selection: Option<Selection>) {
        self.selection = selection;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::selection::Point;

    #[test]
    fn scrolling_shows_history_rows_above_the_live_screen() {
        let mut grid = FakeGrid::new(2, 8);
        grid.push_history("old one");
        grid.push_history("old two");
        grid.set_text(0, 0, "live A");
        grid.set_text(1, 0, "live B");
        assert_eq!(grid.contents(), "live A\nlive B");

        grid.set_scrollback(1);
        assert_eq!(grid.contents(), "old two\nlive A");
        grid.set_scrollback(99);
        assert_eq!(grid.scrollback(), 2, "clamped to the history length");
        assert_eq!(grid.contents(), "old one\nold two");
    }

    #[test]
    fn selected_text_reads_through_the_trait_default() {
        let mut grid = FakeGrid::new(2, 8);
        grid.push_history("history");
        grid.set_text(0, 0, "first");
        grid.set_text(1, 0, "second");
        // From the history row (rfb 2) to column 2 of the bottom row (rfb 0).
        grid.set_selection(Some(Selection {
            anchor: Point {
                rows_from_bottom: 2,
                col: 0,
            },
            head: Point {
                rows_from_bottom: 0,
                col: 2,
            },
        }));
        assert_eq!(grid.selected_text().as_deref(), Some("history\nfirst\nsec"));
        assert_eq!(grid.scrollback(), 0, "offset restored");
    }

    #[test]
    fn feeds_are_recorded_not_parsed() {
        let mut grid = FakeGrid::new(2, 8);
        grid.process(b"\x1b[31mhi");
        assert_eq!(grid.fed(), &[b"\x1b[31mhi".to_vec()]);
        assert_eq!(grid.contents(), "\n", "nothing drawn");
    }
}
