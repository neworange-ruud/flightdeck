//! Split view (`Ctrl-b`): the selected agent's terminals side by side, as the
//! TUI lays them out (`draw_split_view`, `layout::split_columns`).
//!
//! ```text
//! +----------------+-+----------------+-+----------------+
//! | agent  claude  |│| shell 1  zsh   |│| shell 2  zsh   |   pane headers
//! +----------------+│+----------------+│+----------------+
//! | the focused    |│| dimmed         |│| dimmed         |
//! | terminal       |│|                |│|                |
//! +----------------+-+----------------+-+----------------+
//! ```
//!
//! ## The TUI's rules, unchanged
//!
//! - **Which terminals, in what order:** the primary agent, then every child
//!   (extra agents and shells) in creation order, labelled `agent`,
//!   `agent 2`, `shell 1`… — [`flightdeck::view::terminal_views`], the list
//!   the sidebar's nested rows and the TUI's columns come from.
//! - **Columns:** equal widths separated by a one-unit gutter (here a hairline),
//!   each topped by a header with its label; the header of the active
//!   terminal is highlighted.
//! - **Focus:** the active terminal is the tab's selected child (or its
//!   agent), exactly as without split view. Left/Right (APP) and
//!   Alt-Left/Right (both modes) cycle it, a nested sidebar row or a pane (its
//!   header or its body) selects it, and input goes to it alone
//!   (`HostEvent::TerminalInput` writes the active terminal). Ctrl-t adds a
//!   pane (the new shell becomes active), Ctrl-w closes the active child's.
//! - **Sizes:** each terminal's PTY is its own pane's size, measured by the
//!   pane and applied by the host model through
//!   [`AppHost::resize_terminal`](flightdeck::host::AppHost::resize_terminal)
//!   (the TUI's `sync_terminal_sizes`).
//!
//! ## What draws a pane
//!
//! The active pane holds the window's one [`TerminalView`] (keyboard, IME,
//! selection, scrollback, mouse reporting), moved from pane to pane as the
//! focus moves. Every other pane is a read-only [`PaneView`]: the same row
//! cache, layout and glyph painting as the terminal element, at the same font
//! and padding (so a terminal's size does not change when it gains focus),
//! with no cursor — as in the TUI, only the active column shows one — dimmed
//! under the theme's [`Palette::pane_dim`]. Clicking it makes it the active
//! terminal and focuses it, like the TUI's click in a column.
//!
//! [`TerminalView`]: crate::terminal::view::TerminalView

use std::collections::HashMap;

use flightdeck::app::state::TabPhase;
use flightdeck::host::AppHost;
use flightdeck::persistence::workspace::MainView;
use flightdeck::view::{terminal_views, TerminalRef, TerminalView as TerminalRow};
use gpui::{
    fill, point, px, relative, size, App, AppContext, Bounds, Context, Element, ElementId, Entity,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Render, ShapedLine, Style,
    TextAlign, Window,
};

use crate::host::HostModel;
use crate::terminal::element::{measure_cell, paint_box, shape_text, CellMetrics};
use crate::terminal::layout::TermPalette;
use crate::terminal::rowcache::RowCache;
use crate::terminal::view::grid_identity;
use crate::terminal::zoom::{app_font_size, TerminalZoom};
use crate::theme::Palette;

/// A pane's header row: the terminal's label and command.
pub const PANE_HEADER: Pixels = px(28.);

/// The padding around a pane's grid: the app terminal's own (the terminal
/// view pads its element by 18px / 22px), so a pane measures the same grid
/// whichever of the two draws it.
pub const PANE_PAD_Y: Pixels = px(18.);
pub const PANE_PAD_X: Pixels = px(22.);

/// What split view shows this frame: `None` when it is off, or there is no
/// agent selected, or its worktree is still being created (then the main area
/// is what it is without split view, as the TUI draws its "Creating…" line).
/// Only the Projects view draws it; Mission control's focus view keeps one
/// terminal full size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitLayout {
    pub project: usize,
    pub tab_id: String,
    /// The tab's terminals in column order (agent first).
    pub panes: Vec<TerminalRow>,
    /// The active terminal: the pane that gets input.
    pub active: TerminalRef,
}

impl SplitLayout {
    /// Read it from the host.
    pub fn read(host: &AppHost<'_>) -> Option<SplitLayout> {
        let state = host.active_state();
        if !state.split_view || host.workspace_ui().view != MainView::Projects {
            return None;
        }
        let tab = state.selected()?;
        if tab.phase == TabPhase::Creating {
            return None;
        }
        Some(SplitLayout {
            project: host.active_project_index(),
            tab_id: tab.meta.id.clone(),
            panes: terminal_views(tab),
            active: match tab.session.selected_child() {
                Some(i) => TerminalRef::Child(i),
                None => TerminalRef::Primary,
            },
        })
    }

    /// Which tab and how many columns: the sizes measured for one key say
    /// nothing about another (a pane more or fewer changes every width).
    pub fn key(&self) -> SplitKey {
        SplitKey {
            project: self.project,
            tab_id: self.tab_id.clone(),
            panes: self.panes.len(),
        }
    }
}

/// See [`SplitLayout::key`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SplitKey {
    pub project: usize,
    pub tab_id: String,
    pub panes: usize,
}

/// The PTY sizes split view's panes measured, for the host model to apply
/// (see [`crate::host::HostModel::set_pane_size`]). Pure bookkeeping: sizes
/// for a key other than the current one are forgotten, so a tab switch or a
/// pane added never applies a stale column width.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PaneSizes {
    key: Option<SplitKey>,
    sizes: Vec<(TerminalRef, flightdeck::contracts::PtySize)>,
}

impl PaneSizes {
    /// Record `target`'s measured size under `key`. Returns whether anything
    /// changed.
    pub fn record(
        &mut self,
        key: &SplitKey,
        target: TerminalRef,
        size: flightdeck::contracts::PtySize,
    ) -> bool {
        if self.key.as_ref() != Some(key) {
            self.key = Some(key.clone());
            self.sizes.clear();
        }
        match self.sizes.iter_mut().find(|(t, _)| *t == target) {
            Some((_, old)) if *old == size => false,
            Some((_, old)) => {
                *old = size;
                true
            }
            None => {
                self.sizes.push((target, size));
                true
            }
        }
    }

    /// The sizes measured for `key` (none for any other key).
    pub fn for_key(
        &self,
        key: &SplitKey,
    ) -> impl Iterator<Item = (TerminalRef, flightdeck::contracts::PtySize)> + '_ {
        let current = self.key.as_ref() == Some(key);
        self.sizes.iter().copied().filter(move |_| current)
    }
}

/// Identifies one read-only pane's view across frames.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PaneKey {
    pub project: usize,
    pub tab_id: String,
    /// `None` for the agent, `Some(i)` for child `i`.
    pub child: Option<usize>,
}

impl PaneKey {
    pub fn new(layout: &SplitLayout, target: TerminalRef) -> PaneKey {
        PaneKey {
            project: layout.project,
            tab_id: layout.tab_id.clone(),
            child: match target {
                TerminalRef::Primary => None,
                TerminalRef::Child(i) => Some(i),
            },
        }
    }

    pub fn target(&self) -> TerminalRef {
        match self.child {
            None => TerminalRef::Primary,
            Some(i) => TerminalRef::Child(i),
        }
    }
}

/// The read-only panes' views, kept across frames (each keeps its own row
/// cache) and dropped once their pane leaves the screen.
#[derive(Default)]
pub struct Panes {
    views: HashMap<PaneKey, Entity<PaneView>>,
}

impl Panes {
    /// The view for `key`, created on first sight.
    pub fn view(
        &mut self,
        host: &Entity<HostModel>,
        key: PaneKey,
        cx: &mut App,
    ) -> Entity<PaneView> {
        self.views
            .entry(key.clone())
            .or_insert_with(|| cx.new(|cx| PaneView::new(host.clone(), key, cx)))
            .clone()
    }

    /// Forget every view not in `shown`.
    pub fn retain(&mut self, shown: &[PaneKey]) {
        self.views.retain(|key, _| shown.contains(key));
    }
}

/// One read-only pane: a terminal of the selected agent that is not the
/// active one. See the module docs.
pub struct PaneView {
    host: Entity<HostModel>,
    key: PaneKey,
    palette: TermPalette,
    cache: RowCache<Vec<ShapedLine>>,
    /// The cell size the cached lines were shaped at.
    shaped_at: Option<(Pixels, Pixels)>,
}

impl PaneView {
    fn new(host: Entity<HostModel>, key: PaneKey, cx: &mut Context<Self>) -> Self {
        // A zoom chord resizes every terminal, the panes included.
        cx.observe_global::<TerminalZoom>(|_, cx| cx.notify())
            .detach();
        Self {
            host,
            key,
            palette: TermPalette::from_palette(Palette::global(cx)),
            cache: RowCache::default(),
            shaped_at: None,
        }
    }
}

impl Render for PaneView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        PaneGrid {
            pane: cx.entity(),
            host: self.host.clone(),
            key: self.key.clone(),
            palette: self.palette,
        }
    }
}

/// The grid of a [`PaneView`].
struct PaneGrid {
    pane: Entity<PaneView>,
    host: Entity<HostModel>,
    key: PaneKey,
    palette: TermPalette,
}

impl IntoElement for PaneGrid {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// What prepaint hands paint: the pane's cache (handed back after paint) and
/// the cell metrics, when there is a terminal to draw.
struct Prepared {
    cache: RowCache<Vec<ShapedLine>>,
    metrics: Option<CellMetrics>,
}

impl Element for PaneGrid {
    type RequestLayoutState = ();
    type PrepaintState = Prepared;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let mut cache = self
            .pane
            .update(cx, |pane, _| std::mem::take(&mut pane.cache));
        let shaped_at = self.pane.read(cx).shaped_at;
        // The active terminal's size (setting plus zoom), so a pane's grid is
        // the one it has when it gains focus.
        let points = app_font_size(self.host.read(cx).host(), cx);
        let (width, height) = measure_cell(window, points);
        // The grid follows the pane, as the terminal element's does.
        let size = flightdeck::contracts::PtySize {
            rows: (bounds.size.height / height).floor().max(1.0) as u16,
            cols: (bounds.size.width / width).floor().max(1.0) as u16,
        };
        let target = self.key.target();
        let (project, tab_id) = (self.key.project, self.key.tab_id.clone());
        self.host.update(cx, |model, _| {
            model.set_pane_size(project, &tab_id, target, size)
        });
        let host = self.host.read(cx).host();
        let Some(terminal) = host.tab_terminal_at(project, &tab_id, target) else {
            return Prepared {
                cache,
                metrics: None,
            };
        };
        // Only the rows that changed since this pane last drew are laid out.
        cache.refresh(
            grid_identity(terminal),
            terminal.screen(),
            &self.palette,
            false,
        );
        if shaped_at != Some((width, height)) {
            cache.invalidate_derived();
        }
        let font_size = px(points);
        for row in cache.rows_mut() {
            if row.derived.is_none() {
                let lines = row
                    .layout
                    .texts
                    .iter()
                    .map(|span| shape_text(span, width, font_size, window))
                    .collect();
                row.derived = Some(lines);
            }
        }
        Prepared {
            cache,
            metrics: Some(CellMetrics {
                origin: bounds.origin,
                width,
                height,
            }),
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepared: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.paint_quad(fill(bounds, self.palette.bg.hsla()));
        if let Some(m) = prepared.metrics {
            let cell = |row: u16, col: u16, cols: u16| {
                Bounds::new(
                    point(
                        m.origin.x + m.width * f32::from(col),
                        m.origin.y + m.height * f32::from(row),
                    ),
                    size(m.width * f32::from(cols), m.height),
                )
            };
            let rows = prepared.cache.rows();
            window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                for span in rows.iter().flat_map(|r| &r.layout.backgrounds) {
                    window.paint_quad(fill(cell(span.row, span.col, span.cols), span.color.hsla()));
                }
                // No cursor: only the active pane shows one (the TUI's rule).
                for row in rows {
                    let Some(lines) = &row.derived else {
                        continue;
                    };
                    for (span, line) in row.layout.texts.iter().zip(lines) {
                        let _ = line.paint(
                            cell(span.row, span.col, 1).origin,
                            m.height,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                }
                for b in rows.iter().flat_map(|r| &r.layout.boxes) {
                    paint_box(&b.glyph, cell(b.row, b.col, 1), b.fg.hsla(), window);
                }
            });
        }
        crate::terminal::bench::PAINTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let cache = std::mem::take(&mut prepared.cache);
        let shaped_at = prepared.metrics.map(|m| (m.width, m.height));
        self.pane.update(cx, |pane, _| {
            pane.cache = cache;
            if shaped_at.is_some() {
                pane.shaped_at = shaped_at;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flightdeck::contracts::PtySize;

    fn key(tab: &str, panes: usize) -> SplitKey {
        SplitKey {
            project: 0,
            tab_id: tab.to_string(),
            panes,
        }
    }

    #[test]
    fn pane_sizes_belong_to_one_tab_and_pane_count() {
        let mut sizes = PaneSizes::default();
        let two = key("t0", 2);
        let half = PtySize { rows: 30, cols: 60 };
        assert!(sizes.record(&two, TerminalRef::Primary, half));
        assert!(sizes.record(&two, TerminalRef::Child(0), half));
        assert!(
            !sizes.record(&two, TerminalRef::Child(0), half),
            "the same size is no change"
        );
        assert_eq!(sizes.for_key(&two).count(), 2);

        // A third pane narrows every column: the old widths are forgotten
        // rather than applied to the new layout.
        let three = key("t0", 3);
        assert_eq!(sizes.for_key(&three).count(), 0);
        let third = PtySize { rows: 30, cols: 39 };
        assert!(sizes.record(&three, TerminalRef::Child(1), third));
        assert_eq!(
            sizes.for_key(&three).collect::<Vec<_>>(),
            [(TerminalRef::Child(1), third)]
        );
        assert_eq!(sizes.for_key(&two).count(), 0, "nor kept for later");

        // Another tab likewise.
        assert_eq!(sizes.for_key(&key("t1", 3)).count(), 0);
    }

    #[test]
    fn a_pane_key_names_its_terminal() {
        let layout = SplitLayout {
            project: 1,
            tab_id: "t3".to_string(),
            panes: Vec::new(),
            active: TerminalRef::Primary,
        };
        assert_eq!(
            PaneKey::new(&layout, TerminalRef::Child(2)).target(),
            TerminalRef::Child(2)
        );
        assert_eq!(
            PaneKey::new(&layout, TerminalRef::Primary).target(),
            TerminalRef::Primary
        );
        assert_eq!(layout.key().panes, 0);
    }
}
