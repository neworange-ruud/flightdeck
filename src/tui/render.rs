//! Drawing functions for the FlightDeck TUI (T8, SPECS §20, §21, §23, §24).
//!
//! All render functions are pure: they consume state and write into a
//! [`ratatui::Frame`]; they never call git/fs/pty directly.
//!
//! ## Git status cache
//!
//! The sidebar and git status panel need data (dirty flag, ahead/behind, base
//! drift) that is not cached in [`AppState`]. The wiring layer (T9) is
//! responsible for populating a [`GitStatusCache`] (a `HashMap<String,
//! WorktreeStatus>` keyed by tab id) periodically and passing it into
//! [`draw`]. If a tab id is absent from the cache, those indicators render as
//! "?" or blank — this module never panics on a missing entry.
//!
//! T9 integration notes:
//! - Call [`draw`] inside `Terminal::draw(|frame| ...)` once per event-loop
//!   tick with a freshly-computed layout via [`crate::tui::layout::compute`].
//! - Populate [`GitStatusCache`] by calling `collect_status` for each tab in a
//!   background task and updating the cache on completion.
//! - Pass [`UiOverlays`] to control which (if any) overlay is visible.

use std::collections::HashMap;

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::modes::InputMode;
use crate::app::state::{AppState, TabPhase};
use crate::git::status::WorktreeStatus;
use crate::tui::layout;
use crate::tui::palette::CommandPalette;
use crate::tui::selection::Selection;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Git status cache keyed by tab id (SPECS §20, §21).
///
/// Populated by T9; absent entries render as unknown. Never causes a panic.
pub type GitStatusCache = HashMap<String, WorktreeStatus>;

/// Which overlay (if any) is currently shown on top of the main layout.
#[derive(Debug, Clone, Default)]
pub enum UiOverlay {
    /// No overlay — normal main view.
    #[default]
    None,
    /// Command palette with the current [`CommandPalette`] state.
    Palette(CommandPalette),
    /// Help / keybindings overlay.
    Help,
    /// Git status panel for the active tab, optionally with a PR URL.
    GitStatus {
        /// The git status data (typically from [`GitStatusCache`]).
        status: WorktreeStatus,
        /// A PR compare URL, if available (SPECS §14, §21).
        pr_url: Option<String>,
    },
    /// A transient toast/message line (effect feedback).
    Message(String),
}

// ---------------------------------------------------------------------------
// Mouse hit-testing (clickable tabs)
// ---------------------------------------------------------------------------

/// Rows the sidebar header ("Agents") occupies before the first tab.
const SIDEBAR_HEADER_ROWS: u16 = 1;
/// Rows each agent tab occupies in the sidebar: divider + name + agent + git.
const SIDEBAR_ROWS_PER_TAB: u16 = 4;

/// Which child-terminal tab a click landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildTarget {
    /// The primary agent terminal.
    Primary,
    /// The child shell terminal at this index.
    Child(usize),
}

/// What a mouse click resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    /// An Agent Tab in the sidebar (by index).
    AgentTab(usize),
    /// The sidebar chrome itself (header/heading/empty space below the tabs) —
    /// anywhere in the left panel that is not an Agent Tab row. A click here
    /// still focuses the app (APP mode) without changing the selected tab, so
    /// clicking the sidebar works even with zero or one agents (SPECS §23).
    Sidebar,
    /// A child-terminal tab in the main pane.
    Child(ChildTarget),
}

/// Resolve a click at `(col, row)` (terminal coordinates) against the layout for
/// `area`, returning the agent tab or child-terminal tab it lands on, if any.
pub fn hit_test(area: Rect, state: &AppState, col: u16, row: u16) -> Option<HitTarget> {
    let ml = layout::compute(area);
    if rect_contains(ml.sidebar, col, row) {
        // A click on an actual Agent Tab row selects it; anywhere else in the
        // sidebar (logo header, "Agents" heading, or the empty space below the
        // last tab) resolves to the sidebar chrome so the click still focuses
        // the app — even with no agents or just one (SPECS §23).
        return Some(
            sidebar_tab_at(ml.sidebar, state.tabs.len(), col, row)
                .map_or(HitTarget::Sidebar, HitTarget::AgentTab),
        );
    }
    if state.split_view {
        // In split view a click on a column's header row switches to that
        // terminal. Clicks in the column *body* are not switch targets here —
        // they begin a text selection (handled by the mouse wiring, which still
        // focuses the column). This mirrors normal mode, where the tab bar
        // switches and the viewport selects.
        let region = layout::split_region(&ml);
        if rect_contains(region, col, row) {
            let entries = child_tab_entries(state);
            let cols = layout::split_columns(region, entries.len());
            for ((target, _label), c) in entries.iter().zip(cols.iter()) {
                if rect_contains(c.header, col, row) {
                    return Some(HitTarget::Child(*target));
                }
            }
        }
        return None;
    }
    if rect_contains(ml.child_tabs, col, row) {
        for (target, start, w) in child_tab_positions(ml.child_tabs, state) {
            if col >= start && col < start.saturating_add(w) {
                return Some(HitTarget::Child(target));
            }
        }
    }
    None
}

/// Whether `(col, row)` is inside `r`.
fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

/// Map a click in the sidebar `area` to an agent tab index (or `None`).
fn sidebar_tab_at(area: Rect, tab_count: usize, col: u16, row: u16) -> Option<usize> {
    let inner = Block::default().borders(Borders::RIGHT).inner(area);
    if col < inner.x || col >= inner.x.saturating_add(inner.width) {
        return None;
    }
    let first = inner.y.saturating_add(SIDEBAR_HEADER_ROWS);
    if row < first {
        return None;
    }
    let idx = ((row - first) / SIDEBAR_ROWS_PER_TAB) as usize;
    (idx < tab_count).then_some(idx)
}

/// The child-terminal tab entries for the selected tab: the primary "agent" tab
/// plus one per child shell. Shared by rendering and hit-testing so positions
/// always agree.
fn child_tab_entries(state: &AppState) -> Vec<(ChildTarget, String)> {
    let mut v = vec![(ChildTarget::Primary, "agent".to_string())];
    if let Some(tab) = state.selected() {
        for i in 0..tab.session.child_count() {
            v.push((ChildTarget::Child(i), format!("shell {}", i + 1)));
        }
    }
    v
}

/// Compute `(target, start_col, width)` for each child-terminal tab segment,
/// matching exactly how [`draw_child_tab_bar`] lays them out.
fn child_tab_positions(area: Rect, state: &AppState) -> Vec<(ChildTarget, u16, u16)> {
    let mut out = Vec::new();
    let mut x = area.x;
    for (i, (target, label)) in child_tab_entries(state).into_iter().enumerate() {
        if i > 0 {
            x = x.saturating_add(3); // " | " separator
        }
        let w = label.chars().count() as u16 + 2; // " label "
        out.push((target, x, w));
        x = x.saturating_add(w);
    }
    out
}

/// A full-width horizontal divider line (used between sidebar tabs).
fn divider_line(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(Color::DarkGray),
    ))
}

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Draw the complete FlightDeck UI into `frame`.
///
/// Called once per tick by T9 inside `Terminal::draw(|frame| draw(frame, ...))`.
pub fn draw(
    frame: &mut Frame,
    state: &AppState,
    cache: &GitStatusCache,
    overlay: &UiOverlay,
    now_ms: u64,
) {
    let area = frame.area();
    let ml = layout::compute(area);

    draw_header(frame, ml.header);
    let divider = Paragraph::new(divider_line(ml.divider.width as usize));
    frame.render_widget(divider, ml.divider);
    draw_sidebar(frame, state, cache, ml.sidebar, now_ms);
    if state.split_view {
        // Split view reclaims the tab-bar row and lays the selected tab's
        // terminals out side by side in equal-width columns.
        draw_split_view(frame, state, layout::split_region(&ml), now_ms);
    } else {
        draw_child_tab_bar(frame, state, ml.child_tabs);
        draw_terminal_viewport(frame, state, ml.terminal, now_ms);
    }
    let info_divider = Paragraph::new(divider_line(ml.info_divider.width as usize));
    frame.render_widget(info_divider, ml.info_divider);
    draw_info_bar(frame, state, cache, ml.info_bar);
    let status_divider = Paragraph::new(divider_line(ml.status_divider.width as usize));
    frame.render_widget(status_divider, ml.status_divider);
    draw_status_bar(frame, state, ml.status_bar);

    // Draw overlay on top if active.
    match overlay {
        UiOverlay::None => {}
        UiOverlay::Message(msg) => draw_message_toast(frame, msg, area),
        UiOverlay::Palette(palette) => draw_palette_overlay(frame, palette, area),
        UiOverlay::Help => draw_help_overlay(frame, area),
        UiOverlay::GitStatus { status, pr_url } => {
            draw_git_status_overlay(frame, status, pr_url.as_deref(), area);
        }
    }
}

// ---------------------------------------------------------------------------
// Branded header (logo)
// ---------------------------------------------------------------------------

/// The ░▒▓ gradient ramps that flank the wordmark, read *toward* the centered
/// brand: solid blocks on the outside fade down to clear next to the text. The
/// remaining width on each side is filled with solid `█` so the title bar spans
/// the whole window (e.g. `█████▓▓▓▒▒▒░░░ · F L I G H T D E C K · ░░░▒▒▒▓▓▓█████`).
const RAMP_IN: &str = "▓▓▓▒▒▒░░░";
const RAMP_OUT: &str = "░░░▒▒▒▓▓▓";
/// The brand wordmark, spaced (wide) and tight (narrow) variants.
const BRAND_WIDE: &str = " · F L I G H T D E C K · ";
const BRAND_NARROW: &str = " F·L·I·G·H·T·D·E·C·K ";

/// Draw the full-width branded header: the wordmark centered with the block
/// gradient filling the row edge to edge.
pub fn draw_header(frame: &mut Frame, area: Rect) {
    let line = header_line(area.width as usize);
    let para = Paragraph::new(line).alignment(Alignment::Center);
    frame.render_widget(para, area);
}

/// Build the full-width logo [`Line`] for a given width: the wordmark (wide when
/// it fits, tight when it does not) framed by the ░▒▓ ramps and padded with solid
/// `█` blocks out to both edges so the bar always fills the window. Falls back to
/// a plain truncated brand when even the tight framed form is too wide for the
/// row. Exported for testing.
pub fn header_line(width: usize) -> Line<'static> {
    let block_style = Style::default().fg(Color::Cyan);
    let brand_style = Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);

    let ramp = RAMP_IN.chars().count() + RAMP_OUT.chars().count();

    // Pick the widest wordmark whose framed form (brand + both ramps) fits.
    let brand = if width >= BRAND_WIDE.chars().count() + ramp {
        BRAND_WIDE
    } else if width >= BRAND_NARROW.chars().count() + ramp {
        BRAND_NARROW
    } else {
        // Too narrow for the framed logo: show the brand alone, truncated to fit.
        let truncated: String = "FLIGHTDECK".chars().take(width).collect();
        return Line::from(Span::styled(truncated, brand_style));
    };

    // Fill the leftover columns with solid blocks, split across both sides so the
    // wordmark stays centered (any odd column goes to the right side).
    let fill = width - (brand.chars().count() + ramp);
    let left_blocks = fill / 2;
    let right_blocks = fill - left_blocks;

    Line::from(vec![
        Span::styled(format!("{}{RAMP_IN}", "█".repeat(left_blocks)), block_style),
        Span::styled(brand, brand_style),
        Span::styled(
            format!("{RAMP_OUT}{}", "█".repeat(right_blocks)),
            block_style,
        ),
    ])
}

// ---------------------------------------------------------------------------
// Sidebar (SPECS §20, §24)
// ---------------------------------------------------------------------------

/// Draw the left Agent Tabs sidebar.
pub fn draw_sidebar(
    frame: &mut Frame,
    state: &AppState,
    cache: &GitStatusCache,
    area: Rect,
    now_ms: u64,
) {
    let block = Block::default().borders(Borders::RIGHT);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    // Header row (SIDEBAR_HEADER_ROWS): centered "Agents" title.
    lines.push(
        Line::from(Span::styled(
            "Agents",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
        .alignment(Alignment::Center),
    );

    if state.tabs.is_empty() {
        lines.push(divider_line(width));
        lines.push(Line::from(Span::styled(
            "No tabs. Ctrl-n to create.",
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    // Each tab block is SIDEBAR_ROWS_PER_TAB rows: divider, name, agent, git —
    // a divider above every tab including the first (SPECS §20).
    for (i, tab) in state.tabs.iter().enumerate() {
        let selected = state.selected_tab == Some(i);
        let ds = tab.display_status(now_ms);
        let git = cache.get(&tab.meta.id);

        // Divider (top of the tab block).
        lines.push(divider_line(width));

        // Name (with selection marker).
        let name_style = if selected {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let marker = if selected { "▸ " } else { "  " };

        // A tab whose worktree is still being materialized on a background
        // worker shows an animated spinner instead of a process/status line, so
        // the user always sees that something is happening (SPECS §16/§17).
        if tab.phase == TabPhase::Creating {
            let spin = Style::default().fg(Color::Cyan);
            lines.push(Line::from(vec![
                Span::styled(marker, name_style),
                Span::styled(format!("{} ", spinner_frame(now_ms)), spin),
                Span::styled(tab.meta.name.clone(), name_style),
            ]));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("creating worktree…", spin),
            ]));
            // Keep the block height uniform (divider/name/status/git rows).
            lines.push(Line::from(Span::raw("")));
            continue;
        }
        // A colour-coded status dot on the name line so idle (green) vs in
        // progress (blue) vs error (red) is glanceable per tab (SPECS §24).
        // Manual override takes visual priority but never hides the dot.
        let dot_color = ds
            .manual
            .map(|_| Color::Cyan)
            .unwrap_or_else(|| status_label_color(ds.interpreted).1);
        lines.push(Line::from(vec![
            Span::styled(marker, name_style),
            Span::styled("● ", Style::default().fg(dot_color)),
            Span::styled(tab.meta.name.clone(), name_style),
        ]));

        // Agent name + simplified status, e.g. "Claude Code [in progress]".
        // A manual override (cyan) takes visual priority; otherwise the
        // interpreted status collapses to idle / in progress / error.
        let agent_name = state
            .registry
            .get(&tab.meta.agent)
            .map(|a| a.display_name.clone())
            .unwrap_or_else(|| tab.meta.agent.clone());
        let (status_label, status_color) = match ds.manual {
            Some(manual) => (manual.as_str().to_string(), Color::Cyan),
            None => {
                let (label, color) = status_label_color(ds.interpreted);
                (label.to_string(), color)
            }
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(agent_name, Style::default().fg(Color::Gray)),
            Span::raw(" "),
            Span::styled(
                format!("[{status_label}]"),
                Style::default().fg(status_color),
            ),
        ]));

        // Git indicators (dirty, ahead/behind, base drift, recovered/existing).
        lines.push(build_git_indicator_line(tab, git));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// A braille spinner frame chosen from the wall clock (≈12.5 fps), used to
/// animate in-progress work (e.g. a tab whose worktree is being created).
pub fn spinner_frame(now_ms: u64) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[((now_ms / 80) % FRAMES.len() as u64) as usize]
}

/// Collapse an interpreted status to a glanceable sidebar label + colour
/// (SPECS §24): in progress (cyan), error (red), otherwise idle (green).
fn status_label_color(status: crate::contracts::InterpretedStatus) -> (&'static str, Color) {
    use crate::contracts::InterpretedStatus::*;
    match status {
        Starting | Running | Working => ("in progress", Color::Cyan),
        Failed | SessionLost => ("error", Color::Red),
        Idle | WaitingForInput | NeedsAttention | Completed | Stopped | Recovered | Unknown => {
            ("idle", Color::Green)
        }
    }
}

/// Build a single line of git indicators for a sidebar tab row.
fn build_git_indicator_line(
    tab: &crate::app::state::RuntimeTab,
    git: Option<&WorktreeStatus>,
) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];

    // Recovered / attached markers.
    if tab.meta.recovered {
        spans.push(Span::styled(
            "[recovered]",
            Style::default().fg(Color::Magenta),
        ));
        spans.push(Span::raw(" "));
    }
    if tab.meta.attached_existing_branch {
        spans.push(Span::styled("[existing]", Style::default().fg(Color::Cyan)));
        spans.push(Span::raw(" "));
    }

    match git {
        None => {
            spans.push(Span::styled("git: ?", Style::default().fg(Color::DarkGray)));
        }
        Some(ws) => {
            // Dirty indicator.
            if ws.dirty {
                spans.push(Span::styled("~dirty", Style::default().fg(Color::Yellow)));
                spans.push(Span::raw(" "));
            }
            // Ahead/behind vs upstream.
            if ws.upstream.is_some() {
                if ws.ahead > 0 || ws.behind > 0 {
                    let ab = format!("+{} -{}", ws.ahead, ws.behind);
                    spans.push(Span::styled(ab, Style::default().fg(Color::Cyan)));
                    spans.push(Span::raw(" "));
                }
            } else {
                spans.push(Span::styled(
                    "no-upstream",
                    Style::default().fg(Color::DarkGray),
                ));
                spans.push(Span::raw(" "));
            }
            // Base drift.
            if ws.base_drift > 0 {
                let drift = format!("drift:{}", ws.base_drift);
                spans.push(Span::styled(drift, Style::default().fg(Color::Magenta)));
            }
        }
    }

    Line::from(spans)
}

// ---------------------------------------------------------------------------
// Child terminal tab bar (SPECS §19, §20)
// ---------------------------------------------------------------------------

/// Draw the horizontal child terminal tab bar inside the main pane (SPECS §19).
pub fn draw_child_tab_bar(frame: &mut Frame, state: &AppState, area: Rect) {
    let Some(tab) = state.selected() else {
        let empty = Paragraph::new(" No tab selected ").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty, area);
        return;
    };

    // Build "agent | shell 1 | shell 2 …" from the shared segmentation so the
    // rendered positions line up with mouse hit-testing (SPECS §19).
    let active = tab.session.selected_child(); // None = primary
    let mut spans: Vec<Span> = Vec::new();
    for (i, (target, label)) in child_tab_entries(state).into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
        }
        let is_active = match target {
            ChildTarget::Primary => active.is_none(),
            ChildTarget::Child(c) => active == Some(c),
        };
        let style = if is_active {
            let bg = if matches!(target, ChildTarget::Primary) {
                Color::Yellow
            } else {
                Color::Cyan
            };
            Style::default()
                .fg(Color::Black)
                .bg(bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(format!(" {label} "), style));
    }

    let para = Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Reset));
    frame.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Terminal viewport (SPECS §20)
// ---------------------------------------------------------------------------

/// Draw the active terminal viewport (SPECS §20): the VT100 screen of the
/// selected tab's active terminal (primary agent, or the selected child shell),
/// rendered cell-by-cell from its parser.
pub fn draw_terminal_viewport(frame: &mut Frame, state: &AppState, area: Rect, now_ms: u64) {
    let Some(tab) = state.selected() else {
        let p = Paragraph::new(
            "\n  FlightDeck — no Agent Tab selected.\n  Press Ctrl-n to create one.",
        )
        .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, area);
        return;
    };

    // While the worktree is being created on a background worker there is no
    // session yet: show an animated progress message so the UI never looks
    // frozen (SPECS §16/§17).
    if tab.phase == TabPhase::Creating {
        let msg = Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} ", spinner_frame(now_ms)),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                format!("Creating worktree for {}…", tab.meta.branch),
                Style::default().fg(Color::Cyan),
            ),
        ]))
        .alignment(Alignment::Center);
        let inner = Rect {
            y: area.y + area.height / 2,
            height: 1,
            ..area
        };
        frame.render_widget(msg, inner);
        return;
    }

    let Some(term) = tab.session.active() else {
        let p =
            Paragraph::new("  (terminal starting…)").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, area);
        return;
    };

    let focused = state.mode() == InputMode::Terminal;
    render_screen(frame, area, term.screen(), focused, term.selection());
}

/// Background colour used to highlight selected terminal cells (SPECS §20).
const SELECTION_BG: Color = Color::Rgb(58, 90, 138);

/// Render a VT100 [`vt100::Screen`] into `area`, cell-by-cell. When `focused`,
/// the terminal cursor is positioned to match the screen's cursor. Cells inside
/// `selection` are drawn with the selection highlight.
fn render_screen(
    frame: &mut Frame,
    area: Rect,
    screen: &vt100::Screen,
    focused: bool,
    selection: Option<&Selection>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (rows, cols) = screen.size();
    let offset = screen.scrollback();
    {
        let buf = frame.buffer_mut();
        let max_r = area.height.min(rows);
        let max_c = area.width.min(cols);
        for r in 0..max_r {
            // Columns selected on this visible row, if any.
            let sel_cols = selection.and_then(|s| s.row_selection(r, rows, cols, offset));
            for c in 0..max_c {
                let Some(cell) = screen.cell(r, c) else {
                    continue;
                };
                let target = &mut buf[(area.x + c, area.y + r)];
                let contents = cell.contents();
                if contents.is_empty() {
                    target.set_symbol(" ");
                } else {
                    target.set_symbol(contents);
                }
                let mut style = Style::default()
                    .fg(vt_color(cell.fgcolor()))
                    .bg(vt_color(cell.bgcolor()));
                if cell.bold() {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if cell.italic() {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.inverse() {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                // Selection highlight overrides the cell background and drops any
                // inverse so the highlight reads consistently.
                if sel_cols.map(|(a, b)| c >= a && c <= b).unwrap_or(false) {
                    style = style
                        .bg(SELECTION_BG)
                        .fg(Color::White)
                        .remove_modifier(Modifier::REVERSED);
                }
                target.set_style(style);
            }
        }
    }
    if focused && !screen.hide_cursor() {
        let (cr, cc) = screen.cursor_position();
        if cr < area.height && cc < area.width {
            frame.set_cursor_position((area.x + cc, area.y + cr));
        }
    }
}

/// Convert a [`vt100::Color`] to a ratatui [`Color`].
fn vt_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

// ---------------------------------------------------------------------------
// Split view (terminals side by side)
// ---------------------------------------------------------------------------

/// Draw the selected tab's terminals (primary agent + child shells) side by side
/// in equal-width columns, each topped by its label, with a vertical separator
/// between columns. Replaces the horizontal tab bar + single viewport when split
/// view is enabled. Column geometry comes from [`layout::split_columns`] so it
/// matches the per-terminal PTY sizing the wiring layer applies.
pub fn draw_split_view(frame: &mut Frame, state: &AppState, region: Rect, now_ms: u64) {
    let Some(tab) = state.selected() else {
        let p = Paragraph::new(
            "\n  FlightDeck — no Agent Tab selected.\n  Press Ctrl-n to create one.",
        )
        .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, region);
        return;
    };

    // While the worktree is materializing there is no session yet (SPECS §16/§17).
    if tab.phase == TabPhase::Creating {
        let msg = Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{} ", spinner_frame(now_ms)),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                format!("Creating worktree for {}…", tab.meta.branch),
                Style::default().fg(Color::Cyan),
            ),
        ]))
        .alignment(Alignment::Center);
        let inner = Rect {
            y: region.y + region.height / 2,
            height: 1,
            ..region
        };
        frame.render_widget(msg, inner);
        return;
    }

    let entries = child_tab_entries(state);
    let cols = layout::split_columns(region, entries.len());
    let active = tab.session.selected_child(); // None = primary
    let focused = state.mode() == InputMode::Terminal;

    for (i, ((target, label), col)) in entries.iter().zip(cols.iter()).enumerate() {
        let is_active = match target {
            ChildTarget::Primary => active.is_none(),
            ChildTarget::Child(c) => active == Some(*c),
        };

        // Column header: the terminal label, highlighted when active (matching
        // the tab-bar colours: agent = yellow, shell = cyan).
        let header_style = if is_active {
            let bg = if matches!(target, ChildTarget::Primary) {
                Color::Yellow
            } else {
                Color::Cyan
            };
            Style::default()
                .fg(Color::Black)
                .bg(bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let header = Paragraph::new(Line::from(Span::styled(format!(" {label} "), header_style)))
            .alignment(Alignment::Center);
        frame.render_widget(header, col.header);

        // Column body: the terminal's VT100 screen. Only the active column shows
        // the cursor, and only while a terminal is focused.
        let term = match target {
            ChildTarget::Primary => tab.session.primary(),
            ChildTarget::Child(c) => tab.session.child(*c),
        };
        match term {
            Some(term) => render_screen(
                frame,
                col.viewport,
                term.screen(),
                focused && is_active,
                term.selection(),
            ),
            None => {
                let p = Paragraph::new("  (starting…)").style(Style::default().fg(Color::DarkGray));
                frame.render_widget(p, col.viewport);
            }
        }

        // Vertical separator in the gutter to the right of every column but the
        // last, spanning the full region height.
        if i + 1 < cols.len() {
            let sep_x = col.col.right();
            let sep = Paragraph::new(vec![Line::from("│"); region.height as usize])
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(
                sep,
                Rect {
                    x: sep_x,
                    y: region.y,
                    width: 1,
                    height: region.height,
                },
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Git info bar (SPECS §21)
// ---------------------------------------------------------------------------

/// Draw the one-line git info bar for the selected tab (SPECS §21).
///
/// Shown for whichever child terminal is active (agent or shell) — it reflects
/// the tab's worktree, not the focused process. Content: branch name, the
/// add/modify/delete file counts, ahead/behind vs upstream, base drift, and the
/// base branch. Data comes from the [`GitStatusCache`]; a missing entry renders
/// as `git: ?` and never panics.
pub fn draw_info_bar(frame: &mut Frame, state: &AppState, cache: &GitStatusCache, area: Rect) {
    let line = info_bar_line(state, cache);
    let para = Paragraph::new(line).style(Style::default().bg(Color::Reset));
    frame.render_widget(para, area);
}

/// A dim ` │ ` segment separator for the info bar.
fn info_sep() -> Span<'static> {
    Span::styled(" │ ", Style::default().fg(Color::DarkGray))
}

/// Build the git info bar [`Line`] for the selected tab. Exported for testing.
pub fn info_bar_line(state: &AppState, cache: &GitStatusCache) -> Line<'static> {
    let Some(tab) = state.selected() else {
        return Line::from(Span::styled(
            " No Agent Tab selected",
            Style::default().fg(Color::DarkGray),
        ));
    };
    let git = cache.get(&tab.meta.id);

    let mut spans: Vec<Span> = Vec::new();

    // Branch (prefer the freshly-collected name; fall back to stored meta).
    let branch = git
        .map(|w| w.branch.clone())
        .unwrap_or_else(|| tab.meta.branch.clone());
    spans.push(Span::styled(" ⎇ ", Style::default().fg(Color::Blue)));
    spans.push(Span::styled(
        branch,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));

    match git {
        None => {
            spans.push(info_sep());
            spans.push(Span::styled("git: ?", Style::default().fg(Color::DarkGray)));
        }
        Some(ws) => {
            // Change counts: +added ~modified -deleted (N files), or "clean".
            spans.push(info_sep());
            let ch = ws.changes;
            if ch.is_empty() {
                spans.push(Span::styled("clean", Style::default().fg(Color::Green)));
            } else {
                spans.push(Span::styled(
                    format!("+{}", ch.added),
                    Style::default().fg(Color::Green),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("~{}", ch.modified),
                    Style::default().fg(Color::Yellow),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("-{}", ch.deleted),
                    Style::default().fg(Color::Red),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("({} files)", ch.total()),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            // Ahead/behind vs upstream.
            spans.push(info_sep());
            if ws.upstream.is_some() {
                spans.push(Span::styled(
                    format!("↑{} ↓{}", ws.ahead, ws.behind),
                    Style::default().fg(Color::Cyan),
                ));
            } else {
                spans.push(Span::styled(
                    "no upstream",
                    Style::default().fg(Color::DarkGray),
                ));
            }

            // Base drift (only when the base has moved).
            if ws.base_drift > 0 {
                spans.push(info_sep());
                spans.push(Span::styled(
                    format!("base +{}", ws.base_drift),
                    Style::default().fg(Color::Magenta),
                ));
            }

            // Base branch for context.
            spans.push(info_sep());
            spans.push(Span::styled(
                format!("base: {}", ws.base_branch),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    Line::from(spans)
}

// ---------------------------------------------------------------------------
// Status bar (SPECS §23)
// ---------------------------------------------------------------------------

/// Draw the mode status bar (SPECS §23).
///
/// Terminal mode: `MODE: TERMINAL | Alt+Esc: app commands | Ctrl-g: command palette`
/// App mode:      `MODE: APP | Enter: focus terminal | Ctrl-g: command palette | ?: help`
pub fn draw_status_bar(frame: &mut Frame, state: &AppState, area: Rect) {
    let text = status_bar_text(state.mode(), state.update_available.as_deref());
    let para = Paragraph::new(text).style(Style::default().bg(Color::Reset));
    frame.render_widget(para, area);
}

/// The key that leaves terminal focus, per platform. `Alt+Esc` everywhere
/// except Windows, where the OS reserves `Alt+Esc` (cycles windows) so the
/// terminal app never receives it — Windows uses `Shift+Esc` instead.
#[cfg(windows)]
pub const LEAVE_FOCUS_KEY: &str = "Shift+Esc";
#[cfg(not(windows))]
pub const LEAVE_FOCUS_KEY: &str = "Alt+Esc";

/// Build the status bar [`Line`] for the given mode (SPECS §23), with an
/// optional trailing update hint when a newer release is available (SPECS §30).
///
/// Exported for snapshot testing.
pub fn status_bar_text(mode: InputMode, update_available: Option<&str>) -> Line<'static> {
    let mut spans = match mode {
        InputMode::Terminal => vec![
            Span::raw(" "),
            Span::styled(
                "MODE: TERMINAL",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | "),
            Span::styled(LEAVE_FOCUS_KEY, Style::default().fg(Color::Yellow)),
            Span::raw(": app commands | "),
            Span::styled("Ctrl-g", Style::default().fg(Color::Yellow)),
            Span::raw(": command palette"),
        ],
        InputMode::App => vec![
            Span::raw(" "),
            Span::styled(
                "MODE: APP",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | "),
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw(": focus terminal | "),
            Span::styled("Ctrl-g", Style::default().fg(Color::Yellow)),
            Span::raw(": command palette | "),
            Span::styled("?", Style::default().fg(Color::Yellow)),
            Span::raw(": help"),
        ],
    };

    // Update notice (SPECS §30): a non-intrusive hint, never a modal. It points
    // at `flightdeck update`, which itself routes Homebrew installs to `brew
    // upgrade`, so a single message is correct for every install method.
    if let Some(version) = update_available {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("● v{version} available — run `flightdeck update`"),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    Line::from(spans)
}

// ---------------------------------------------------------------------------
// Git status panel (SPECS §21)
// ---------------------------------------------------------------------------

/// Draw the git status panel as a centered overlay (SPECS §21).
///
/// Shows: branch, base branch, drift, dirty/clean, ahead/behind vs upstream,
/// whether upstream exists, worktree path, and optionally a PR compare URL.
/// No file diff view (SPECS §21 "No file diff view in MVP").
pub fn draw_git_status_overlay(
    frame: &mut Frame,
    status: &WorktreeStatus,
    pr_url: Option<&str>,
    area: Rect,
) {
    let overlay_area = layout::centered_overlay(area, 70, 18);
    frame.render_widget(Clear, overlay_area);

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("Branch:     ", Style::default().fg(Color::Gray)),
        Span::styled(status.branch.clone(), Style::default().fg(Color::Yellow)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Base branch:", Style::default().fg(Color::Gray)),
        Span::raw(" "),
        Span::styled(
            status.base_branch.clone(),
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Base drift: ", Style::default().fg(Color::Gray)),
        Span::styled(
            if status.base_drift == 0 {
                "none".to_string()
            } else {
                format!("{} commits ahead since creation", status.base_drift)
            },
            if status.base_drift == 0 {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Magenta)
            },
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Dirty:      ", Style::default().fg(Color::Gray)),
        Span::styled(
            if status.dirty { "yes" } else { "clean" },
            if status.dirty {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Green)
            },
        ),
    ]));

    let upstream_label = status.upstream.as_deref().unwrap_or("none (not pushed)");
    lines.push(Line::from(vec![
        Span::styled("Upstream:   ", Style::default().fg(Color::Gray)),
        Span::styled(
            upstream_label.to_string(),
            if status.upstream.is_some() {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            },
        ),
    ]));

    if status.upstream.is_some() {
        lines.push(Line::from(vec![
            Span::styled("Ahead/behind:", Style::default().fg(Color::Gray)),
            Span::raw(" "),
            Span::styled(
                format!("↑{} ↓{}", status.ahead, status.behind),
                Style::default().fg(Color::Cyan),
            ),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("Worktree:   ", Style::default().fg(Color::Gray)),
        Span::styled(
            status.worktree_path.to_string_lossy().to_string(),
            Style::default().fg(Color::White),
        ),
    ]));

    if let Some(url) = pr_url {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("PR URL:     ", Style::default().fg(Color::Gray)),
            Span::styled(url.to_string(), Style::default().fg(Color::Green)),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Esc / q to close",
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .title(" Git Status ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, overlay_area);
}

// ---------------------------------------------------------------------------
// Command palette overlay (SPECS §22)
// ---------------------------------------------------------------------------

/// Draw the command palette as a centered overlay (SPECS §22).
pub fn draw_palette_overlay(frame: &mut Frame, palette: &CommandPalette, area: Rect) {
    let overlay_area = layout::centered_overlay(area, 60, 32);
    frame.render_widget(Clear, overlay_area);

    let block = Block::default()
        .title(" Command Palette  (Esc to close) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(overlay_area);
    frame.render_widget(block, overlay_area);

    // Split inner: one row for filter input, rest for filtered list.
    let [filter_area, list_area] = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Length(1),
        ratatui::layout::Constraint::Fill(1),
    ])
    .areas(inner);

    // Filter input line.
    let filter_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Cyan)),
        Span::raw(palette.filter().to_string()),
        Span::styled("_", Style::default().fg(Color::Cyan)), // cursor
    ]);
    frame.render_widget(Paragraph::new(filter_line), filter_area);

    // Filtered list.
    let filtered = palette.filtered();
    let selected_idx = palette.selected_index();
    let mut last_group: Option<&str> = None;
    let mut items: Vec<ListItem> = Vec::new();
    for (i, entry) in filtered.iter().enumerate() {
        if last_group != Some(entry.group) {
            // Blank line above each group header (except the first) for breathing room.
            if last_group.is_some() {
                items.push(ListItem::new(Line::raw("")));
            }
            last_group = Some(entry.group);
            items.push(ListItem::new(Line::from(Span::styled(
                format!("  {}", entry.group),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ))));
        }

        items.push(if i == selected_idx {
            ListItem::new(Line::from(Span::styled(
                format!("  {} ", entry.label),
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )))
        } else {
            ListItem::new(Line::from(Span::styled(
                format!("  {} ", entry.label),
                Style::default().fg(Color::White),
            )))
        });
    }

    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("  (no matches)").style(Style::default().fg(Color::DarkGray)),
            list_area,
        );
    } else {
        frame.render_widget(List::new(items), list_area);
    }
}

// ---------------------------------------------------------------------------
// Help overlay (SPECS §23)
// ---------------------------------------------------------------------------

/// Draw the help / keybindings overlay (SPECS §23).
pub fn draw_help_overlay(frame: &mut Frame, area: Rect) {
    let overlay_area = layout::centered_overlay(area, 64, 40);
    frame.render_widget(Clear, overlay_area);

    let help_text = vec![
        Line::from(Span::styled(
            "FlightDeck Keyboard Shortcuts",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(Span::styled("Global", Style::default().fg(Color::Yellow))),
        shortcut_line("  Ctrl-g", "Command palette"),
        shortcut_line("  Ctrl-q", "Quit / close app"),
        shortcut_line("  Ctrl-n", "New Agent Tab"),
        shortcut_line("  Ctrl-p", "Push current branch"),
        shortcut_line("  Ctrl-u", "Pull base (git pull --rebase)"),
        shortcut_line("  Ctrl-f", "Finish current Agent Tab"),
        shortcut_line("  Ctrl-k", "Close current Agent Tab"),
        shortcut_line("  ?", "Help / keybindings"),
        Line::raw(""),
        Line::from(Span::styled(
            "Agent Tab Navigation",
            Style::default().fg(Color::Yellow),
        )),
        shortcut_line("  Up / Down (or Alt)", "Previous / Next Agent Tab"),
        shortcut_line("  Alt-1 .. Alt-9", "Jump to Agent Tab by index"),
        shortcut_line("  Mouse click", "Select Agent Tab"),
        Line::raw(""),
        Line::from(Span::styled(
            "Child Terminal Navigation",
            Style::default().fg(Color::Yellow),
        )),
        shortcut_line("  Ctrl-t", "New child terminal"),
        shortcut_line("  Ctrl-w", "Close active child terminal"),
        shortcut_line(
            "  Left / Right (or Alt)",
            "Cycle terminal tabs (agent + shells)",
        ),
        shortcut_line("  Ctrl-b", "Toggle split view (terminals side by side)"),
        shortcut_line("  Mouse click", "Select terminal tab"),
        Line::raw(""),
        Line::from(Span::styled(
            "Selection / Clipboard",
            Style::default().fg(Color::Yellow),
        )),
        shortcut_line("  Drag", "Select terminal text (copies on release)"),
        shortcut_line("  Drag past edge", "Auto-scrolls to reach offscreen text"),
        shortcut_line("  Shift-drag", "Force selection over a mouse-driven app"),
        Line::raw(""),
        Line::from(Span::styled("Focus", Style::default().fg(Color::Yellow))),
        #[cfg(windows)]
        shortcut_line("  Shift+Esc", "Leave terminal focus / focus app"),
        #[cfg(not(windows))]
        shortcut_line("  Alt+Esc", "Leave terminal focus / focus app"),
        shortcut_line("  Enter", "Focus active terminal"),
        Line::raw(""),
        Line::from(Span::styled("Status", Style::default().fg(Color::Yellow))),
        shortcut_line("  Ctrl-s", "Set manual status"),
        shortcut_line("  Ctrl-r", "Restart primary agent"),
        Line::raw(""),
        Line::from(Span::styled(
            "  Esc / q to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let block = Block::default()
        .title(" Help / Keybindings ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let para = Paragraph::new(help_text).block(block);
    frame.render_widget(para, overlay_area);
}

/// Build a shortcut description line for the help overlay.
fn shortcut_line(keys: &'static str, desc: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(keys, Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(desc, Style::default().fg(Color::Gray)),
    ])
}

// ---------------------------------------------------------------------------
// Toast / message overlay
// ---------------------------------------------------------------------------

/// Draw a one-line message toast at the bottom of the screen.
pub fn draw_message_toast(frame: &mut Frame, msg: &str, area: Rect) {
    if area.height == 0 {
        return;
    }
    let toast_area = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
    let para = Paragraph::new(Line::from(vec![
        Span::styled(" ", Style::default().bg(Color::DarkGray)),
        Span::styled(
            msg.to_string(),
            Style::default().bg(Color::DarkGray).fg(Color::White),
        ),
        Span::styled(" ", Style::default().bg(Color::DarkGray)),
    ]));
    frame.render_widget(para, toast_area);
}

// ---------------------------------------------------------------------------
// Tests (SPECS §26)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    use crate::app::modes::InputMode;
    use crate::contracts::Config;
    use crate::persistence::project_state::default_state;

    fn test_terminal(width: u16, height: u16) -> Terminal<TestBackend> {
        let backend = TestBackend::new(width, height);
        Terminal::new(backend).unwrap()
    }

    fn empty_state() -> AppState {
        AppState::new(
            Config::default(),
            default_state("main"),
            "/repo",
            "/repo/state.json",
        )
    }

    fn empty_cache() -> GitStatusCache {
        GitStatusCache::new()
    }

    fn state_with_tabs(n: usize) -> AppState {
        let mut ps = default_state("main");
        for i in 0..n {
            ps.tabs.push(crate::contracts::TabState {
                id: format!("t{i}"),
                name: format!("tab{i}"),
                slug: format!("tab{i}"),
                agent: "opencode".to_string(),
                branch: format!("flightdeck/tab{i}"),
                worktree_path_relative: format!(".flightdeck/worktrees/tab{i}"),
                base_branch: "main".to_string(),
                base_commit_sha: "sha".to_string(),
                created_at: "t".to_string(),
                attached_existing_branch: false,
                recovered: false,
                last_known_status: "unknown".to_string(),
                manual_status: None,
                containerized: false,
                container_image: None,
            });
        }
        AppState::new(Config::default(), ps, "/repo", "/repo/state.json")
    }

    // --- Mouse hit-testing (clickable tabs) ------------------------------

    #[test]
    fn hit_test_maps_sidebar_rows_to_agent_tabs() {
        let state = state_with_tabs(2);
        let area = Rect::new(0, 0, 80, 24);
        // Rows 0-1 are the logo header + divider; row 2 is the sidebar's "Agent
        // Tabs" heading. Tab 0 occupies rows 3..=6, tab 1 rows 7..=10.
        assert_eq!(hit_test(area, &state, 2, 3), Some(HitTarget::AgentTab(0)));
        assert_eq!(hit_test(area, &state, 2, 6), Some(HitTarget::AgentTab(0)));
        assert_eq!(hit_test(area, &state, 2, 7), Some(HitTarget::AgentTab(1)));
        // The header band sits above the sidebar and selects nothing.
        assert_eq!(hit_test(area, &state, 2, 0), None);
        // The sidebar heading (and any non-tab sidebar row) resolves to the
        // sidebar chrome, so the click still focuses the app (SPECS §23).
        assert_eq!(hit_test(area, &state, 2, 2), Some(HitTarget::Sidebar));
    }

    #[test]
    fn hit_test_empty_sidebar_resolves_to_chrome() {
        // With no agents, a click anywhere in the sidebar (heading or the empty
        // space below it) still resolves to the sidebar chrome so APP mode is
        // reachable by clicking the left panel (SPECS §23).
        let state = state_with_tabs(0);
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(hit_test(area, &state, 2, 2), Some(HitTarget::Sidebar));
        assert_eq!(hit_test(area, &state, 2, 5), Some(HitTarget::Sidebar));
    }

    #[test]
    fn terminal_viewport_renders_parsed_pty_output() {
        // Regression: the active terminal's PTY output must actually render
        // (previously a placeholder was shown). Spawn a primary, feed it bytes,
        // and assert the text lands in the viewport region of the buffer.
        use crate::contracts::PtySize;
        use crate::testing::FakePty;
        use std::path::Path;

        let pty = FakePty::new();
        pty.queue_session();
        let mut state = state_with_tabs(1);
        state.tabs[0]
            .session
            .spawn_primary(&pty, "agent", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        state.tabs[0]
            .session
            .primary_mut()
            .unwrap()
            .process_output(b"HELLO_FLIGHTDECK");

        let mut term = test_terminal(80, 24);
        term.draw(|frame| draw(frame, &state, &empty_cache(), &UiOverlay::None, 0))
            .unwrap();

        let buffer = term.backend().buffer().clone();
        let all_text: String = (0..24_u16)
            .flat_map(|y| (0..80_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();
        assert!(
            all_text.contains("HELLO_FLIGHTDECK"),
            "terminal viewport must render parsed PTY output"
        );
    }

    #[test]
    fn hit_test_maps_child_tab_bar_to_primary() {
        let state = state_with_tabs(1);
        let area = Rect::new(0, 0, 80, 24);
        // Child tab bar is the first body row (row 2, below the logo + divider);
        // the "agent" segment starts at the sidebar width (28), spanning " agent ".
        assert_eq!(
            hit_test(area, &state, 30, 2),
            Some(HitTarget::Child(ChildTarget::Primary))
        );
    }

    #[test]
    fn split_view_renders_both_terminals_side_by_side() {
        // In split view the primary agent and a child shell render at the same
        // time, each in its own column.
        use crate::contracts::PtySize;
        use crate::testing::FakePty;
        use std::path::Path;

        let pty = FakePty::new();
        pty.queue_session();
        pty.queue_session();
        let mut state = state_with_tabs(1);
        state.split_view = true;
        let session = &mut state.tabs[0].session;
        session
            .spawn_primary(&pty, "agent", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        session
            .spawn_child(&pty, "zsh", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        session.primary_mut().unwrap().process_output(b"AGENT_PANE");
        session.child_mut(0).unwrap().process_output(b"SHELL_PANE");

        let mut term = test_terminal(120, 30);
        term.draw(|frame| draw(frame, &state, &empty_cache(), &UiOverlay::None, 0))
            .unwrap();

        let buffer = term.backend().buffer().clone();
        let all_text: String = (0..30_u16)
            .flat_map(|y| (0..120_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();
        assert!(all_text.contains("AGENT_PANE"), "agent column must render");
        assert!(all_text.contains("SHELL_PANE"), "shell column must render");
    }

    #[test]
    fn hit_test_in_split_view_selects_column() {
        use crate::contracts::PtySize;
        use crate::testing::FakePty;
        use std::path::Path;

        let pty = FakePty::new();
        pty.queue_session();
        pty.queue_session();
        let mut state = state_with_tabs(1);
        state.split_view = true;
        let session = &mut state.tabs[0].session;
        session
            .spawn_primary(&pty, "agent", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        session
            .spawn_child(&pty, "zsh", &[], Path::new("/wt"), PtySize::default())
            .unwrap();

        let area = Rect::new(0, 0, 120, 30);
        // Two columns over the main pane (x ≥ sidebar width 28). A click on a
        // column's header row switches to that terminal: the left header lands
        // on the agent (primary) column, the right header on the shell column.
        let region = layout::split_region(&layout::compute(area));
        let cols = layout::split_columns(region, 2);
        let left = cols[0].col.x + cols[0].col.width / 2;
        let right = cols[1].col.x + cols[1].col.width / 2;
        let header_row = cols[0].header.y;
        assert_eq!(
            hit_test(area, &state, left, header_row),
            Some(HitTarget::Child(ChildTarget::Primary))
        );
        assert_eq!(
            hit_test(area, &state, right, header_row),
            Some(HitTarget::Child(ChildTarget::Child(0)))
        );
        // A click in a column *body* is not a switch target — it begins a text
        // selection instead (handled by the mouse wiring).
        let body_row = cols[0].viewport.y + 1;
        assert_eq!(hit_test(area, &state, left, body_row), None);
        assert_eq!(hit_test(area, &state, right, body_row), None);
    }

    // --- Git info bar (SPECS §21) ----------------------------------------

    fn flatten(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    // --- Branded header (logo) -------------------------------------------

    #[test]
    fn header_uses_wide_logo_when_space_allows() {
        let flat = flatten(&header_line(200));
        assert!(flat.contains("F L I G H T D E C K"), "wide brand: {flat:?}");
        assert!(flat.contains("██████"), "block flourish: {flat:?}");
    }

    #[test]
    fn header_fills_the_full_window_width() {
        // The title bar must span the entire width edge to edge, with blocks
        // running out to both ends and the ░▒▓ ramps framing the wordmark.
        for width in [50usize, 80, 120, 201] {
            let line = header_line(width);
            let flat = flatten(&line);
            assert_eq!(
                flat.chars().count(),
                width,
                "title bar must be exactly {width} cols: {flat:?}"
            );
            assert!(flat.starts_with('█'), "fills to the left edge: {flat:?}");
            assert!(flat.ends_with('█'), "fills to the right edge: {flat:?}");
            assert!(flat.contains("▓▓▓▒▒▒░░░"), "left ramp present: {flat:?}");
        }
    }

    #[test]
    fn header_shrinks_to_narrow_logo_when_tight() {
        // 40 cols fits the narrow logo (brand + ramps) but not the wide one.
        let flat = flatten(&header_line(40));
        assert!(
            flat.contains("F·L·I·G·H·T·D·E·C·K"),
            "narrow brand: {flat:?}"
        );
        assert!(
            !flat.contains("F L I G H T"),
            "must not be the wide brand: {flat:?}"
        );
        assert!(flat.contains("▓▓▓▒▒▒░░░"), "block ramp: {flat:?}");
    }

    #[test]
    fn header_falls_back_to_truncated_brand_when_very_narrow() {
        let flat = flatten(&header_line(8));
        assert_eq!(flat, "FLIGHTDE", "8-col fallback: {flat:?}");
    }

    #[test]
    fn header_and_divider_render_on_top_two_rows() {
        let state = state_with_tabs(1);
        let mut term = test_terminal(120, 24);
        term.draw(|frame| draw(frame, &state, &empty_cache(), &UiOverlay::None, 0))
            .unwrap();
        let buffer = term.backend().buffer().clone();
        let row0: String = (0..120)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect();
        let row1: String = (0..120)
            .map(|x| buffer[(x, 1)].symbol().to_string())
            .collect();
        // The logo (block flourish + brand) sits on the very first row.
        assert!(row0.contains("██████"), "logo row: {row0:?}");
        assert!(row0.contains("F L I G H T"), "brand on logo row: {row0:?}");
        // The divider fills the second row.
        assert!(
            row1.chars().filter(|&c| c == '─').count() > 100,
            "divider row should be a full-width rule: {row1:?}"
        );
    }

    #[test]
    fn info_bar_without_selection_says_no_tab() {
        let state = empty_state();
        let flat = flatten(&info_bar_line(&state, &empty_cache()));
        assert!(flat.contains("No Agent Tab selected"), "got: {flat:?}");
    }

    #[test]
    fn info_bar_without_cache_shows_branch_and_unknown_git() {
        let state = state_with_tabs(1);
        let flat = flatten(&info_bar_line(&state, &empty_cache()));
        assert!(flat.contains("flightdeck/tab0"), "branch missing: {flat:?}");
        assert!(flat.contains("git: ?"), "unknown marker missing: {flat:?}");
    }

    #[test]
    fn info_bar_shows_branch_and_change_counts() {
        let state = state_with_tabs(1);
        let mut cache = empty_cache();
        cache.insert(
            "t0".to_string(),
            WorktreeStatus {
                branch: "flightdeck/tab0".to_string(),
                base_branch: "main".to_string(),
                dirty: true,
                changes: crate::git::status::WorktreeChanges {
                    added: 1,
                    modified: 2,
                    deleted: 3,
                },
                ahead: 4,
                behind: 5,
                upstream: Some("origin/flightdeck/tab0".to_string()),
                base_drift: 6,
                worktree_path: PathBuf::from("/repo/.flightdeck/worktrees/tab0"),
            },
        );
        let flat = flatten(&info_bar_line(&state, &cache));
        assert!(flat.contains("flightdeck/tab0"), "branch: {flat:?}");
        assert!(flat.contains("+1"), "added: {flat:?}");
        assert!(flat.contains("~2"), "modified: {flat:?}");
        assert!(flat.contains("-3"), "deleted: {flat:?}");
        assert!(flat.contains("(6 files)"), "total: {flat:?}");
        assert!(flat.contains("↑4 ↓5"), "ahead/behind: {flat:?}");
        assert!(flat.contains("base +6"), "drift: {flat:?}");
        assert!(flat.contains("base: main"), "base branch: {flat:?}");
    }

    #[test]
    fn info_bar_clean_worktree_says_clean() {
        let state = state_with_tabs(1);
        let mut cache = empty_cache();
        cache.insert(
            "t0".to_string(),
            WorktreeStatus {
                branch: "flightdeck/tab0".to_string(),
                base_branch: "main".to_string(),
                dirty: false,
                changes: crate::git::status::WorktreeChanges::default(),
                ahead: 0,
                behind: 0,
                upstream: None,
                base_drift: 0,
                worktree_path: PathBuf::from("/repo/.flightdeck/worktrees/tab0"),
            },
        );
        let flat = flatten(&info_bar_line(&state, &cache));
        assert!(flat.contains("clean"), "clean marker: {flat:?}");
        assert!(flat.contains("no upstream"), "upstream marker: {flat:?}");
    }

    #[test]
    fn info_bar_renders_above_status_bar_in_buffer() {
        // The info bar occupies the row just above the bottom status bar.
        let state = state_with_tabs(1);
        let mut cache = empty_cache();
        cache.insert(
            "t0".to_string(),
            WorktreeStatus {
                branch: "flightdeck/tab0".to_string(),
                base_branch: "main".to_string(),
                dirty: true,
                changes: crate::git::status::WorktreeChanges {
                    added: 2,
                    modified: 0,
                    deleted: 0,
                },
                ahead: 0,
                behind: 0,
                upstream: None,
                base_drift: 0,
                worktree_path: PathBuf::from("/repo/.flightdeck/worktrees/tab0"),
            },
        );
        let mut term = test_terminal(80, 24);
        term.draw(|frame| draw(frame, &state, &cache, &UiOverlay::None, 0))
            .unwrap();
        let buffer = term.backend().buffer().clone();
        // Layout bottom rows: info_bar (y = 21), status_divider (y = 22),
        // status_bar (y = 23).
        let info_row: String = (0..80)
            .map(|x| buffer[(x, 21)].symbol().to_string())
            .collect();
        assert!(
            info_row.contains("flightdeck/tab0"),
            "info bar row should show the branch, got: {info_row:?}"
        );
        // The divider row sits directly above the status bar.
        let divider_row: String = (0..80)
            .map(|x| buffer[(x, 22)].symbol().to_string())
            .collect();
        assert!(
            divider_row.contains('─'),
            "divider row should be drawn above status bar, got: {divider_row:?}"
        );
    }

    // --- Status bar text (SPECS §23) -------------------------------------

    #[test]
    fn status_bar_terminal_mode_text() {
        let line = status_bar_text(InputMode::Terminal, None);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("MODE: TERMINAL"), "must show mode name");
        assert!(flat.contains("Esc"), "must mention Esc");
        assert!(flat.contains("app commands"), "must say app commands");
        assert!(flat.contains("Ctrl-g"), "must mention Ctrl-g");
        assert!(flat.contains("command palette"), "must mention palette");
    }

    #[test]
    fn status_bar_app_mode_text() {
        let line = status_bar_text(InputMode::App, None);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("MODE: APP"), "must show mode name");
        assert!(flat.contains("Enter"), "must mention Enter");
        assert!(flat.contains("focus terminal"), "must say focus terminal");
        assert!(flat.contains("Ctrl-g"), "must mention Ctrl-g");
        assert!(flat.contains("command palette"), "must mention palette");
        assert!(flat.contains('?'), "must mention '?'");
        assert!(flat.contains("help"), "must mention help");
    }

    #[test]
    fn status_bar_shows_update_hint_when_available() {
        let line = status_bar_text(InputMode::App, Some("1.0.3"));
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            flat.contains("v1.0.3 available"),
            "must show the new version"
        );
        assert!(
            flat.contains("flightdeck update"),
            "must point at the update command"
        );
        // Absent the notice, the bar is unchanged.
        let none = status_bar_text(InputMode::App, None);
        let none_flat: String = none.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!none_flat.contains("available"), "no hint when up to date");
    }

    // --- Render smoke tests (TestBackend) ---------------------------------

    #[test]
    fn draw_does_not_panic_with_no_tabs() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::None, 0);
        })
        .unwrap();
    }

    #[test]
    fn draw_does_not_panic_with_message_overlay() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        term.draw(|frame| {
            draw(
                frame,
                &state,
                &cache,
                &UiOverlay::Message("Test message".to_string()),
                0,
            );
        })
        .unwrap();
    }

    #[test]
    fn draw_does_not_panic_with_help_overlay() {
        let mut term = test_terminal(80, 30);
        let state = empty_state();
        let cache = empty_cache();
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::Help, 0);
        })
        .unwrap();
    }

    #[test]
    fn draw_does_not_panic_with_palette_overlay() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        let palette = CommandPalette::new();
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::Palette(palette), 0);
        })
        .unwrap();
    }

    #[test]
    fn draw_does_not_panic_with_git_status_overlay() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        let ws = WorktreeStatus {
            branch: "flightdeck/test".to_string(),
            base_branch: "main".to_string(),
            dirty: true,
            changes: crate::git::status::WorktreeChanges {
                added: 1,
                modified: 2,
                deleted: 0,
            },
            ahead: 3,
            behind: 1,
            upstream: Some("origin/flightdeck/test".to_string()),
            base_drift: 2,
            worktree_path: PathBuf::from("/repo/.flightdeck/worktrees/test"),
        };
        term.draw(|frame| {
            draw(
                frame,
                &state,
                &cache,
                &UiOverlay::GitStatus {
                    status: ws,
                    pr_url: Some("https://github.com/owner/repo/compare/main...test".to_string()),
                },
                0,
            );
        })
        .unwrap();
    }

    #[test]
    fn status_bar_appears_at_bottom_of_buffer() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::None, 0);
        })
        .unwrap();

        // Bottom row (y=23) should contain status bar text.
        let buffer = term.backend().buffer().clone();
        let bottom_row: String = (0..80)
            .map(|x| buffer[(x, 23)].symbol().to_string())
            .collect();

        // Status bar must be on the bottom row.
        assert!(
            bottom_row.contains("MODE:") || bottom_row.contains("APP"),
            "bottom row should contain status bar, got: {bottom_row:?}"
        );
    }

    #[test]
    fn git_status_overlay_shows_branch() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        let ws = WorktreeStatus {
            branch: "flightdeck/mybranch".to_string(),
            base_branch: "main".to_string(),
            dirty: false,
            changes: crate::git::status::WorktreeChanges::default(),
            ahead: 0,
            behind: 0,
            upstream: None,
            base_drift: 0,
            worktree_path: PathBuf::from("/repo/.flightdeck/worktrees/mybranch"),
        };
        term.draw(|frame| {
            draw(
                frame,
                &state,
                &cache,
                &UiOverlay::GitStatus {
                    status: ws,
                    pr_url: None,
                },
                0,
            );
        })
        .unwrap();

        // The buffer should contain the branch name somewhere.
        let buffer = term.backend().buffer().clone();
        let all_text: String = (0..24_u16)
            .flat_map(|y| (0..80_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();

        assert!(
            all_text.contains("flightdeck/mybranch"),
            "git status overlay must show branch name, got: ...truncated..."
        );
    }

    #[test]
    fn sidebar_shows_no_tabs_hint() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::None, 0);
        })
        .unwrap();

        let buffer = term.backend().buffer().clone();
        let all_text: String = (0..24_u16)
            .flat_map(|y| (0..28_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();

        assert!(
            all_text.contains("No tabs"),
            "sidebar should show 'No tabs' hint when empty, got: {all_text:?}"
        );
    }

    // --- §24: simplified sidebar status (idle / in progress / error) ------

    #[test]
    fn status_label_color_collapses_to_three_buckets() {
        use crate::contracts::InterpretedStatus::*;
        use ratatui::style::Color;

        // In progress (cyan).
        for s in [Starting, Running, Working] {
            assert_eq!(status_label_color(s), ("in progress", Color::Cyan));
        }
        // Error (red).
        for s in [Failed, SessionLost] {
            assert_eq!(status_label_color(s), ("error", Color::Red));
        }
        // Everything else reads as idle (green).
        for s in [
            Idle,
            WaitingForInput,
            NeedsAttention,
            Completed,
            Stopped,
            Recovered,
            Unknown,
        ] {
            assert_eq!(status_label_color(s), ("idle", Color::Green));
        }
    }

    #[test]
    fn sidebar_shows_bracketed_status_without_proc_prefix() {
        let state = state_with_tabs(1);
        let mut term = test_terminal(80, 24);
        term.draw(|f| draw(f, &state, &empty_cache(), &UiOverlay::None, 0))
            .unwrap();

        let buffer = term.backend().buffer().clone();
        let all_text: String = (0..24_u16)
            .flat_map(|y| (0..80_u16).map(move |x| (x, y)))
            .map(|(x, y)| buffer[(x, y)].symbol().to_string())
            .collect();

        // New format: "<agent> [<status>]"; a fresh (not-started) tab reads idle.
        assert!(
            all_text.contains("[idle]"),
            "sidebar should show bracketed status, got: {all_text:?}"
        );
        // The "proc:" prefix is gone.
        assert!(
            !all_text.contains("proc:"),
            "sidebar must not show the 'proc:' prefix, got: {all_text:?}"
        );
    }
}
