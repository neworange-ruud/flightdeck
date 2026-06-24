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

/// Height of the divider row between the header and the rest of the app.
pub const DIVIDER_HEIGHT: u16 = 1;

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
/// │                      │ info_bar                                 │
/// │                      ├──────────────────────────────────────────┤
/// │                      │ status_bar                               │
/// └──────────────────────┴──────────────────────────────────────────┘
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MainLayout {
    /// Full-width branded header (logo) row at the very top.
    pub header: Rect,
    /// Full-width divider row between the header and the rest of the app.
    pub divider: Rect,
    /// Left Agent Tabs sidebar.
    pub sidebar: Rect,
    /// Horizontal child-terminal tab bar (top of the main pane).
    pub child_tabs: Rect,
    /// Active terminal viewport.
    pub terminal: Rect,
    /// Git info bar (branch + change counts), just above the status bar.
    pub info_bar: Rect,
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
    // Full-width top band: header (logo) | divider | body.
    let [header, divider, body] = Layout::vertical([
        Constraint::Length(HEADER_HEIGHT),
        Constraint::Length(DIVIDER_HEIGHT),
        Constraint::Fill(1),
    ])
    .areas(area);

    // Split the body horizontally: sidebar | main pane.
    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Fill(1)]).areas(body);

    // Split main pane vertically: child_tabs | terminal | info_bar | status_bar.
    let [child_tabs, terminal, info_bar, status_bar] = Layout::vertical([
        Constraint::Length(CHILD_TAB_BAR_HEIGHT),
        Constraint::Fill(1),
        Constraint::Length(INFO_BAR_HEIGHT),
        Constraint::Length(STATUS_BAR_HEIGHT),
    ])
    .areas(main);

    MainLayout {
        header,
        divider,
        sidebar,
        child_tabs,
        terminal,
        info_bar,
        status_bar,
    }
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

    /// Total height consumed by the full-width top band (header + divider).
    const TOP_BAND: u16 = HEADER_HEIGHT + DIVIDER_HEIGHT;

    #[test]
    fn header_and_divider_span_full_width_at_top() {
        let area = full_terminal();
        let layout = compute(area);
        // Header is the very first row, full width.
        assert_eq!(layout.header.y, 0);
        assert_eq!(layout.header.x, 0);
        assert_eq!(layout.header.width, area.width);
        assert_eq!(layout.header.height, HEADER_HEIGHT);
        // Divider sits directly below the header, also full width.
        assert_eq!(layout.divider.y, HEADER_HEIGHT);
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
        assert_eq!(layout.info_bar.bottom(), layout.status_bar.top());
        assert_eq!(layout.terminal.bottom(), layout.info_bar.top());
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
        // top band (2) + child_tabs (1) + terminal (?) + info_bar (1) + status (1).
        let expected_h = area.height
            - TOP_BAND
            - CHILD_TAB_BAR_HEIGHT
            - INFO_BAR_HEIGHT
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
        assert!(layout.terminal.bottom() <= layout.info_bar.top());
        assert!(layout.info_bar.bottom() <= layout.status_bar.top());
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
                + layout.info_bar.height
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
}
