//! Mission control (designs A2 and A3): every live session across all open
//! projects at once.
//!
//! ```text
//! +--------------+-----------------------------------------------+
//! | NEEDS YOU    | scope ▾  project ▾   2 need you · 2 working   |
//! |  row …       +-----------------------+-----------------------+
//! | WORKING      | tile: header          | tile                  |
//! |  row …       |  live terminal        |                       |
//! | EARLIER      +-----------------------+-----------------------+
//! |  row …       | EARLIER TODAY  card · card · card             |
//! | n quiet ·    |                                               |
//! |  Projects    |                                               |
//! +--------------+-----------------------------------------------+
//! ```
//!
//! What is shown, and in what order, is [`flightdeck::view::MissionView`]
//! (pure, tested in the core); this module draws it and turns input into the
//! host's own events.
//!
//! ## Grid or focus: the host's input mode decides
//!
//! There is no layout flag. In APP mode Mission control is the grid (A2); in
//! TERMINAL mode it is the focus view (A3): the selected session full size
//! with the others in a strip above it. So Enter — the table's FocusTerminal,
//! unchanged — opens the selected tile with its terminal focused, and Alt-Esc
//! (FocusApp) goes back to the grid, with nothing to keep in sync. Moving
//! between sessions keeps whichever mode is on (see [`select_session`]),
//! because each project remembers its own.
//!
//! ## Selection is the host's
//!
//! The selected tile is the host's active project and selected Agent Tab,
//! nothing more. Selecting a tile dispatches `SwitchProject(Index)` and
//! `SwitchAgentTab(Index)` — the events the project tabs and Alt-N send — so
//! every existing chord (Ctrl-p, Ctrl-f, Ctrl-t, …) acts on the selected tile
//! with no special case. The only keys Mission control takes over are the
//! ones that mean *moving* ([`MissionControl::intercept`]): the arrows and
//! Alt-1..9 follow tile order here, and Enter with no tiles does nothing.

pub mod tile;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::time::Duration;

use flightdeck::app::commands::{Command, Selector};
use flightdeck::app::keymap::{Action, KeymapEntry};
use flightdeck::app::modes::InputMode;
use flightdeck::host::{AppHost, HostEvent};
use flightdeck::persistence::workspace::{MainView, MissionScope, RecentScope};
use flightdeck::view::{
    format_elapsed, grid_columns, grid_visible_rows, move_selection, AgentBadge, AgentRowView,
    CardAction, GridMove, MissionCard, MissionTile, MissionView, SessionKey,
};
use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, AnyElement, App, AppContext, BoxShadow, Context, Entity, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, StyleRefinement, Styled, Task, Window,
};
use gpui_component::button::Button;
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex, Sizable};

use crate::commands::{keycap, keymap, perform_entry, perform_id};
use crate::fonts::MONO_FAMILY;
use crate::host::HostModel;
use crate::terminal::view::TerminalView;
use crate::theme::{Hex, Palette};
use crate::views::icons;
use crate::views::sidebar::{monogram, monogram_ink, SIDEBAR_WIDTH};
use tile::TileView;

/// How often an unselected tile may redraw its terminal: ≤10 fps.
pub const TILE_REFRESH: Duration = Duration::from_millis(100);

/// Every this many tile refreshes (2 s), the other projects' git status is
/// refreshed too.
const GIT_REFRESH_EVERY: u32 = 20;

/// The gap between tiles and around the grid (the mockup's 10px).
const GAP: Pixels = px(10.);
/// A tile's title row.
const TILE_HEADER: Pixels = px(36.);
/// The focus view's strip of other sessions.
const STRIP_HEIGHT: Pixels = px(68.);

/// The session the host has selected: its active project's selected tab.
pub fn host_selection(host: &AppHost<'_>) -> Option<SessionKey> {
    let tab = host.active_state().selected()?;
    Some(SessionKey {
        project: host.active_project_index(),
        tab_id: tab.meta.id.clone(),
    })
}

/// Make `key` the host's selection (project, then Agent Tab), and leave the
/// host in `mode`: APP keeps the grid, TERMINAL keeps (or opens) the focus
/// view. Every step is the event a control elsewhere already sends.
pub fn select_session(
    host: &Entity<HostModel>,
    key: &SessionKey,
    tab_index: usize,
    mode: InputMode,
    cx: &mut App,
) {
    host.update(cx, |model, cx| {
        if model.host().active_project_index() != key.project {
            model.dispatch(HostEvent::SwitchProject(Selector::Index(key.project)), cx);
        }
        // Always, even when it is already selected: it is also what marks
        // the session read, as viewing it in the Projects view does.
        model.dispatch(
            HostEvent::Command(Command::SwitchAgentTab(Selector::Index(tab_index))),
            cx,
        );
        let focused = model.host().terminal_focused();
        match mode {
            InputMode::Terminal if !focused => model.dispatch(HostEvent::FocusTerminal, cx),
            InputMode::App if focused => model.dispatch(HostEvent::FocusApp, cx),
            _ => {}
        }
    });
}

/// Show `to`. Entering Mission control lands on the grid (APP mode) with a
/// tile selected when there is one, so the chords have a target from the
/// first key; leaving it keeps the host's selection, which is why "Show in
/// Projects" opens that very session there.
pub fn switch_view(host: &Entity<HostModel>, to: MainView, cx: &mut App) {
    let from = host.read(cx).host().workspace_ui().view;
    if from == to {
        return;
    }
    host.update(cx, |model, cx| {
        let mut ui = model.host().workspace_ui().clone();
        ui.view = to;
        model.host_mut().set_workspace_ui(ui);
        cx.notify();
    });
    if to == MainView::Mission {
        let (view, current) = {
            let host = host.read(cx).host();
            (host.mission_view(), host_selection(host))
        };
        let on_a_tile = current.is_some_and(|k| view.tile(&k).is_some());
        match view.tiles.first() {
            Some(first) if !on_a_tile => {
                select_session(host, &first.key, first.tab_index, InputMode::App, cx)
            }
            _ => {
                if host.read(cx).host().terminal_focused() {
                    host.update(cx, |model, cx| model.dispatch(HostEvent::FocusApp, cx));
                }
            }
        }
    }
}

/// Alt-m: the other view.
pub fn toggle_view(host: &Entity<HostModel>, cx: &mut App) {
    let to = host.read(cx).host().workspace_ui().view.toggled();
    switch_view(host, to, cx);
}

/// Replace Mission control's scope (persisted with the workspace).
pub fn set_scope(host: &Entity<HostModel>, scope: MissionScope, cx: &mut App) {
    host.update(cx, |model, cx| {
        let mut ui = model.host().workspace_ui().clone();
        ui.mission_scope = scope;
        model.host_mut().set_workspace_ui(ui);
        cx.notify();
    });
}

/// Perform a card's action for its session: select it, then run the keymap
/// entry whose command the action is — the chord's own event.
pub fn perform_card_action(
    host: &Entity<HostModel>,
    card: &MissionCard,
    action: CardAction,
    cx: &mut App,
) {
    select_session(host, &card.key, card.tab_index, InputMode::App, cx);
    if let Some(entry) = keymap().entry_for_action(&Action::Dispatch(action.command())) {
        perform_entry(entry, host, cx);
    }
}

/// The root of Mission control: owns the tiles' cached views and their
/// refresh timer. See the module docs.
pub struct MissionControl {
    host: Entity<HostModel>,
    /// The window's terminal view: the focus view's full-size terminal is the
    /// same view the Projects view shows (only one is ever on screen).
    terminal: Entity<TerminalView>,
    /// The window's APP-mode focus: carried by this view's sidebar, as by the
    /// Projects sidebar, so APP chords fire here.
    app_focus: FocusHandle,
    tiles: HashMap<SessionKey, Entity<TileView>>,
    /// The host produced something since the tiles last redrew.
    dirty: bool,
    /// Refresh ticks so far, for [`GIT_REFRESH_EVERY`].
    refreshes: u32,
    /// The grid's scroll position, once there are more tiles than fit.
    grid_scroll: ScrollHandle,
    /// The selection the grid last scrolled into view.
    scrolled_to: Option<SessionKey>,
    _refresh: Task<()>,
}

impl MissionControl {
    pub fn new(
        host: Entity<HostModel>,
        terminal: Entity<TerminalView>,
        app_focus: FocusHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&host, |this, host, cx| {
            let model = host.read(cx);
            if model.host().workspace_ui().view != MainView::Mission {
                return;
            }
            this.dirty = true;
            // The selected tile keeps up with its output; the rest wait for
            // the next refresh.
            if let Some(tile) = host_selection(model.host()).and_then(|k| this.tiles.get(&k)) {
                tile.update(cx, |_, cx| cx.notify());
            }
        })
        .detach();
        let refresh = cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(TILE_REFRESH).await;
            if this.update(cx, |view, cx| view.refresh_tiles(cx)).is_err() {
                break;
            }
        });
        Self {
            host,
            terminal,
            app_focus,
            tiles: HashMap::new(),
            dirty: false,
            refreshes: 0,
            grid_scroll: ScrollHandle::new(),
            scrolled_to: None,
            _refresh: refresh,
        }
    }

    fn refresh_tiles(&mut self, cx: &mut Context<Self>) {
        // Every other project's git status, now and then: the host keeps only
        // the active one fresh, and here every project's diff is on screen.
        self.refreshes = self.refreshes.wrapping_add(1);
        if self.refreshes.is_multiple_of(GIT_REFRESH_EVERY) {
            self.host.update(cx, |model, _| {
                let (shown, active, count) = {
                    let host = model.host();
                    (
                        host.workspace_ui().view == MainView::Mission,
                        host.active_project_index(),
                        host.project_count(),
                    )
                };
                if shown {
                    for index in (0..count).filter(|i| *i != active) {
                        model.host_mut().refresh_git_status(index);
                    }
                }
            });
        }
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        for tile in self.tiles.values() {
            tile.update(cx, |_, cx| cx.notify());
        }
    }

    /// Take over a chord that means *moving* in Mission control; `false`
    /// leaves it to [`perform_entry`], which is every other chord.
    ///
    /// - Grid (APP): Up/Down (bare or Alt) move a row, Left/Right a tile,
    ///   Alt-N selects tile N; Enter with no tile selected selects the first
    ///   and opens it, and with no tiles at all does nothing.
    /// - Focus (TERMINAL): Alt-N and Alt-Up/Down swap the open session for
    ///   another tile, staying in the focus view. (Alt-Left/Right still cycle
    ///   the open session's own terminals.)
    pub fn intercept(host: &Entity<HostModel>, entry: &KeymapEntry, cx: &mut App) -> bool {
        let (view, current, mode) = {
            let h = host.read(cx).host();
            (
                h.mission_view(),
                host_selection(h).and_then(|k| h.mission_view().tile(&k).map(|(i, _)| i)),
                h.active_state().mode(),
            )
        };
        let count = view.tiles.len();
        let columns = grid_columns(count);
        let go = |index: usize, mode: InputMode, cx: &mut App| {
            if let Some(tile) = view.tiles.get(index) {
                select_session(host, &tile.key, tile.tab_index, mode, cx);
            }
        };
        let step = |mv: GridMove| match current {
            Some(i) => move_selection(i, count, columns, mv),
            None => 0,
        };
        match (mode, &entry.action) {
            (InputMode::App, Action::Dispatch(Command::SwitchAgentTab(sel))) => {
                let target = match sel {
                    Selector::Prev => step(GridMove::Up),
                    Selector::Next => step(GridMove::Down),
                    Selector::Index(i) => *i,
                };
                go(target, InputMode::App, cx);
                true
            }
            (InputMode::App, Action::Dispatch(Command::SwitchChildTerminal(sel))) => {
                let target = match sel {
                    Selector::Prev => step(GridMove::Left),
                    Selector::Next => step(GridMove::Right),
                    Selector::Index(_) => return false,
                };
                go(target, InputMode::App, cx);
                true
            }
            // On a tile, Enter is just FocusTerminal: the chord's own event.
            (InputMode::App, Action::FocusTerminal) => match current {
                Some(_) => false,
                None => {
                    go(0, InputMode::Terminal, cx);
                    true
                }
            },
            (InputMode::Terminal, Action::Dispatch(Command::SwitchAgentTab(sel))) => {
                let target = match (sel, current) {
                    (Selector::Index(i), _) => *i,
                    (Selector::Prev, Some(i)) => i.saturating_sub(1),
                    (Selector::Next, Some(i)) => (i + 1).min(count.saturating_sub(1)),
                    (_, None) => 0,
                };
                go(target, InputMode::Terminal, cx);
                true
            }
            _ => false,
        }
    }

    /// The tile view for `key`, created on first sight.
    fn tile_view(&mut self, key: &SessionKey, cx: &mut Context<Self>) -> Entity<TileView> {
        if let Some(tile) = self.tiles.get(key) {
            return tile.clone();
        }
        let host = self.host.clone();
        let for_key = key.clone();
        let tile = cx.new(|cx| TileView::new(host, for_key, cx));
        self.tiles.insert(key.clone(), tile.clone());
        tile
    }
}

/// What one frame of Mission control is drawn from, read from the host once.
struct Frame {
    view: MissionView,
    selected: Option<SessionKey>,
    mode: InputMode,
    has_terminal: bool,
    scope: MissionScope,
    /// `(name, root)` of every open project, in workspace order.
    projects: Vec<(String, String)>,
}

impl Frame {
    fn read(host: &AppHost<'_>) -> Frame {
        Frame {
            view: host.mission_view(),
            selected: host_selection(host),
            mode: host.active_state().mode(),
            has_terminal: host.active_terminal().is_some(),
            scope: host.workspace_ui().mission_scope.clone(),
            projects: (0..host.project_count())
                .filter_map(|i| {
                    Some((
                        host.project_name(i)?.to_string(),
                        host.project_root(i)?.to_string_lossy().to_string(),
                    ))
                })
                .collect(),
        }
    }

    fn is_selected(&self, key: &SessionKey) -> bool {
        self.selected.as_ref() == Some(key)
    }
}

impl Render for MissionControl {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = *Palette::global(cx);
        let frame = Frame::read(self.host.read(cx).host());
        // Tiles that left the grid lose their cached view.
        self.tiles
            .retain(|key, _| frame.view.tiles.iter().any(|t| &t.key == key));

        let sidebar = mission_sidebar(&frame, &self.host, &p)
            .key_context(flightdeck::app::keymap::Context::App.name())
            .track_focus(&self.app_focus)
            .on_key_down({
                let host = self.host.clone();
                move |event, _, cx| {
                    // The TUI's leniency for App-mode chords GPUI did not bind
                    // exactly, through the same interception as a bound chord.
                    if let Some(entry) = flightdeck_desktop::keys::app_key_down(keymap(), event) {
                        if !MissionControl::intercept(&host, entry, cx) {
                            perform_entry(entry, &host, cx);
                        }
                        cx.stop_propagation();
                    }
                }
            });

        let main = if frame.mode == InputMode::Terminal {
            self.focus_main(&frame, &p, cx)
        } else {
            self.grid_main(&frame, &p, window, cx)
        };

        h_flex()
            .flex_1()
            .min_h_0()
            .items_start()
            .child(sidebar)
            .child(main)
    }
}

impl MissionControl {
    /// A2's main area: scope bar, tile grid, earlier cards.
    fn grid_main(
        &mut self,
        frame: &Frame,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = &frame.view;
        let count = view.tiles.len();
        let columns = grid_columns(count);
        let tiles: Vec<AnyElement> = view
            .tiles
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let grid = self.tile_view(&t.key, cx);
                tile(i, t, frame.is_selected(&t.key), grid, &self.host, p)
            })
            .collect();

        let cards_height = if view.cards.is_empty() {
            px(0.)
        } else {
            px(128.)
        };
        let grid: AnyElement = if count == 0 {
            empty_grid(view, p).into_any_element()
        } else if count <= 6 {
            v_flex()
                .flex_1()
                .min_h_0()
                .gap(GAP)
                .children(grid_rows(tiles, columns, None))
                .into_any_element()
        } else {
            // More than fit: two rows' height each, and scroll.
            let chrome = crate::views::titlebar::TITLEBAR_HEIGHT
                + crate::views::status_bar::STATUS_BAR_HEIGHT
                + SCOPE_BAR_HEIGHT
                + GAP * 4.;
            let avail = window.viewport_size().height - chrome - cards_height;
            let rows = grid_visible_rows(count) as f32;
            let row_h = ((avail - GAP * (rows - 1.)) / rows).max(px(160.));
            // Keep a newly selected tile on screen (arrows, Alt-N, a click in
            // the rail); leave the user's own scrolling alone otherwise.
            let selected_row = frame
                .selected
                .as_ref()
                .and_then(|k| view.tile(k))
                .map(|(i, _)| i / columns);
            if frame.selected != self.scrolled_to {
                if let Some(row) = selected_row {
                    self.grid_scroll.scroll_to_item(row);
                }
                self.scrolled_to = frame.selected.clone();
            }
            div()
                .id("mission-grid")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .gap(GAP)
                .overflow_y_scroll()
                .track_scroll(&self.grid_scroll)
                .children(grid_rows(tiles, columns, Some(row_h)))
                .into_any_element()
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .p(GAP)
            .gap(GAP)
            .bg(p.surface_terminal.hsla())
            .child(scope_bar(frame, &self.host, p))
            .child(grid)
            .when(!view.cards.is_empty(), |d| {
                d.child(cards(view, &frame.scope, &self.host, p))
            })
            .into_any_element()
    }

    /// A3's main area: the strip, the git strip, the full-size terminal.
    fn focus_main(&mut self, frame: &Frame, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let view = &frame.view;
        let (strip_view, project_name) = {
            let host = self.host.read(cx).host();
            (
                host.git_strip(),
                host.project_name(host.active_project_index())
                    .unwrap_or_default()
                    .to_string(),
            )
        };
        let previews: Vec<String> = {
            let host = self.host.read(cx).host();
            view.tiles.iter().map(|t| last_line(host, &t.key)).collect()
        };
        let back = {
            let host = self.host.clone();
            v_flex()
                .id("mission-back-to-grid")
                .debug_selector(|| "mission-back-to-grid".into())
                .flex_none()
                .w(px(96.))
                .h_full()
                .items_center()
                .justify_center()
                .gap(px(3.))
                .rounded(px(8.))
                .border_1()
                .border_color(p.border.hsla())
                .text_size(px(12.))
                .text_color(p.ink_2.hsla())
                .cursor_pointer()
                .child(icons::icon(crate::assets::icon::MISSION, px(13.), p.ink_2))
                .child("Grid")
                .on_click(move |_, _, cx| perform_id("FocusApp", &host, cx))
        };
        let entries = view
            .tiles
            .iter()
            .zip(previews)
            .enumerate()
            .map(|(i, (t, preview))| {
                strip_entry(i, t, frame.is_selected(&t.key), preview, &self.host, p)
            });
        let strip = h_flex()
            .flex_shrink_0()
            .h(STRIP_HEIGHT)
            .p(GAP)
            .gap(px(8.))
            .bg(p.surface_window.hsla())
            .border_b_1()
            .border_color(p.hairline.hsla())
            .child(back)
            .children(entries);

        let show_in_projects = {
            let host = self.host.clone();
            h_flex()
                .id("mission-show-in-projects")
                .debug_selector(|| "mission-show-in-projects".into())
                .flex_none()
                .h(px(28.))
                .px(px(10.))
                .gap(px(7.))
                .rounded(px(6.))
                .border_1()
                .border_color(p.border.hsla())
                .text_size(px(12.))
                .text_color(p.ink_2.hsla())
                .cursor_pointer()
                .child("Show in Projects")
                .child(icons::keycap(
                    keycap("ToggleMissionControl").unwrap_or_default(),
                    p.faint,
                ))
                .on_click(move |_, _, cx| switch_view(&host, MainView::Projects, cx))
        };
        let header = h_flex()
            .flex_shrink_0()
            .bg(p.surface_window.hsla())
            .child(
                div()
                    .flex_none()
                    .h(crate::views::git_strip::GIT_STRIP_HEIGHT)
                    .flex()
                    .items_center()
                    .pl(px(18.))
                    .border_b_1()
                    .border_color(p.hairline.hsla())
                    .text_size(px(12.5))
                    .text_color(p.muted.hsla())
                    .child(format!("{project_name} /")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(crate::views::git_strip::git_strip(
                        &strip_view,
                        &self.host,
                        p,
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .h(crate::views::git_strip::GIT_STRIP_HEIGHT)
                    .flex()
                    .items_center()
                    .pr_3()
                    .border_b_1()
                    .border_color(p.hairline.hsla())
                    .child(show_in_projects),
            );

        let body: AnyElement = if frame.has_terminal {
            div()
                .flex_1()
                .min_h_0()
                .child(self.terminal.clone())
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(p.muted.hsla())
                .child("This session has no running terminal.")
                .into_any_element()
        };

        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(p.surface_terminal.hsla())
            .child(strip)
            .child(header)
            .child(body)
            .into_any_element()
    }
}

/// The scope bar's height.
const SCOPE_BAR_HEIGHT: Pixels = px(28.);

/// The last line of words a session's terminal shows, for the strip: the
/// lowest row with a letter or digit on it, so a dialog's bottom border
/// (`╰────╯`) or a bare prompt is skipped for the question above it.
fn last_line(host: &AppHost<'_>, key: &SessionKey) -> String {
    let Some(terminal) = host.tab_terminal(key.project, &key.tab_id) else {
        return String::new();
    };
    let screen = terminal.screen();
    let (rows, cols) = screen.size();
    (0..rows)
        .rev()
        .map(|r| screen.row_text(r, 0, cols))
        .find(|line| line.chars().any(char::is_alphanumeric))
        .map(|line| {
            // Drop a dialog's side borders around the words.
            line.trim_matches(|c: char| c.is_whitespace() || ('\u{2500}'..='\u{257f}').contains(&c))
                .to_string()
        })
        .unwrap_or_default()
}

/// Split `tiles` into rows of `columns`, padding the last row with empty
/// cells so every tile keeps its column's width.
fn grid_rows(tiles: Vec<AnyElement>, columns: usize, height: Option<Pixels>) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    let mut iter = tiles.into_iter().peekable();
    while iter.peek().is_some() {
        let mut row = h_flex().gap(GAP).w_full().items_stretch();
        row = match height {
            Some(h) => row.h(h).flex_none(),
            None => row.flex_1().min_h_0(),
        };
        let mut n = 0;
        for tile in iter.by_ref().take(columns) {
            row = row.child(tile);
            n += 1;
        }
        for _ in n..columns {
            row = row.child(div().flex_1().min_w_0());
        }
        rows.push(row.into_any_element());
    }
    rows
}

/// The words for a session's status and its age: `working · 2m`.
fn status_words(row: &AgentRowView) -> String {
    let word = if row.manual_status.is_some() {
        row.status_text.as_str()
    } else {
        match row.badge {
            AgentBadge::Working => "working",
            AgentBadge::WaitingAttention => "waiting",
            AgentBadge::Idle => "idle",
            AgentBadge::Done => "done",
            AgentBadge::Error => "error",
        }
    };
    match row.status_since_secs {
        Some(secs) => format!("{word} {}", format_elapsed(secs)),
        None => word.to_string(),
    }
}

/// `+214 −38` in the diff colours — or `2 files` when the changes are files
/// with no line counts (new, untracked) — and nothing for a clean or unknown
/// tree.
fn diff(row: &AgentRowView, p: &Palette) -> Option<gpui::Div> {
    let changes = row.changes.filter(|c| !c.is_clean())?;
    if changes.lines_added == 0 && changes.lines_removed == 0 {
        return Some(
            div()
                .flex_none()
                .font_family(MONO_FAMILY)
                .text_size(px(11.))
                .text_color(p.muted.hsla())
                .child(match changes.files {
                    1 => "1 file".to_string(),
                    n => format!("{n} files"),
                }),
        );
    }
    Some(
        h_flex()
            .flex_none()
            .gap(px(6.))
            .font_family(MONO_FAMILY)
            .text_size(px(11.))
            .child(
                div()
                    .text_color(p.diff_added.hsla())
                    .child(format!("+{}", changes.lines_added)),
            )
            .child(
                div()
                    .text_color(p.diff_removed.hsla())
                    .child(format!("−{}", changes.lines_removed)),
            ),
    )
}

/// One tile: title row, live terminal, and (selected) the open hint.
fn tile(
    index: usize,
    t: &MissionTile,
    selected: bool,
    grid: Entity<TileView>,
    host: &Entity<HostModel>,
    p: &Palette,
) -> AnyElement {
    let row = &t.row;
    let border = match (selected, t.needs_you) {
        (true, true) => p.status_attention,
        (true, false) => p.accent,
        (false, true) => p.attention_border,
        (false, false) => p.border,
    };
    let header_bg = if t.needs_you {
        p.attention_surface
    } else {
        p.surface_sidebar
    };
    let header = h_flex()
        .flex_shrink_0()
        .h(TILE_HEADER)
        .px(px(12.))
        .gap(px(10.))
        .bg(header_bg.hsla())
        .border_b_1()
        .border_color(
            if t.needs_you {
                p.attention_border
            } else {
                p.hairline
            }
            .hsla(),
        )
        .text_size(px(12.5))
        .child(icons::agent_glyph(
            ("mission-tile-glyph", index),
            row.badge,
            px(11.),
            p,
        ))
        .child(
            div()
                .flex_none()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.ink.hsla())
                .child(row.name.clone()),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(p.muted.hsla())
                .child(format!("{} · {}", t.project_name, row.agent_name)),
        )
        .child(div().flex_1())
        .child(
            div()
                .flex_none()
                .text_size(px(11.))
                .text_color(icons::badge_colour(row.badge, p).hsla())
                .child(status_words(row)),
        )
        .children(diff(row, p));

    let footer = selected.then(|| {
        h_flex()
            .flex_shrink_0()
            .h(px(30.))
            .px(px(12.))
            .gap(px(8.))
            .border_t_1()
            .border_color(
                if t.needs_you {
                    p.attention_border
                } else {
                    p.hairline
                }
                .hsla(),
            )
            .bg(header_bg.hsla())
            .text_size(px(11.5))
            .text_color(p.muted.hsla())
            .when_some(t.alt_index, |f, n| {
                f.child(icons::keycap(
                    keycap(&format!("JumpToAgentTab{n}")).unwrap_or_default(),
                    p.faint,
                ))
            })
            .child(div().flex_1())
            .child(icons::keycap(
                keycap("FocusTerminal").unwrap_or_default(),
                p.ink_2,
            ))
            .child("open full size")
    });

    let mut shadow = p.status_attention.hsla();
    shadow.a = 0.14;
    let key = t.key.clone();
    let tab_index = t.tab_index;
    let host = host.clone();
    v_flex()
        .id(("mission-tile", index))
        .debug_selector(move || format!("mission-tile-{index}"))
        .flex_1()
        .min_w_0()
        .h_full()
        .rounded(px(10.))
        .border(px(if selected { 1.5 } else { 1. }))
        .border_color(border.hsla())
        .when(selected && t.needs_you, |d| {
            d.shadow(vec![BoxShadow {
                color: shadow,
                offset: gpui::point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(4.),
                inset: false,
            }])
        })
        .bg(p.surface_terminal.hsla())
        .overflow_hidden()
        .cursor_pointer()
        .child(header)
        .child(
            div()
                .flex_1()
                .min_h_0()
                .px(px(14.))
                .py(px(10.))
                .child(grid.cached(StyleRefinement::default().size_full())),
        )
        .children(footer)
        .on_click(move |event, _, cx| {
            // A click selects; a double click opens it, as Enter does.
            let mode = if event.click_count() >= 2 {
                InputMode::Terminal
            } else {
                InputMode::App
            };
            select_session(&host, &key, tab_index, mode, cx);
        })
        .into_any_element()
}

/// The grid with nothing live in scope.
fn empty_grid(view: &MissionView, p: &Palette) -> impl IntoElement {
    let hint = if view.cards.is_empty() {
        "Sessions that are working or waiting for you show up here."
    } else {
        "Sessions that are working or waiting for you show up here; the recent ones are below."
    };
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap_1()
        .child(
            div()
                .text_sm()
                .text_color(p.ink_2.hsla())
                .child("Nothing is running"),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(p.muted.hsla())
                .child(hint),
        )
}

/// The scope menu's label for `window`.
fn scope_label(window: RecentScope) -> &'static str {
    match window {
        RecentScope::Day => "Active + updated in the last 24h",
        RecentScope::Week => "Active + updated in the last 7 days",
        RecentScope::All => "Active + everything with activity",
    }
}

/// The cards section's title for `window`.
fn cards_title(window: RecentScope) -> &'static str {
    match window {
        RecentScope::Day => "EARLIER TODAY",
        RecentScope::Week => "EARLIER THIS WEEK",
        RecentScope::All => "EARLIER",
    }
}

/// A count chip (`2 need you`).
fn chip(text: String, ink: Hex, bg: Hex) -> gpui::Div {
    div()
        .flex_none()
        .h(px(24.))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(12.))
        .bg(bg.hsla())
        .text_color(ink.hsla())
        .text_size(px(12.))
        .child(text)
}

/// Scope menu, project filter and the counts (the mockup's titlebar row; it
/// sits above the grid so the titlebar stays the one shared with Projects).
fn scope_bar(frame: &Frame, host: &Entity<HostModel>, p: &Palette) -> impl IntoElement {
    let (view, scope, projects) = (&frame.view, &frame.scope, frame.projects.as_slice());
    let window_menu = {
        let host = host.clone();
        let scope = scope.clone();
        Button::new("mission-scope")
            .icon(gpui_component::Icon::empty().path(crate::assets::icon::CLOCK))
            .label(scope_label(scope.window))
            .outline()
            .small()
            .dropdown_caret(true)
            .text_size(px(12.5))
            .text_color(p.ink_2.hsla())
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for window in RecentScope::ALL {
                    let host = host.clone();
                    let mut next = scope.clone();
                    next.window = window;
                    menu = menu.item(
                        PopupMenuItem::new(scope_label(window))
                            .checked(window == scope.window)
                            .on_click(move |_, _, cx| set_scope(&host, next.clone(), cx)),
                    );
                }
                menu
            })
    };
    let project_label: SharedString = match view.project_filter.and_then(|i| projects.get(i)) {
        Some((name, _)) => name.clone().into(),
        None => match projects.len() {
            1 => "All projects".into(),
            n => format!("All {n} projects").into(),
        },
    };
    let project_menu = {
        let host = host.clone();
        let scope = scope.clone();
        let projects = projects.to_vec();
        let filter = view.project_filter;
        Button::new("mission-project")
            .label(project_label)
            .outline()
            .small()
            .dropdown_caret(true)
            .text_size(px(12.5))
            .text_color(p.ink_2.hsla())
            .dropdown_menu(move |menu, _, _| {
                let all = {
                    let host = host.clone();
                    let mut next = scope.clone();
                    next.project = None;
                    PopupMenuItem::new("All projects")
                        .checked(filter.is_none())
                        .on_click(move |_, _, cx| set_scope(&host, next.clone(), cx))
                };
                let mut menu = menu.item(all).separator();
                for (i, (name, root)) in projects.iter().enumerate() {
                    let host = host.clone();
                    let mut next = scope.clone();
                    next.project = Some(root.clone());
                    menu = menu.item(
                        PopupMenuItem::new(name.clone())
                            .checked(filter == Some(i))
                            .on_click(move |_, _, cx| set_scope(&host, next.clone(), cx)),
                    );
                }
                menu
            })
    };
    let needs = view.tiles.iter().filter(|t| t.needs_you).count();
    h_flex()
        .flex_shrink_0()
        .h(SCOPE_BAR_HEIGHT)
        .gap(px(10.))
        .child(window_menu)
        .child(project_menu)
        .when(needs > 0, |d| {
            d.child(chip(
                format!("{needs} need{} you", if needs == 1 { "s" } else { "" }),
                p.status_attention,
                p.status_attention_bg,
            ))
        })
        .when(view.working_count > 0, |d| {
            d.child(chip(
                format!("{} working", view.working_count),
                p.working_chip_ink,
                p.working_chip_bg,
            ))
        })
        .when(!view.cards.is_empty(), |d| {
            d.child(chip(
                format!(
                    "{} {}",
                    view.cards.len(),
                    cards_title(scope.window).to_lowercase()
                ),
                p.muted,
                p.chip_bg,
            ))
        })
}

/// "EARLIER TODAY": compact cards, three to a row.
fn cards(
    view: &MissionView,
    scope: &MissionScope,
    host: &Entity<HostModel>,
    p: &Palette,
) -> impl IntoElement {
    let cards: Vec<AnyElement> = view
        .cards
        .iter()
        .enumerate()
        .map(|(i, c)| card(i, c, host, p))
        .collect();
    let mut rows = Vec::new();
    let mut iter = cards.into_iter().peekable();
    while iter.peek().is_some() {
        let mut row = h_flex().gap(GAP).w_full();
        let mut n = 0;
        for card in iter.by_ref().take(3) {
            row = row.child(card);
            n += 1;
        }
        for _ in n..3 {
            row = row.child(div().flex_1().min_w_0());
        }
        rows.push(row);
    }
    v_flex()
        .flex_shrink_0()
        .gap(px(8.))
        .child(
            h_flex()
                .gap(px(10.))
                .px_1()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(p.muted.hsla())
                .child(cards_title(scope.window))
                .child(div().flex_1().h(px(1.)).bg(p.hairline.hsla())),
        )
        .child(
            div()
                .id("mission-cards")
                .max_h(px(212.))
                .overflow_y_scroll()
                .child(v_flex().gap(GAP).children(rows)),
        )
}

/// One card: name and age, the detail line, diff and the one action.
fn card(index: usize, c: &MissionCard, host: &Entity<HostModel>, p: &Palette) -> AnyElement {
    let row = &c.row;
    let ago = c
        .last_event
        .map(|e| match format_elapsed(e.secs_ago).as_str() {
            "now" => "now".to_string(),
            t => format!("{t} ago"),
        })
        .unwrap_or_default();
    let diff_or_clean: AnyElement = match diff(row, p) {
        Some(d) => d.into_any_element(),
        None => div()
            .text_color(p.muted.hsla())
            .child(if row.changes.is_some() { "clean" } else { "" })
            .into_any_element(),
    };
    let action = c.action.map(|action| {
        let host = host.clone();
        let card = c.clone();
        let cap = keymap()
            .entry_for_action(&Action::Dispatch(action.command()))
            .and_then(|e| keycap(e.id))
            .unwrap_or_default();
        h_flex()
            .id(("mission-card-action", index))
            .debug_selector(move || format!("mission-card-action-{index}"))
            .flex_none()
            .gap(px(6.))
            .px(px(8.))
            .h(px(22.))
            .rounded(px(5.))
            .border_1()
            .border_color(p.border.hsla())
            .text_color(p.ink_2.hsla())
            .cursor_pointer()
            .hover(|s| s.bg(p.surface_raised.hsla()))
            .child(action.label())
            .child(icons::keycap(cap, p.faint))
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                perform_card_action(&host, &card, action, cx);
            })
    });
    let key = c.key.clone();
    let tab_index = c.tab_index;
    let select_host = host.clone();
    v_flex()
        .id(("mission-card", index))
        .debug_selector(move || format!("mission-card-{index}"))
        .flex_1()
        .min_w_0()
        .gap(px(6.))
        .px(px(14.))
        .py(px(12.))
        .rounded(px(10.))
        .bg(p.surface_sidebar.hsla())
        .border_1()
        .border_color(p.hairline.hsla())
        .cursor_pointer()
        .child(
            h_flex()
                .gap(px(8.))
                .text_size(px(13.))
                .child(icons::agent_glyph(
                    ("mission-card-glyph", index),
                    row.badge,
                    px(12.),
                    p,
                ))
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.ink.hsla())
                        .child(row.name.clone()),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(p.muted.hsla())
                        .child(c.project_name.clone()),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(p.muted.hsla())
                        .child(ago),
                ),
        )
        .child(
            div()
                .truncate()
                .font_family(MONO_FAMILY)
                .text_size(px(11.5))
                .text_color(p.ink_2.hsla())
                .child(c.detail.clone()),
        )
        .child(
            h_flex()
                .gap(px(8.))
                .font_family(MONO_FAMILY)
                .text_size(px(11.))
                .child(diff_or_clean)
                .child(div().flex_1())
                .children(action),
        )
        .on_click(move |_, _, cx| select_session(&select_host, &key, tab_index, InputMode::App, cx))
        .into_any_element()
}

/// One entry of the focus view's strip.
fn strip_entry(
    index: usize,
    t: &MissionTile,
    open: bool,
    preview: String,
    host: &Entity<HostModel>,
    p: &Palette,
) -> AnyElement {
    let (bg, border) = match (open, t.needs_you) {
        (true, _) => (p.surface_raised_nested, p.separator),
        (false, true) => (p.attention_surface, p.attention_border),
        (false, false) => (p.surface_sidebar, p.hairline),
    };
    let tag: AnyElement = if open {
        div()
            .flex_none()
            .text_size(px(10.5))
            .text_color(p.ink_2.hsla())
            .child("open")
            .into_any_element()
    } else {
        match t.alt_index {
            Some(n) => icons::keycap(
                keycap(&format!("JumpToAgentTab{n}")).unwrap_or_default(),
                p.faint,
            )
            .into_any_element(),
            None => div().into_any_element(),
        }
    };
    let key = t.key.clone();
    let tab_index = t.tab_index;
    let host = host.clone();
    v_flex()
        .id(("mission-strip", index))
        .debug_selector(move || format!("mission-strip-{index}"))
        .flex_1()
        .min_w_0()
        .h_full()
        .justify_center()
        .gap(px(3.))
        .px(px(12.))
        .rounded(px(8.))
        .bg(bg.hsla())
        .border_1()
        .border_color(border.hsla())
        .cursor_pointer()
        .child(
            h_flex()
                .gap(px(8.))
                .text_size(px(12.5))
                .child(icons::agent_glyph(
                    ("mission-strip-glyph", index),
                    t.row.badge,
                    px(10.),
                    p,
                ))
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(p.ink.hsla())
                        .child(t.row.name.clone()),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(p.muted.hsla())
                        .child(t.project_name.clone()),
                )
                .child(div().flex_1())
                .child(tag),
        )
        .child(
            div()
                .truncate()
                .font_family(MONO_FAMILY)
                .text_size(px(11.))
                .text_color(
                    if t.needs_you {
                        p.status_attention
                    } else {
                        p.muted
                    }
                    .hsla(),
                )
                .child(preview),
        )
        .on_click(move |_, _, cx| select_session(&host, &key, tab_index, InputMode::Terminal, cx))
        .into_any_element()
}

/// The rail, regrouped by status across all projects (A2/A3's sidebar).
fn mission_sidebar(
    frame: &Frame,
    host: &Entity<HostModel>,
    p: &Palette,
) -> gpui::Stateful<gpui::Div> {
    let view = &frame.view;
    let scope = &frame.scope;
    let needs: Vec<(usize, &MissionTile)> = view
        .tiles
        .iter()
        .enumerate()
        .filter(|(_, t)| t.needs_you)
        .collect();
    let working: Vec<(usize, &MissionTile)> = view
        .tiles
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.needs_you)
        .collect();

    let section = |title: &'static str, count: usize, ink: Hex, first: bool| {
        h_flex()
            .justify_between()
            .px(px(10.))
            .pt(px(if first { 0. } else { 14. }))
            .pb(px(6.))
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(ink.hsla())
            .child(title)
            .child(
                div()
                    .font_family(MONO_FAMILY)
                    .font_weight(FontWeight::NORMAL)
                    .child(count.to_string()),
            )
    };

    let mut list = v_flex().gap(px(2.)).px_2().py(px(14.));
    let mut first = true;
    if !needs.is_empty() {
        list = list.child(section("NEEDS YOU", needs.len(), p.status_attention, first));
        first = false;
        list = list.children(
            needs
                .iter()
                .map(|(i, t)| session_row(*i, t, frame.is_selected(&t.key), host, p)),
        );
    }
    if !working.is_empty() {
        list = list.child(section("WORKING", working.len(), p.working_chip_ink, first));
        first = false;
        list = list.children(
            working
                .iter()
                .map(|(i, t)| session_row(*i, t, frame.is_selected(&t.key), host, p)),
        );
    }
    if !view.cards.is_empty() {
        list = list.child(section(
            cards_title(scope.window),
            view.cards.len(),
            p.muted,
            first,
        ));
        list = list.children(
            view.cards
                .iter()
                .enumerate()
                .map(|(i, c)| card_row(i, c, frame.is_selected(&c.key), host, p)),
        );
    }
    if view.tiles.is_empty() && view.cards.is_empty() {
        list = list.child(
            div()
                .px(px(10.))
                .text_size(px(12.))
                .text_color(p.muted.hsla())
                .child("Nothing in scope."),
        );
    }

    let footer = {
        let host = host.clone();
        let text = match view.quiet_count {
            0 => "Every session is shown".to_string(),
            1 => "1 quiet session hidden".to_string(),
            n => format!("{n} quiet sessions hidden"),
        };
        h_flex()
            .flex_shrink_0()
            .px(px(18.))
            .py(px(12.))
            .gap_1()
            .border_t_1()
            .border_color(p.hairline.hsla())
            .text_size(px(12.))
            .text_color(p.muted.hsla())
            .child(text)
            .child("·")
            .child(
                div()
                    .id("mission-see-all")
                    .debug_selector(|| "mission-see-all".into())
                    .text_color(p.accent.hsla())
                    .cursor_pointer()
                    .child("see all in Projects")
                    .on_click(move |_, _, cx| switch_view(&host, MainView::Projects, cx)),
            )
    };

    v_flex()
        .id("mission-sidebar")
        .flex_shrink_0()
        .w(SIDEBAR_WIDTH)
        .h_full()
        .bg(p.surface_sidebar.hsla())
        .border_r_1()
        .border_color(p.hairline.hsla())
        .child(
            div()
                .id("mission-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(list),
        )
        .child(footer)
}

/// A live session's rail row: monogram with badge, name and age, project and
/// status.
fn session_row(
    index: usize,
    t: &MissionTile,
    selected: bool,
    host: &Entity<HostModel>,
    p: &Palette,
) -> AnyElement {
    let row = &t.row;
    let row_bg = if selected {
        p.surface_raised
    } else {
        p.surface_sidebar
    };
    let monogram_tile = div()
        .relative()
        .flex_none()
        .size(px(28.))
        .rounded(px(7.))
        .bg(if selected {
            p.surface_raised_nested
        } else {
            p.surface_tile
        }
        .hsla())
        .flex()
        .items_center()
        .justify_center()
        .font_family(MONO_FAMILY)
        .text_size(px(10.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(monogram_ink(&row.agent_name, p).hsla())
        .child(monogram(&row.agent_name))
        .child(
            div()
                .absolute()
                .right(px(-4.))
                .bottom(px(-4.))
                .size(px(14.))
                .rounded_full()
                .bg(row_bg.hsla())
                .flex()
                .items_center()
                .justify_center()
                .child(icons::agent_glyph(
                    ("mission-row-badge", index),
                    row.badge,
                    px(10.),
                    p,
                )),
        );
    let age = row
        .status_since_secs
        .map(format_elapsed)
        .unwrap_or_default();
    let key = t.key.clone();
    let tab_index = t.tab_index;
    let host = host.clone();
    h_flex()
        .id(("mission-row", index))
        .debug_selector(move || format!("mission-row-{index}"))
        .items_start()
        .gap(px(11.))
        .px(px(10.))
        .py(px(9.))
        .rounded(px(8.))
        .when(selected, |d| {
            d.bg(p.surface_raised.hsla())
                .border_1()
                .border_color(p.border.hsla())
        })
        .cursor_pointer()
        .child(monogram_tile)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    h_flex()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(13.))
                                .font_weight(if selected {
                                    FontWeight::SEMIBOLD
                                } else {
                                    FontWeight::MEDIUM
                                })
                                .text_color(p.ink.hsla())
                                .child(row.name.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(11.))
                                .text_color(p.muted.hsla())
                                .child(age),
                        ),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(11.5))
                        .text_color(p.muted.hsla())
                        .child(format!("{} · {}", t.project_name, status_words(row))),
                ),
        )
        .on_click(move |_, _, cx| {
            let mode = if host.read(cx).host().terminal_focused() {
                InputMode::Terminal
            } else {
                InputMode::App
            };
            select_session(&host, &key, tab_index, mode, cx);
        })
        .into_any_element()
}

/// A recent session's compact rail row: glyph, name, age.
fn card_row(
    index: usize,
    c: &MissionCard,
    selected: bool,
    host: &Entity<HostModel>,
    p: &Palette,
) -> AnyElement {
    let age = c
        .last_event
        .map(|e| format_elapsed(e.secs_ago))
        .unwrap_or_default();
    let key = c.key.clone();
    let tab_index = c.tab_index;
    let host = host.clone();
    h_flex()
        .id(("mission-card-row", index))
        .debug_selector(move || format!("mission-card-row-{index}"))
        .gap(px(11.))
        .px(px(10.))
        .py(px(7.))
        .rounded(px(8.))
        .when(selected, |d| d.bg(p.surface_raised.hsla()))
        .cursor_pointer()
        .child(
            div()
                .flex_none()
                .w(px(12.))
                .flex()
                .justify_center()
                .child(icons::agent_glyph(
                    ("mission-card-row-glyph", index),
                    c.row.badge,
                    px(12.),
                    p,
                )),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(13.))
                .text_color(p.ink_2.hsla())
                .child(c.row.name.clone()),
        )
        .child(div().flex_1())
        .child(
            div()
                .flex_none()
                .text_size(px(11.))
                .text_color(p.muted.hsla())
                .child(age),
        )
        .on_click(move |_, _, cx| select_session(&host, &key, tab_index, InputMode::App, cx))
        .into_any_element()
}

/// Mission control's status bar: the mode pill and the hints A2 / A3 show,
/// the remote indicators, help. Every hint is its keymap entry's keycap and
/// click.
pub fn mission_status_bar(host: &Entity<HostModel>, p: &Palette, cx: &App) -> impl IntoElement {
    let model = host.read(cx);
    let mode = model.host().active_state().mode();
    let remote = model.host().remote_status();
    let notices = model.host().notices();
    let (pill_bg, pill_ink, word, pill_entry) = match mode {
        InputMode::Terminal => (
            p.pill_terminal_bg,
            p.pill_terminal_ink,
            "TERMINAL",
            "FocusApp",
        ),
        InputMode::App => (p.pill_app_bg, p.pill_app_ink, "APP", "FocusTerminal"),
    };
    let pill = {
        let host = host.clone();
        div()
            .id("mode-pill")
            .debug_selector(|| "mode-pill".into())
            .flex_none()
            .px_2()
            .py(px(2.))
            .rounded(px(4.))
            .bg(pill_bg.hsla())
            .text_color(pill_ink.hsla())
            .font_family(MONO_FAMILY)
            .text_size(px(10.5))
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .child(word)
            .on_click(move |_, _, cx| perform_id(pill_entry, &host, cx))
    };
    let jump = format!("{}…9", keycap("JumpToAgentTab1").unwrap_or_default());
    let hints: Vec<(String, &'static str, Option<&'static str>)> = match mode {
        InputMode::App => vec![
            ("↑↓←→".to_string(), "move between tiles", None),
            (
                keycap("FocusTerminal").unwrap_or_default(),
                "open full size",
                Some("FocusTerminal"),
            ),
            (jump, "jump", None),
            (
                keycap("ToggleMissionControl").unwrap_or_default(),
                "switch view",
                Some("ToggleMissionControl"),
            ),
        ],
        InputMode::Terminal => vec![
            (
                keycap("FocusApp").unwrap_or_default(),
                "back to grid",
                Some("FocusApp"),
            ),
            (jump, "swap session", None),
        ],
    };
    let hint = |key: String, label: &'static str, entry: Option<&'static str>| {
        let host = host.clone();
        h_flex()
            .id(label)
            .debug_selector(move || format!("mission-hint-{label}"))
            .flex_none()
            .gap_1()
            .when(entry.is_some(), |d| {
                d.cursor_pointer().hover(|s| s.text_color(p.ink_2.hsla()))
            })
            .child(
                div()
                    .font_family(MONO_FAMILY)
                    .text_color(p.ink_2.hsla())
                    .child(key),
            )
            .child(label)
            .on_click(move |_, _, cx| {
                if let Some(entry) = entry {
                    perform_id(entry, &host, cx);
                }
            })
    };
    h_flex()
        .flex_shrink_0()
        .h(crate::views::status_bar::STATUS_BAR_HEIGHT)
        .px(px(14.))
        .gap_4()
        .bg(p.surface_window.hsla())
        .border_t_1()
        .border_color(p.hairline.hsla())
        .text_size(px(11.5))
        .text_color(p.muted.hsla())
        .child(pill)
        .children(hints.into_iter().map(|(k, l, e)| hint(k, l, e)))
        .child(div().flex_1())
        .children(flightdeck_desktop::overlays::update::isolated_badge(
            &notices, cx,
        ))
        .child(flightdeck_desktop::overlays::remote::remote_indicator(
            &remote, cx,
        ))
        .child(hint(
            keycap("OpenHelp").unwrap_or_default(),
            "help",
            Some("OpenHelp"),
        ))
}
