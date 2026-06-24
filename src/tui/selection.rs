//! Mouse text selection over a terminal's VT100 screen + scrollback.
//!
//! Native terminal selection is unavailable while FlightDeck holds mouse capture
//! (the host emulator never sees the drag), so we implement our own. Selections
//! are tracked in **rows-from-bottom** coordinates so a selected region stays
//! pinned to the same content lines as the viewport scrolls — only *appending*
//! new output shifts them, which is exactly what we want for drag-to-scroll
//! (scrolling the view must not slide the selection off the text it covers).
//!
//! All logic here is pure: it depends only on the screen geometry (`rows`,
//! `cols`) and the current scrollback `offset`, never on a live terminal. Text
//! extraction lives on [`crate::terminal::session::Terminal`] because it needs
//! to read cells from (possibly off-screen) scrollback.

/// A point in a terminal's content.
///
/// `rows_from_bottom == 0` is the bottom-most live row; larger values are
/// further up (older). This is invariant under pure scrolling: a content line
/// keeps the same `rows_from_bottom` regardless of the current scrollback
/// offset (see [`screen_row_to_rfb`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    /// Distance in rows from the bottom-most live row (0 = bottom).
    pub rows_from_bottom: i64,
    /// Column within the row.
    pub col: u16,
}

/// A mouse selection: an `anchor` (where the drag started) and a `head` (where
/// it currently is). Either may be the visually-higher point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,
}

/// Convert a visible screen row (`0` = top visible) to rows-from-bottom given
/// the screen height and current scrollback `offset`.
///
/// Invariant under scrolling: scrolling up by `n` increases `offset` by `n` and
/// moves a given content line *down* the screen by `n` rows, leaving this value
/// unchanged.
pub fn screen_row_to_rfb(screen_row: u16, rows: u16, offset: usize) -> i64 {
    (rows as i64 - 1 - screen_row as i64) + offset as i64
}

/// The screen row a given rows-from-bottom value maps to under `offset`, or a
/// value outside `0..rows` when the line is not currently visible.
pub fn rfb_to_screen_row(rfb: i64, rows: u16, offset: usize) -> i64 {
    (rows as i64 - 1) - rfb + offset as i64
}

impl Selection {
    /// Begin a selection collapsed onto a single point.
    pub fn new(p: Point) -> Self {
        Selection { anchor: p, head: p }
    }

    /// Whether the selection covers nothing (anchor and head coincide).
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// The selection endpoints in reading order: `(first, last)` where `first`
    /// is the visually-higher / earlier point and `last` the lower / later one.
    pub fn first_last(&self) -> (Point, Point) {
        // Reading order ascends by `(-rows_from_bottom, col)`: higher rfb first,
        // then smaller column.
        let key = |p: Point| (-p.rows_from_bottom, p.col);
        if key(self.anchor) <= key(self.head) {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// The inclusive `(start_col, end_col)` selected on the content line at
    /// `rfb`, or `None` if that line lies outside the selection. `cols` is the
    /// screen width, used to clamp and to extend full-row segments.
    pub fn col_range_for_rfb(&self, rfb: i64, cols: u16) -> Option<(u16, u16)> {
        let (first, last) = self.first_last();
        if rfb > first.rows_from_bottom || rfb < last.rows_from_bottom {
            return None;
        }
        let last_col = cols.saturating_sub(1);
        if first.rows_from_bottom == last.rows_from_bottom {
            // Single-line selection.
            let a = first.col.min(last.col).min(last_col);
            let b = first.col.max(last.col).min(last_col);
            Some((a, b))
        } else if rfb == first.rows_from_bottom {
            // Top line: from the start column to the end of the row.
            Some((first.col.min(last_col), last_col))
        } else if rfb == last.rows_from_bottom {
            // Bottom line: from the start of the row to the end column.
            Some((0, last.col.min(last_col)))
        } else {
            // A fully-selected middle line.
            Some((0, last_col))
        }
    }

    /// The inclusive `(start_col, end_col)` selected on a given visible screen
    /// row under `offset`, or `None` if nothing on that row is selected.
    pub fn row_selection(
        &self,
        screen_row: u16,
        rows: u16,
        cols: u16,
        offset: usize,
    ) -> Option<(u16, u16)> {
        let rfb = screen_row_to_rfb(screen_row, rows, offset);
        self.col_range_for_rfb(rfb, cols)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(rfb: i64, col: u16) -> Point {
        Point {
            rows_from_bottom: rfb,
            col,
        }
    }

    #[test]
    fn screen_row_rfb_is_invariant_under_scroll() {
        // The bottom row at offset 0 is rfb 0; the top row of a 24-row screen is
        // rfb 23.
        assert_eq!(screen_row_to_rfb(23, 24, 0), 0);
        assert_eq!(screen_row_to_rfb(0, 24, 0), 23);
        // Scrolling up by 5 moves that same content line (rfb 0) down off the
        // bottom, but its rfb is unchanged: it now sits at screen row 28 (off
        // screen) — i.e. the mapping is stable.
        // The content that was at screen row 0 (rfb 23) is now at row 5.
        assert_eq!(screen_row_to_rfb(5, 24, 5), 23);
        // Round-trips back to a screen row.
        assert_eq!(rfb_to_screen_row(23, 24, 5), 5);
    }

    #[test]
    fn empty_selection_has_no_range() {
        let s = Selection::new(p(0, 3));
        assert!(s.is_empty());
        assert_eq!(s.col_range_for_rfb(0, 80), Some((3, 3)));
    }

    #[test]
    fn single_line_selection_orders_columns() {
        // Drag from col 10 back to col 4 on the same line.
        let s = Selection {
            anchor: p(2, 10),
            head: p(2, 4),
        };
        assert_eq!(s.col_range_for_rfb(2, 80), Some((4, 10)));
        // Other lines are not part of the selection.
        assert_eq!(s.col_range_for_rfb(1, 80), None);
        assert_eq!(s.col_range_for_rfb(3, 80), None);
    }

    #[test]
    fn multi_line_selection_spans_rows() {
        // From (rfb 5, col 20) down to (rfb 2, col 8): top line runs col 20→end,
        // middle lines are full, bottom line runs col 0→8.
        let s = Selection {
            anchor: p(5, 20),
            head: p(2, 8),
        };
        assert_eq!(s.col_range_for_rfb(5, 80), Some((20, 79))); // top
        assert_eq!(s.col_range_for_rfb(4, 80), Some((0, 79))); // middle
        assert_eq!(s.col_range_for_rfb(3, 80), Some((0, 79))); // middle
        assert_eq!(s.col_range_for_rfb(2, 80), Some((0, 8))); // bottom
        assert_eq!(s.col_range_for_rfb(6, 80), None); // above
        assert_eq!(s.col_range_for_rfb(1, 80), None); // below
    }

    #[test]
    fn anchor_below_head_is_normalised() {
        // Anchor is the lower point (smaller rfb); head is higher up. Reading
        // order must still put the higher line first.
        let s = Selection {
            anchor: p(2, 8),
            head: p(5, 20),
        };
        let (first, last) = s.first_last();
        assert_eq!(first, p(5, 20));
        assert_eq!(last, p(2, 8));
        assert_eq!(s.col_range_for_rfb(5, 80), Some((20, 79)));
        assert_eq!(s.col_range_for_rfb(2, 80), Some((0, 8)));
    }

    #[test]
    fn columns_clamp_to_width() {
        let s = Selection {
            anchor: p(0, 200),
            head: p(0, 5),
        };
        assert_eq!(s.col_range_for_rfb(0, 80), Some((5, 79)));
    }

    #[test]
    fn row_selection_uses_offset() {
        // A 24-row screen scrolled up by 3. A selection pinned at rfb 23 lands on
        // screen row 3: rfb_to_screen_row(23, 24, 3) = 23 - 23 + 3 = 3.
        let s = Selection {
            anchor: p(23, 0),
            head: p(23, 10),
        };
        assert_eq!(rfb_to_screen_row(23, 24, 3), 3);
        assert_eq!(s.row_selection(3, 24, 80, 3), Some((0, 10)));
        assert_eq!(s.row_selection(5, 24, 80, 3), None);
    }
}
