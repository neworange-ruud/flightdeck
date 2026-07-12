//! Layout math as pure functions of `(area: Rect, ...)` (T8, SPECS §20).
//!
//! All functions are pure — they take a [`Rect`] and return sub-rects with no
//! I/O, no rendering, and no state. Every function is independently unit-
//! testable without a real terminal.
//!
//! T9 integration note: the wiring layer calls [`compute`] on every resize and
//! passes the resulting [`MainLayout`] to all render functions.

use ratatui::layout::{Constraint, Layout, Rect};

/// Width of the Agent Tabs sidebar in columns (SPECS §20).
pub const SIDEBAR_WIDTH: u16 = 28;

/// Height of the child-terminal tab bar in rows (SPECS §19, §20).
pub const CHILD_TAB_BAR_HEIGHT: u16 = 1;

/// Height of the status/action bar in rows (SPECS §23).
pub const STATUS_BAR_HEIGHT: u16 = 1;

/// Height of the git info bar in rows (branch + change counts, SPECS §21).
pub const INFO_BAR_HEIGHT: u16 = 1;

/// Height of the full-width branded header (logo) row.
pub const HEADER_HEIGHT: u16 = 1;

/// Height of the full-width project tab row (switch between open projects).
pub const PROJECT_TAB_BAR_HEIGHT: u16 = 1;

/// Height of the divider row between the header and the rest of the app.
pub const DIVIDER_HEIGHT: u16 = 1;

/// Height of the divider row directly above the status bar.
pub const STATUS_DIVIDER_HEIGHT: u16 = 1;

/// Height of the divider row directly above the git info bar.
pub const INFO_DIVIDER_HEIGHT: u16 = 1;

/// The computed sub-rects for the main FlightDeck layout (SPECS §20).
///
/// ```text
/// ┌─────────────────────────────────────────────────────────────────┐
/// │ header (full-width logo)                                         │
/// ├─────────────────────────────────────────────────────────────────┤
/// │ divider                                                         │
/// ├──────────────────────┬──────────────────────────────────────────┤
/// │ sidebar              │ child_tabs                               │
/// │                      ├──────────────────────────────────────────┤
/// │                      │                                          │
/// │                      │          terminal                        │
/// │                      │                                          │
/// │                      ├──────────────────────────────────────────┤
/// │                      │ info_divider                             │
/// │                      ├──────────────────────────────────────────┤
/// │                      │ info_bar                                 │
/// │                      ├──────────────────────────────────────────┤
/// │                      │ status_divider                           │
/// │                      ├──────────────────────────────────────────┤
/// │                      │ status_bar                               │
/// └──────────────────────┴──────────────────────────────────────────┘
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MainLayout {
    /// Full-width branded header (logo) row at the very top.
    pub header: Rect,
    /// Full-width project tab row, directly below the header.
    pub project_tabs: Rect,
    /// Full-width divider row between the header and the rest of the app.
    pub divider: Rect,
    /// Left Agent Tabs sidebar.
    pub sidebar: Rect,
    /// Horizontal child-terminal tab bar (top of the main pane).
    pub child_tabs: Rect,
    /// Active terminal viewport.
    pub terminal: Rect,
    /// Full-width divider row directly above the git info bar.
    pub info_divider: Rect,
    /// Git info bar (branch + change counts), just above the status bar.
    pub info_bar: Rect,
    /// Full-width divider row directly above the status bar.
    pub status_divider: Rect,
    /// Status/action bar (bottom of the main pane).
    pub status_bar: Rect,
}

/// Compute the main layout from a total `area` (SPECS §20).
///
/// Returns [`MainLayout`] with the four sub-rects. The sidebar is
/// [`SIDEBAR_WIDTH`] columns wide; the rest fills the right pane. The right
/// pane is split vertically: one row for the child tab bar, one row for the
/// status bar, and the remainder for the terminal viewport.
///
/// If the area is too small (e.g. less than the minimum heights/widths),
/// sub-rects may be zero-sized — callers must handle this gracefully.
pub fn compute(area: Rect) -> MainLayout {
    // Full-width top band: header (logo) | project tabs | divider | body.
    let [header, project_tabs, divider, body] = Layout::vertical([
        Constraint::Length(HEADER_HEIGHT),
        Constraint::Length(PROJECT_TAB_BAR_HEIGHT),
        Constraint::Length(DIVIDER_HEIGHT),
        Constraint::Fill(1),
    ])
    .areas(area);

    // Split the body horizontally: sidebar | main pane.
    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Fill(1)]).areas(body);

    // Split main pane vertically: child_tabs | terminal | info_divider
    // | info_bar | status_divider | status_bar.
    let [child_tabs, terminal, info_divider, info_bar, status_divider, status_bar] =
        Layout::vertical([
            Constraint::Length(CHILD_TAB_BAR_HEIGHT),
            Constraint::Fill(1),
            Constraint::Length(INFO_DIVIDER_HEIGHT),
            Constraint::Length(INFO_BAR_HEIGHT),
            Constraint::Length(STATUS_DIVIDER_HEIGHT),
            Constraint::Length(STATUS_BAR_HEIGHT),
        ])
        .areas(main);

    MainLayout {
        header,
        project_tabs,
        divider,
        sidebar,
        child_tabs,
        terminal,
        info_divider,
        info_bar,
        status_divider,
        status_bar,
    }
}

/// Height of each split-view column's header row (the terminal's label).
pub const SPLIT_HEADER_HEIGHT: u16 = 1;

/// One column of the split view: a header row (the terminal label) above its
/// terminal `viewport`. `col` is the full column span (header + viewport) and is
/// used to place the inter-column separators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitColumn {
    /// The whole column rect (header + viewport).
    pub col: Rect,
    /// The header row (terminal label).
    pub header: Rect,
    /// The terminal viewport below the header.
    pub viewport: Rect,
}

/// The full main-pane region used by split view: the child-tab-bar row (which
/// split view does not draw) merged with the terminal viewport, so the columns
/// reclaim that row. Falls back gracefully on degenerate layouts.
pub fn split_region(ml: &MainLayout) -> Rect {
    Rect {
        x: ml.child_tabs.x,
        y: ml.child_tabs.y,
        width: ml.terminal.width,
        height: ml.child_tabs.height.saturating_add(ml.terminal.height),
    }
}

/// Divide `region` into `n` equal-width columns separated by a one-column gutter,
/// each with a [`SPLIT_HEADER_HEIGHT`]-row header reserved at the top (SPECS:
/// split view). Remainder columns are widened by one so the whole width is used.
/// Returns an empty vec for `n == 0` or a zero-sized region.
pub fn split_columns(region: Rect, n: usize) -> Vec<SplitColumn> {
    let mut out = Vec::new();
    if n == 0 || region.width == 0 || region.height == 0 {
        return out;
    }
    let n_u16 = n as u16;
    // One-column gutters between adjacent columns (n - 1 of them).
    let gutters = n_u16 - 1;
    let avail = region.width.saturating_sub(gutters);
    let base = avail / n_u16;
    let extra = avail % n_u16; // first `extra` columns get one more column

    let header_h = SPLIT_HEADER_HEIGHT.min(region.height);
    let mut x = region.x;
    for i in 0..n {
        let w = base + if (i as u16) < extra { 1 } else { 0 };
        let col = Rect {
            x,
            y: region.y,
            width: w,
            height: region.height,
        };
        let header = Rect {
            x,
            y: region.y,
            width: w,
            height: header_h,
        };
        let viewport = Rect {
            x,
            y: region.y.saturating_add(header_h),
            width: w,
            height: region.height.saturating_sub(header_h),
        };
        out.push(SplitColumn {
            col,
            header,
            viewport,
        });
        x = x.saturating_add(w).saturating_add(1); // skip the gutter column
    }
    out
}

/// Compute the centered overlay area for modals/palette (e.g. help, command
/// palette). Returns a [`Rect`] that is at most `max_w` × `max_h` and centered
/// in `area`.
pub fn centered_overlay(area: Rect, max_w: u16, max_h: u16) -> Rect {
    let w = max_w.min(area.width);
    let h = max_h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn full_terminal() -> Rect {
        Rect::new(0, 0, 120, 40)
    }

    /// Total height consumed by the full-width top band
    /// (header + project tabs + divider).
    const TOP_BAND: u16 = HEADER_HEIGHT + PROJECT_TAB_BAR_HEIGHT + DIVIDER_HEIGHT;

    #[test]
    fn header_project_tabs_and_divider_span_full_width_at_top() {
        let area = full_terminal();
        let layout = compute(area);
        // Header is the very first row, full width.
        assert_eq!(layout.header.y, 0);
        assert_eq!(layout.header.x, 0);
        assert_eq!(layout.header.width, area.width);
        assert_eq!(layout.header.height, HEADER_HEIGHT);
        // Project tab row sits directly below the header, full width.
        assert_eq!(layout.project_tabs.y, HEADER_HEIGHT);
        assert_eq!(layout.project_tabs.x, 0);
        assert_eq!(layout.project_tabs.width, area.width);
        assert_eq!(layout.project_tabs.height, PROJECT_TAB_BAR_HEIGHT);
        // Divider sits directly below the project tabs, also full width.
        assert_eq!(layout.divider.y, HEADER_HEIGHT + PROJECT_TAB_BAR_HEIGHT);
        assert_eq!(layout.divider.x, 0);
        assert_eq!(layout.divider.width, area.width);
        assert_eq!(layout.divider.height, DIVIDER_HEIGHT);
        // The rest of the app begins below the divider.
        assert_eq!(layout.divider.bottom(), layout.sidebar.top());
        assert_eq!(layout.divider.bottom(), layout.child_tabs.top());
    }

    #[test]
    fn sidebar_has_correct_width() {
        let layout = compute(full_terminal());
        assert_eq!(layout.sidebar.width, SIDEBAR_WIDTH);
        assert_eq!(layout.sidebar.x, 0);
        assert_eq!(layout.sidebar.y, TOP_BAND);
    }

    #[test]
    fn sidebar_fills_remaining_height_below_top_band() {
        let area = full_terminal();
        let layout = compute(area);
        assert_eq!(layout.sidebar.height, area.height - TOP_BAND);
    }

    #[test]
    fn main_pane_starts_after_sidebar() {
        let area = full_terminal();
        let layout = compute(area);
        assert_eq!(layout.child_tabs.x, SIDEBAR_WIDTH);
        assert_eq!(layout.terminal.x, SIDEBAR_WIDTH);
        assert_eq!(layout.status_bar.x, SIDEBAR_WIDTH);
    }

    #[test]
    fn main_pane_fills_remaining_width() {
        let area = full_terminal();
        let expected_main_w = area.width - SIDEBAR_WIDTH;
        let layout = compute(area);
        assert_eq!(layout.child_tabs.width, expected_main_w);
        assert_eq!(layout.terminal.width, expected_main_w);
        assert_eq!(layout.info_bar.width, expected_main_w);
        assert_eq!(layout.status_bar.width, expected_main_w);
    }

    #[test]
    fn info_bar_sits_directly_above_status_bar() {
        let area = full_terminal();
        let layout = compute(area);
        assert_eq!(layout.info_bar.height, INFO_BAR_HEIGHT);
        assert_eq!(layout.info_bar.x, SIDEBAR_WIDTH);
        // A divider row now sits between the info bar and the status bar.
        assert_eq!(layout.info_bar.bottom(), layout.status_divider.top());
        assert_eq!(layout.status_divider.bottom(), layout.status_bar.top());
        assert_eq!(layout.status_divider.height, STATUS_DIVIDER_HEIGHT);
        // A divider row also sits between the terminal and the info bar.
        assert_eq!(layout.terminal.bottom(), layout.info_divider.top());
        assert_eq!(layout.info_divider.bottom(), layout.info_bar.top());
        assert_eq!(layout.info_divider.height, INFO_DIVIDER_HEIGHT);
    }

    #[test]
    fn child_tabs_bar_height() {
        let layout = compute(full_terminal());
        assert_eq!(layout.child_tabs.height, CHILD_TAB_BAR_HEIGHT);
        // The child tab bar is the first row of the body, below the top band.
        assert_eq!(layout.child_tabs.y, TOP_BAND);
    }

    #[test]
    fn status_bar_height_and_at_bottom() {
        let area = full_terminal();
        let layout = compute(area);
        assert_eq!(layout.status_bar.height, STATUS_BAR_HEIGHT);
        assert_eq!(
            layout.status_bar.y,
            area.height - STATUS_BAR_HEIGHT,
            "status bar must be at bottom of main pane"
        );
    }

    #[test]
    fn terminal_viewport_fills_remaining() {
        let area = full_terminal();
        let layout = compute(area);
        // top band (2) + child_tabs (1) + terminal (?) + info_divider (1)
        // + info_bar (1) + status_divider (1) + status (1).
        let expected_h = area.height
            - TOP_BAND
            - CHILD_TAB_BAR_HEIGHT
            - INFO_DIVIDER_HEIGHT
            - INFO_BAR_HEIGHT
            - STATUS_DIVIDER_HEIGHT
            - STATUS_BAR_HEIGHT;
        assert_eq!(layout.terminal.height, expected_h);
        assert_eq!(layout.terminal.y, TOP_BAND + CHILD_TAB_BAR_HEIGHT);
    }

    #[test]
    fn rects_do_not_overlap() {
        let layout = compute(full_terminal());
        // Sidebar and main pane must not overlap horizontally.
        assert!(layout.sidebar.right() <= layout.terminal.left());
        // Vertical panes must not overlap within main pane.
        assert!(layout.child_tabs.bottom() <= layout.terminal.top());
        assert!(layout.terminal.bottom() <= layout.info_divider.top());
        assert!(layout.info_divider.bottom() <= layout.info_bar.top());
        assert!(layout.info_bar.bottom() <= layout.status_divider.top());
        assert!(layout.status_divider.bottom() <= layout.status_bar.top());
    }

    #[test]
    fn total_area_accounted_for() {
        let area = full_terminal();
        let layout = compute(area);
        // Sidebar spans the full height below the top band.
        assert_eq!(layout.sidebar.height, area.height - TOP_BAND);
        // Width sum: sidebar + main pane columns.
        assert_eq!(layout.sidebar.width + layout.child_tabs.width, area.width);
        // Height sum: top band + main pane rows == full height.
        assert_eq!(
            TOP_BAND
                + layout.child_tabs.height
                + layout.terminal.height
                + layout.info_divider.height
                + layout.info_bar.height
                + layout.status_divider.height
                + layout.status_bar.height,
            area.height
        );
    }

    #[test]
    fn minimum_area_does_not_panic() {
        // Degenerate area: should produce valid (possibly zero-sized) rects.
        let _ = compute(Rect::new(0, 0, 0, 0));
        let _ = compute(Rect::new(0, 0, 1, 1));
        let _ = compute(Rect::new(0, 0, 10, 3));
    }

    #[test]
    fn centered_overlay_fits_within_area() {
        let area = Rect::new(0, 0, 80, 24);
        let overlay = centered_overlay(area, 60, 16);
        assert_eq!(overlay.width, 60);
        assert_eq!(overlay.height, 16);
        assert!(overlay.left() >= area.left());
        assert!(overlay.right() <= area.right());
        assert!(overlay.top() >= area.top());
        assert!(overlay.bottom() <= area.bottom());
    }

    #[test]
    fn centered_overlay_clamps_to_area() {
        let area = Rect::new(0, 0, 40, 10);
        let overlay = centered_overlay(area, 200, 200);
        assert_eq!(overlay.width, 40);
        assert_eq!(overlay.height, 10);
    }

    #[test]
    fn split_region_reclaims_the_tab_bar_row() {
        let ml = compute(full_terminal());
        let region = split_region(&ml);
        // Starts at the tab bar row and spans both it and the terminal viewport.
        assert_eq!(region.x, ml.child_tabs.x);
        assert_eq!(region.y, ml.child_tabs.y);
        assert_eq!(region.width, ml.terminal.width);
        assert_eq!(region.height, ml.child_tabs.height + ml.terminal.height);
    }

    #[test]
    fn split_columns_divide_width_equally_with_gutters() {
        // Width 31, 3 columns → 2 gutters → 29 usable → 9,10,10? No: base=9,
        // extra=2 so first two columns get 10, last gets 9. Sum + gutters = 31.
        let region = Rect::new(0, 0, 31, 10);
        let cols = split_columns(region, 3);
        assert_eq!(cols.len(), 3);
        let total: u16 = cols.iter().map(|c| c.col.width).sum::<u16>() + 2; // + gutters
        assert_eq!(total, region.width);
        // Columns are ordered left to right with a one-column gutter between.
        assert_eq!(cols[0].col.x, 0);
        assert_eq!(cols[1].col.x, cols[0].col.right() + 1);
        assert_eq!(cols[2].col.x, cols[1].col.right() + 1);
        // Equal-ish: widths differ by at most one.
        let max = cols.iter().map(|c| c.col.width).max().unwrap();
        let min = cols.iter().map(|c| c.col.width).min().unwrap();
        assert!(max - min <= 1);
    }

    #[test]
    fn split_columns_reserve_a_header_row() {
        let region = Rect::new(2, 5, 40, 12);
        let cols = split_columns(region, 2);
        for c in &cols {
            assert_eq!(c.header.height, SPLIT_HEADER_HEIGHT);
            assert_eq!(c.header.y, region.y);
            assert_eq!(c.viewport.y, region.y + SPLIT_HEADER_HEIGHT);
            assert_eq!(c.viewport.height, region.height - SPLIT_HEADER_HEIGHT);
            // Header and viewport share the column's x and width.
            assert_eq!(c.header.x, c.col.x);
            assert_eq!(c.viewport.x, c.col.x);
            assert_eq!(c.header.width, c.col.width);
            assert_eq!(c.viewport.width, c.col.width);
        }
    }

    #[test]
    fn split_columns_single_column_has_no_gutter() {
        let region = Rect::new(0, 0, 20, 8);
        let cols = split_columns(region, 1);
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].col.width, region.width);
        assert_eq!(cols[0].col.x, region.x);
    }

    #[test]
    fn split_columns_degenerate_inputs_do_not_panic() {
        assert!(split_columns(Rect::new(0, 0, 0, 0), 3).is_empty());
        assert!(split_columns(Rect::new(0, 0, 40, 10), 0).is_empty());
        // Very narrow region with many columns: no panic, widths may be zero.
        let _ = split_columns(Rect::new(0, 0, 2, 4), 5);
    }
}
