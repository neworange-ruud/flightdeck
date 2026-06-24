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

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::modes::InputMode;
use crate::app::state::AppState;
use crate::git::status::WorktreeStatus;
use crate::tui::layout;
use crate::tui::palette::CommandPalette;

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
// Top-level entry point
// ---------------------------------------------------------------------------

/// Draw the complete FlightDeck UI into `frame`.
///
/// Called once per tick by T9 inside `Terminal::draw(|frame| draw(frame, ...))`.
pub fn draw(frame: &mut Frame, state: &AppState, cache: &GitStatusCache, overlay: &UiOverlay) {
    let area = frame.area();
    let ml = layout::compute(area);

    draw_sidebar(frame, state, cache, ml.sidebar);
    draw_child_tab_bar(frame, state, ml.child_tabs);
    draw_terminal_viewport(frame, state, ml.terminal);
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
// Sidebar (SPECS §20, §24)
// ---------------------------------------------------------------------------

/// Draw the left Agent Tabs sidebar.
pub fn draw_sidebar(frame: &mut Frame, state: &AppState, cache: &GitStatusCache, area: Rect) {
    let block = Block::default()
        .title(" Agent Tabs ")
        .borders(Borders::RIGHT);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.tabs.is_empty() {
        let hint = Paragraph::new("No tabs. Ctrl-n to create.")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(hint, inner);
        return;
    }

    let items: Vec<ListItem> = state
        .tabs
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            let selected = state.selected_tab == Some(i);
            let ds = tab.display_status();
            let git = cache.get(&tab.meta.id);

            // Line 1: tab name (with selection marker).
            let name_style = if selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let marker = if selected { "▸ " } else { "  " };
            let name_line = Line::from(vec![
                Span::styled(marker, name_style),
                Span::styled(tab.meta.name.clone(), name_style),
            ]);

            // Line 2: agent | process: <state> | status: <interp>
            let agent_name = state
                .registry
                .get(&tab.meta.agent)
                .map(|a| a.display_name.as_str())
                .unwrap_or(&tab.meta.agent);

            let (status_str, status_color) = if let Some(manual) = ds.manual {
                (
                    format!("{} | proc: {}", manual.as_str(), ds.process.as_str()),
                    Color::Cyan,
                )
            } else {
                let color = interpreted_color(ds.interpreted);
                (
                    format!(
                        "proc: {} | {}",
                        ds.process.as_str(),
                        ds.interpreted.as_str()
                    ),
                    color,
                )
            };

            let agent_line = Line::from(vec![
                Span::raw("  "),
                Span::styled(agent_name, Style::default().fg(Color::Gray)),
                Span::raw(" "),
                Span::styled(status_str, Style::default().fg(status_color)),
            ]);

            // Line 3: git indicators (dirty, ahead/behind, base drift, markers).
            let git_line = build_git_indicator_line(tab, git);

            let mut lines = vec![name_line, agent_line, git_line];

            // Blank line separator between tabs.
            lines.push(Line::raw(""));

            ListItem::new(lines)
        })
        .collect();

    let list = List::new(items);
    frame.render_widget(list, inner);
}

/// Colour for an interpreted status label (SPECS §24).
fn interpreted_color(status: crate::contracts::InterpretedStatus) -> Color {
    use crate::contracts::InterpretedStatus::*;
    match status {
        Starting => Color::Blue,
        Running => Color::Green,
        WaitingForInput => Color::Yellow,
        NeedsAttention => Color::Magenta,
        Completed => Color::Cyan,
        Failed => Color::Red,
        Stopped => Color::DarkGray,
        SessionLost | Recovered => Color::Magenta,
        Unknown => Color::DarkGray,
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

    // Build "agent | shell 1 | shell 2 …" style tab bar.
    let mut spans: Vec<Span> = Vec::new();

    let primary_selected = tab.session.selected_child().is_none();
    let primary_style = if primary_selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    spans.push(Span::styled(" agent ", primary_style));

    for i in 0..tab.session.child_count() {
        let child_selected = tab.session.selected_child() == Some(i);
        let style = if child_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
        spans.push(Span::styled(format!(" shell {} ", i + 1), style));
    }

    let line = Line::from(spans);
    let para = Paragraph::new(line).style(Style::default().bg(Color::Reset));
    frame.render_widget(para, area);
}

// ---------------------------------------------------------------------------
// Terminal viewport (SPECS §20)
// ---------------------------------------------------------------------------

/// Draw the active terminal viewport (SPECS §20).
///
/// The actual byte-level PTY rendering (scrollback buffer, ANSI escape
/// processing) is handled by T9's PTY integration. This function renders a
/// placeholder/scrollback string. T9 should replace this with a proper
/// terminal widget or render the scrollback bytes directly into the buffer.
pub fn draw_terminal_viewport(frame: &mut Frame, state: &AppState, area: Rect) {
    let block = Block::default().borders(Borders::NONE);

    let content = if state.selected().is_none() {
        Paragraph::new("\n  FlightDeck — no Agent Tab selected.\n  Press Ctrl-n to create one.")
            .style(Style::default().fg(Color::DarkGray))
            .block(block)
    } else {
        // T9 should render the actual terminal content here. For now we show a
        // placeholder that T9 replaces with a real terminal widget.
        Paragraph::new(
            "  [terminal output — rendered by T9 wiring layer]\n\
             \n\
             \x1b[2m  (Replace this with the PTY scrollback render)\x1b[0m",
        )
        .style(Style::default().fg(Color::Gray))
        .block(block)
    };

    frame.render_widget(content, area);
}

// ---------------------------------------------------------------------------
// Status bar (SPECS §23)
// ---------------------------------------------------------------------------

/// Draw the mode status bar (SPECS §23).
///
/// Terminal mode: `MODE: TERMINAL | Esc: app commands | Ctrl-g: command palette`
/// App mode:      `MODE: APP | Enter: focus terminal | Ctrl-g: command palette | ?: help`
pub fn draw_status_bar(frame: &mut Frame, state: &AppState, area: Rect) {
    let text = status_bar_text(state.mode());
    let para = Paragraph::new(text).style(Style::default().bg(Color::Reset));
    frame.render_widget(para, area);
}

/// Build the status bar [`Line`] for the given mode (SPECS §23).
///
/// Exported for snapshot testing.
pub fn status_bar_text(mode: InputMode) -> Line<'static> {
    match mode {
        InputMode::Terminal => Line::from(vec![
            Span::styled(
                "MODE: TERMINAL",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | "),
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw(": app commands | "),
            Span::styled("Ctrl-g", Style::default().fg(Color::Yellow)),
            Span::raw(": command palette"),
        ]),
        InputMode::App => Line::from(vec![
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
        ]),
    }
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
    let overlay_area = layout::centered_overlay(area, 60, 20);
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
    let items: Vec<ListItem> = filtered
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            if i == selected_idx {
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
            }
        })
        .collect();

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
    let overlay_area = layout::centered_overlay(area, 64, 26);
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
        shortcut_line("  Ctrl-f", "Finish current Agent Tab"),
        shortcut_line("  Ctrl-k", "Close current Agent Tab"),
        shortcut_line("  ?", "Help / keybindings"),
        Line::raw(""),
        Line::from(Span::styled(
            "Agent Tab Navigation",
            Style::default().fg(Color::Yellow),
        )),
        shortcut_line("  Alt-Left / Alt-Right", "Previous / Next Agent Tab"),
        shortcut_line("  Alt-1 .. Alt-9", "Jump to Agent Tab by index"),
        Line::raw(""),
        Line::from(Span::styled(
            "Child Terminal Navigation",
            Style::default().fg(Color::Yellow),
        )),
        shortcut_line("  Ctrl-t", "New child terminal"),
        shortcut_line("  Ctrl-w", "Close active child terminal"),
        shortcut_line("  Ctrl-Tab", "Next child terminal"),
        shortcut_line("  Ctrl-Shift-Tab", "Previous child terminal"),
        Line::raw(""),
        Line::from(Span::styled("Focus", Style::default().fg(Color::Yellow))),
        shortcut_line("  Esc", "Leave terminal focus / focus app"),
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

    // --- Status bar text (SPECS §23) -------------------------------------

    #[test]
    fn status_bar_terminal_mode_text() {
        let line = status_bar_text(InputMode::Terminal);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("MODE: TERMINAL"), "must show mode name");
        assert!(flat.contains("Esc"), "must mention Esc");
        assert!(flat.contains("app commands"), "must say app commands");
        assert!(flat.contains("Ctrl-g"), "must mention Ctrl-g");
        assert!(flat.contains("command palette"), "must mention palette");
    }

    #[test]
    fn status_bar_app_mode_text() {
        let line = status_bar_text(InputMode::App);
        let flat: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(flat.contains("MODE: APP"), "must show mode name");
        assert!(flat.contains("Enter"), "must mention Enter");
        assert!(flat.contains("focus terminal"), "must say focus terminal");
        assert!(flat.contains("Ctrl-g"), "must mention Ctrl-g");
        assert!(flat.contains("command palette"), "must mention palette");
        assert!(flat.contains('?'), "must mention '?'");
        assert!(flat.contains("help"), "must mention help");
    }

    // --- Render smoke tests (TestBackend) ---------------------------------

    #[test]
    fn draw_does_not_panic_with_no_tabs() {
        let mut term = test_terminal(80, 24);
        let state = empty_state();
        let cache = empty_cache();
        term.draw(|frame| {
            draw(frame, &state, &cache, &UiOverlay::None);
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
            draw(frame, &state, &cache, &UiOverlay::Help);
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
            draw(frame, &state, &cache, &UiOverlay::Palette(palette));
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
            draw(frame, &state, &cache, &UiOverlay::None);
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
            draw(frame, &state, &cache, &UiOverlay::None);
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
}
