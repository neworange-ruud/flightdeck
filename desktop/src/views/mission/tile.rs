//! A tile's live terminal: any session's grid, read-only and scaled down.
//!
//! The grid is the host's ([`AppHost::tab_terminal`]), sized by the Projects
//! view's PTY viewport, which is far larger than a tile. Squeezing every cell
//! in would mean a 5px font; instead the tile picks a small readable size
//! ([`MIN_FONT`]..[`MAX_FONT`], shrinking to fit the width when it can) and
//! shows the **bottom** of the content — the rows ending at the cursor or the
//! last non-blank row, whichever is lower — which is where an agent's prompt,
//! question or progress line lives. Columns are anchored left and clipped.
//!
//! Nothing here writes: no resize (the PTY keeps the Projects view's size),
//! no input, no selection. Opening the session (Enter) is how to type.
//!
//! ## Rendering budget
//!
//! Each [`TileView`] is its own entity, embedded as a GPUI *cached* view
//! (`Entity::cached`): a window redraw replays its last frame unless the tile
//! itself was notified. Mission control notifies the selected tile whenever
//! the host reports output and every other tile at most every
//! `TILE_REFRESH` (100 ms, ≤10 fps), so a grid of busy agents costs a few
//! layouts per second, not one per tile per frame. A tile scrolled out of the
//! grid's viewport lays out and paints nothing (its bounds miss the content
//! mask). A redrawn tile uses the terminal element's damage tracking: each
//! tile keeps its own [`RowCache`] (its own damage position, so it never takes
//! the Projects view's damage or another tile's), lays out only the rows that
//! changed since it last drew, and shapes only the rows it shows, keeping the
//! shaped lines until they change or the tile's font size does. An idle
//! Mission control notifies no tile at all (the host only notifies on
//! change; see `crate::host`).
//!
//! [`AppHost::tab_terminal`]: flightdeck::host::AppHost::tab_terminal

use flightdeck::terminal::grid::GridDamage;
use flightdeck::view::SessionKey;
use gpui::{
    fill, point, px, relative, size, App, Bounds, Context, Element, ElementId, Entity,
    GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Render, ShapedLine, Style,
    TextAlign, Window,
};

use crate::host::HostModel;
use crate::terminal::element::{mono_font, paint_box, shape_text, CellMetrics};
use crate::terminal::layout::{RowLayout, TermPalette};
use crate::terminal::rowcache::{CachedRow, RowCache};
use crate::terminal::view::grid_identity;
use crate::theme::Palette;

/// The smallest tile font: below this Geist Mono stops being text.
pub const MIN_FONT: f32 = 9.0;
/// The largest: the mockup's tile text (the full terminal is 13px).
pub const MAX_FONT: f32 = 11.5;
/// Line height as a multiple of the font size (the mockup's 12px/18px).
const LINE_HEIGHT: f32 = 1.45;

/// One tile's grid, as a cached view (see the module docs).
pub struct TileView {
    host: Entity<HostModel>,
    key: SessionKey,
    palette: TermPalette,
    /// The rows laid out and shaped on earlier frames; lent to the element
    /// for each frame (see the module docs).
    cache: RowCache<Vec<ShapedLine>>,
    /// The font size and cell width the cached lines were shaped at.
    shaped_at: Option<(Pixels, Pixels)>,
}

impl TileView {
    pub fn new(host: Entity<HostModel>, key: SessionKey, cx: &mut Context<Self>) -> Self {
        Self {
            host,
            key,
            palette: TermPalette::from_palette(Palette::global(cx)),
            cache: RowCache::default(),
            shaped_at: None,
        }
    }
}

impl TileView {
    /// Whether this tile's terminal changed since the tile last drew it
    /// (damage since its own position, or a different terminal altogether),
    /// so Mission control redraws only the tiles that need it.
    pub fn has_changes(&self, host: &flightdeck::host::AppHost<'_>) -> bool {
        match host.tab_terminal(self.key.project, &self.key.tab_id) {
            None => !self.cache.rows().is_empty(),
            Some(terminal) => {
                self.cache.source() != Some(grid_identity(terminal))
                    || terminal.screen().damage_since(self.cache.seen).0
                        != GridDamage::Rows(Vec::new())
            }
        }
    }
}

impl Render for TileView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        TileGrid {
            tile: cx.entity(),
            host: self.host.clone(),
            key: self.key.clone(),
            palette: self.palette,
        }
    }
}

/// The font size a tile of `width` uses for a grid `cols` wide, given the
/// font's advance per pixel of size (`advance_ratio`): as large as fits the
/// width, within [`MIN_FONT`]..[`MAX_FONT`]. Pure, for the tests.
pub fn tile_font_size(width: f32, cols: u16, advance_ratio: f32) -> f32 {
    if cols == 0 || advance_ratio <= 0.0 {
        return MAX_FONT;
    }
    (width / (f32::from(cols) * advance_ratio)).clamp(MIN_FONT, MAX_FONT)
}

/// The rows of a `rows`-high grid a tile `fit` rows tall shows: the window
/// ending just below the lowest of the cursor and the last row with content.
/// Pure, for the tests.
pub fn visible_rows(
    rows: u16,
    fit: u16,
    last_content: Option<u16>,
    cursor: Option<u16>,
) -> (u16, u16) {
    let bottom = last_content.max(cursor).map_or(0, |r| r + 1).min(rows);
    let end = bottom.max(fit.min(rows));
    (end.saturating_sub(fit), end)
}

/// The lowest row anything is drawn on.
fn last_content_row<S>(rows: &[CachedRow<S>]) -> Option<u16> {
    let drawn = |layout: &RowLayout| {
        layout.texts.iter().any(|t| !t.text.trim().is_empty())
            || !layout.boxes.is_empty()
            || !layout.backgrounds.is_empty()
    };
    rows.iter()
        .enumerate()
        .filter(|(_, row)| drawn(&row.layout))
        .map(|(i, _)| i as u16)
        .max()
}

struct TileGrid {
    tile: Entity<TileView>,
    host: Entity<HostModel>,
    key: SessionKey,
    palette: TermPalette,
}

impl IntoElement for TileGrid {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// What prepaint hands paint: the tile's cache (handed back after paint),
/// the rows shown, and the cell metrics shifted so row `start` is at the top.
struct Prepared {
    cache: RowCache<Vec<ShapedLine>>,
    shown: Option<(u16, u16, CellMetrics, Pixels)>,
}

impl Element for TileGrid {
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
        // Scrolled out of the grid: no layout, no paint.
        let mut cache = self
            .tile
            .update(cx, |tile, _| std::mem::take(&mut tile.cache));
        let shaped_at = self.tile.read(cx).shaped_at;
        if !window.content_mask().bounds.intersects(&bounds) {
            return Prepared { cache, shown: None };
        }
        let host = self.host.read(cx).host();
        let Some(terminal) = host.tab_terminal(self.key.project, &self.key.tab_id) else {
            return Prepared { cache, shown: None };
        };
        let (rows, cols) = terminal.screen().size();

        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&mono_font());
        let probe = px(100.);
        let advance_ratio = text_system
            .advance(font_id, probe, 'm')
            .map(|s| f32::from(s.width) / 100.)
            .unwrap_or(0.6);
        let font = tile_font_size(f32::from(bounds.size.width), cols, advance_ratio);
        let font_size = px(font);
        let metrics = CellMetrics {
            origin: bounds.origin,
            width: px(font * advance_ratio),
            height: px((font * LINE_HEIGHT).round()),
        };
        let fit = (bounds.size.height / metrics.height).floor().max(1.0) as u16;

        // Only the rows that changed since this tile last drew are laid out.
        cache.refresh(
            grid_identity(terminal),
            terminal.screen(),
            &self.palette,
            false,
        );
        if shaped_at != Some((font_size, metrics.width)) {
            cache.invalidate_derived();
        }
        let cursor = terminal.screen().cursor();
        let cursor_row = cursor.visible.then_some(cursor.row);
        let (start, end) = visible_rows(rows, fit, last_content_row(cache.rows()), cursor_row);
        // Shape only what is shown and not shaped yet.
        let (from, to) = (usize::from(start), usize::from(end).min(cache.rows().len()));
        for row in &mut cache.rows_mut()[from.min(to)..to] {
            if row.derived.is_none() {
                let lines = row
                    .layout
                    .texts
                    .iter()
                    .map(|span| shape_text(span, metrics.width, font_size, window))
                    .collect();
                row.derived = Some(lines);
            }
        }
        let shifted = CellMetrics {
            origin: point(
                metrics.origin.x,
                metrics.origin.y - metrics.height * f32::from(start),
            ),
            ..metrics
        };
        Prepared {
            cache,
            shown: Some((start, end, shifted, font_size)),
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
        if let Some((start, end, m, _)) = prepared.shown {
            let rows = prepared.cache.rows();
            let (from, to) = (usize::from(start), usize::from(end).min(rows.len()));
            let shown = &rows[from.min(to)..to];
            window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                let cell = |row: u16, col: u16, cols: u16| {
                    Bounds::new(
                        point(
                            m.origin.x + m.width * f32::from(col),
                            m.origin.y + m.height * f32::from(row),
                        ),
                        size(m.width * f32::from(cols), m.height),
                    )
                };
                for span in shown.iter().flat_map(|r| &r.layout.backgrounds) {
                    window.paint_quad(fill(cell(span.row, span.col, span.cols), span.color.hsla()));
                }
                for row in shown {
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
                for b in shown.iter().flat_map(|r| &r.layout.boxes) {
                    paint_box(&b.glyph, cell(b.row, b.col, 1), b.fg.hsla(), window);
                }
            });
        }
        crate::terminal::bench::PAINTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let cache = std::mem::take(&mut prepared.cache);
        let shaped_at = prepared
            .shown
            .map(|(_, _, m, font_size)| (font_size, m.width));
        self.tile.update(cx, |tile, _| {
            tile.cache = cache;
            if shaped_at.is_some() {
                tile.shaped_at = shaped_at;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_font_shrinks_to_fit_the_width_within_its_bounds() {
        // Geist Mono's advance is ~0.6 of its size.
        assert_eq!(tile_font_size(1000.0, 80, 0.6), MAX_FONT);
        let fitted = tile_font_size(560.0, 100, 0.6);
        assert!((fitted - 560.0 / 60.0).abs() < 1e-3, "{fitted}");
        assert_eq!(
            tile_font_size(200.0, 200, 0.6),
            MIN_FONT,
            "clipped, not tiny"
        );
        assert_eq!(tile_font_size(500.0, 0, 0.6), MAX_FONT);
    }

    #[test]
    fn the_lowest_content_row_is_read_from_the_row_cache() {
        use flightdeck::terminal::grid::Emulator;
        let palette = TermPalette::from_palette(&Palette::dark());
        let mut grid = Emulator::Alacritty.build(8, 20);
        let mut cache: RowCache<()> = RowCache::default();
        cache.refresh(1, grid.as_ref(), &palette, false);
        assert_eq!(last_content_row(cache.rows()), None, "blank");
        grid.process(b"one\r\n\r\nthree");
        cache.refresh(1, grid.as_ref(), &palette, false);
        assert_eq!(last_content_row(cache.rows()), Some(2));
        // A tile of the same grid keeps its own damage position: another
        // reader's refresh does not hide the change from it.
        let mut other: RowCache<()> = RowCache::default();
        other.refresh(1, grid.as_ref(), &palette, false);
        grid.process(b"\r\n\r\nfive");
        assert!(cache.refresh(1, grid.as_ref(), &palette, false) >= 1);
        assert!(other.refresh(1, grid.as_ref(), &palette, false) >= 1);
        assert_eq!(last_content_row(other.rows()), Some(4));
    }

    #[test]
    fn the_window_ends_at_the_lowest_content() {
        // 38-row grid, 10 rows fit.
        assert_eq!(
            visible_rows(38, 10, Some(37), None),
            (28, 38),
            "full screen"
        );
        assert_eq!(
            visible_rows(38, 10, Some(20), Some(21)),
            (12, 22),
            "cursor lower"
        );
        assert_eq!(
            visible_rows(38, 10, Some(3), Some(2)),
            (0, 10),
            "short output: top"
        );
        assert_eq!(visible_rows(38, 10, None, None), (0, 10), "blank");
        assert_eq!(
            visible_rows(5, 10, Some(4), None),
            (0, 5),
            "grid shorter than tile"
        );
    }
}
