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
//! mask). The terminal element's damage tracking (being built alongside this)
//! is not used yet: a redrawn tile lays out its whole grid, which the refresh
//! cap keeps cheap. Moving tiles onto it is the next step once it lands.
//!
//! [`AppHost::tab_terminal`]: flightdeck::host::AppHost::tab_terminal

use flightdeck::view::SessionKey;
use gpui::{
    fill, px, relative, size, App, Bounds, Context, Element, ElementId, Entity, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Render, Style, Window,
};

use crate::host::HostModel;
use crate::terminal::element::{mono_font, paint_box, paint_text, CellMetrics};
use crate::terminal::layout::{self, FrameLayout, TermPalette};
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
}

impl TileView {
    pub fn new(host: Entity<HostModel>, key: SessionKey, cx: &mut Context<Self>) -> Self {
        Self {
            host,
            key,
            palette: TermPalette::from_palette(Palette::global(cx)),
        }
    }
}

impl Render for TileView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        TileGrid {
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

/// Keep what falls in rows `start..end` and move it up by `start`.
fn window_of(frame: FrameLayout, start: u16, end: u16) -> FrameLayout {
    let keep = |row: u16| row >= start && row < end;
    FrameLayout {
        rows: end - start,
        cols: frame.cols,
        backgrounds: frame
            .backgrounds
            .into_iter()
            .filter(|s| keep(s.row))
            .map(|mut s| {
                s.row -= start;
                s
            })
            .collect(),
        selection: Vec::new(),
        cursor: None,
        texts: frame
            .texts
            .into_iter()
            .filter(|s| keep(s.row))
            .map(|mut s| {
                s.row -= start;
                s
            })
            .collect(),
        boxes: frame
            .boxes
            .into_iter()
            .filter(|s| keep(s.row))
            .map(|mut s| {
                s.row -= start;
                s
            })
            .collect(),
    }
}

/// The lowest row anything is drawn on.
fn last_content_row(frame: &FrameLayout) -> Option<u16> {
    let texts = frame
        .texts
        .iter()
        .filter(|t| !t.text.trim().is_empty())
        .map(|t| t.row);
    let boxes = frame.boxes.iter().map(|b| b.row);
    let fills = frame.backgrounds.iter().map(|b| b.row);
    texts.chain(boxes).chain(fills).max()
}

struct TileGrid {
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

struct Prepared {
    frame: Option<(FrameLayout, CellMetrics, Pixels)>,
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
        if !window.content_mask().bounds.intersects(&bounds) {
            return Prepared { frame: None };
        }
        let host = self.host.read(cx).host();
        let Some(terminal) = host.tab_terminal(self.key.project, &self.key.tab_id) else {
            return Prepared { frame: None };
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

        let frame = layout::layout(terminal.screen(), None, &self.palette, false);
        let cursor = terminal.screen().cursor();
        let cursor_row = cursor.visible.then_some(cursor.row);
        let (start, end) = visible_rows(rows, fit, last_content_row(&frame), cursor_row);
        Prepared {
            frame: Some((window_of(frame, start, end), metrics, font_size)),
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
        let Some((frame, m, font_size)) = &prepared.frame else {
            return;
        };
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            let cell = |row: u16, col: u16, cols: u16| {
                Bounds::new(
                    gpui::point(
                        m.origin.x + m.width * f32::from(col),
                        m.origin.y + m.height * f32::from(row),
                    ),
                    size(m.width * f32::from(cols), m.height),
                )
            };
            for span in &frame.backgrounds {
                window.paint_quad(fill(cell(span.row, span.col, span.cols), span.color.hsla()));
            }
            for span in &frame.texts {
                paint_text(span, m, *font_size, window, cx);
            }
            for b in &frame.boxes {
                paint_box(&b.glyph, cell(b.row, b.col, 1), b.fg.hsla(), window);
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
