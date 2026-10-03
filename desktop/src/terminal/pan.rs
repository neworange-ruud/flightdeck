//! Panning a grid that does not fit its element.
//!
//! A remote window draws the host's grid at its natural size: the host owns
//! PTY geometry (`specs/WEB_INTERFACE.md` D4), so the grid can be larger than
//! the window. R17 rules what happens then, for the browser and so for this
//! window too: **the view scrolls** — nothing is clipped out of reach,
//! nothing is scaled. This module is that rule as arithmetic, pure so it is
//! tested without a window.
//!
//! - The viewport is anchored to the grid's **bottom-left**: a terminal's
//!   newest line and its prompt are at the bottom, so that is what a window
//!   too short for the grid shows first.
//! - [`Pan`] is how far the viewport moved from there: `up` rows towards the
//!   grid's top, `left` columns towards its right edge.
//! - The wheel treats the grid's hidden top rows as the stretch between the
//!   visible rows and the history: scrolling up pans to the grid's top first
//!   and then goes into the history; scrolling down leaves the history first
//!   and then pans back to the bottom ([`scroll_up`], [`scroll_down`]).
//! - Typing brings the cursor back into view ([`reveal`]), as a terminal
//!   scrolled into its history returns to the live screen on a key.
//!
//! A grid that fits (every local terminal: the element sizes the PTY to
//! itself) has nothing to pan, and every function here is then the identity.

/// How far the viewport is panned from the grid's bottom-left corner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pan {
    /// Rows up from the bottom-anchored view.
    pub up: u16,
    /// Columns in from the left edge.
    pub left: u16,
}

/// The part of the grid an element shows: its first row and column, and how
/// many of each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Viewport {
    pub top: u16,
    pub left: u16,
    pub rows: u16,
    pub cols: u16,
    /// Grid rows that do not fit (0 when the grid fits).
    pub overflow_rows: u16,
    /// Grid columns that do not fit (0 when the grid fits).
    pub overflow_cols: u16,
}

impl Viewport {
    /// Whether any of the grid is out of view.
    pub fn overflows(&self) -> bool {
        self.overflow_rows > 0 || self.overflow_cols > 0
    }
}

impl Pan {
    /// This pan held within what `grid` (rows, cols) overflows `fit` by.
    pub fn clamped(self, grid: (u16, u16), fit: (u16, u16)) -> Pan {
        Pan {
            up: self.up.min(grid.0.saturating_sub(fit.0)),
            left: self.left.min(grid.1.saturating_sub(fit.1)),
        }
    }

    /// What an element of `fit` (rows, cols) shows of a `grid` (rows, cols)
    /// panned by `self`.
    pub fn viewport(self, grid: (u16, u16), fit: (u16, u16)) -> Viewport {
        let overflow_rows = grid.0.saturating_sub(fit.0);
        let overflow_cols = grid.1.saturating_sub(fit.1);
        let pan = self.clamped(grid, fit);
        Viewport {
            top: overflow_rows - pan.up,
            left: pan.left,
            rows: grid.0.min(fit.0),
            cols: grid.1.min(fit.1),
            overflow_rows,
            overflow_cols,
        }
    }
}

/// Scroll `lines` up: pan towards the grid's top first. Returns the new pan
/// and the lines left over for the history.
pub fn scroll_up(pan: Pan, view: &Viewport, lines: usize) -> (Pan, usize) {
    let room = usize::from(view.overflow_rows.saturating_sub(pan.up));
    let taken = room.min(lines);
    let pan = Pan {
        up: pan.up + taken as u16,
        ..pan
    };
    (pan, lines - taken)
}

/// Scroll `lines` down, of which the history already took `from_history`
/// (it is left first): pan the rest back towards the bottom.
pub fn scroll_down(pan: Pan, lines: usize, from_history: usize) -> Pan {
    let rest = lines.saturating_sub(from_history);
    Pan {
        up: pan
            .up
            .saturating_sub(rest.min(usize::from(u16::MAX)) as u16),
        ..pan
    }
}

/// Pan `columns` to the right (positive) or left (negative), within the
/// overflow.
pub fn scroll_sideways(pan: Pan, view: &Viewport, columns: i32) -> Pan {
    let left = (i32::from(pan.left) + columns).clamp(0, i32::from(view.overflow_cols));
    Pan {
        left: left as u16,
        ..pan
    }
}

/// The least pan that puts `cursor` (row, col) in view.
pub fn reveal(view: &Viewport, cursor: (u16, u16)) -> Pan {
    let (row, col) = cursor;
    let mut top = view.top;
    if row < top {
        top = row;
    } else if row >= top + view.rows {
        top = row + 1 - view.rows;
    }
    let mut left = view.left;
    if col < left {
        left = col;
    } else if col >= left + view.cols {
        left = col + 1 - view.cols;
    }
    Pan {
        up: view.overflow_rows.saturating_sub(top),
        left: left.min(view.overflow_cols),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRID: (u16, u16) = (40, 120);
    const FIT: (u16, u16) = (30, 100);

    #[test]
    fn a_grid_that_fits_has_nothing_to_pan() {
        let view = Pan { up: 5, left: 5 }.viewport((24, 80), (30, 100));
        assert_eq!(
            view,
            Viewport {
                top: 0,
                left: 0,
                rows: 24,
                cols: 80,
                overflow_rows: 0,
                overflow_cols: 0,
            }
        );
        assert!(!view.overflows());
        let (pan, rest) = scroll_up(Pan::default(), &view, 3);
        assert_eq!((pan, rest), (Pan::default(), 3), "all of it is history's");
    }

    #[test]
    fn an_oversized_grid_shows_its_bottom_left_first() {
        let view = Pan::default().viewport(GRID, FIT);
        assert_eq!((view.top, view.left), (10, 0));
        assert_eq!((view.rows, view.cols), (30, 100));
        assert!(view.overflows());
    }

    #[test]
    fn a_pan_is_held_within_the_overflow() {
        let view = Pan { up: 99, left: 99 }.viewport(GRID, FIT);
        assert_eq!((view.top, view.left), (0, 20));
        assert_eq!(
            Pan { up: 99, left: 99 }.clamped(GRID, FIT),
            Pan { up: 10, left: 20 }
        );
    }

    #[test]
    fn scrolling_up_reaches_the_grid_top_before_the_history() {
        let view = Pan::default().viewport(GRID, FIT);
        let (pan, rest) = scroll_up(Pan::default(), &view, 4);
        assert_eq!((pan.up, rest), (4, 0));
        let view = pan.viewport(GRID, FIT);
        let (pan, rest) = scroll_up(pan, &view, 9);
        assert_eq!((pan.up, rest), (10, 3), "the top, then 3 lines of history");
    }

    #[test]
    fn scrolling_down_leaves_the_history_before_panning() {
        let pan = Pan { up: 10, left: 0 };
        assert_eq!(scroll_down(pan, 3, 3).up, 10, "all three were history");
        assert_eq!(scroll_down(pan, 5, 2).up, 7);
        assert_eq!(scroll_down(pan, 50, 0).up, 0, "never below the bottom");
    }

    #[test]
    fn sideways_scrolling_stays_within_the_columns() {
        let view = Pan::default().viewport(GRID, FIT);
        assert_eq!(scroll_sideways(Pan::default(), &view, 7).left, 7);
        assert_eq!(scroll_sideways(Pan::default(), &view, 70).left, 20);
        assert_eq!(scroll_sideways(Pan { up: 0, left: 3 }, &view, -9).left, 0);
    }

    #[test]
    fn typing_brings_the_cursor_into_view() {
        // Panned to the top: the cursor on the last row comes back down.
        let pan = Pan { up: 10, left: 0 };
        let view = pan.viewport(GRID, FIT);
        assert_eq!(reveal(&view, (39, 0)), Pan { up: 0, left: 0 });
        // Off the right edge: just far enough to show its column.
        let view = Pan::default().viewport(GRID, FIT);
        assert_eq!(reveal(&view, (39, 105)).left, 6);
        // Already in view: unchanged.
        assert_eq!(reveal(&view, (20, 50)), Pan::default());
    }
}
