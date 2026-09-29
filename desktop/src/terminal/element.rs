//! The GPUI element that paints a terminal's cell grid.
//!
//! Each frame: measure the monospace cell, size the grid (and so the PTY) to
//! the bounds, bring the view's [`RowCache`] up to date (only rows the
//! emulator reports damaged are laid out and shaped again, see
//! [`super::rowcache`]), then paint it back to front — backgrounds, selection,
//! a filled cursor, text, box geometry, and the outline/bar/underline cursors.
//! Colours were resolved by the layout; this file only turns cells into
//! pixels.
//!
//! Text is shaped per span. An ASCII span is shaped with GPUI's forced
//! per-glyph advance set to the cell width, so every glyph lands on its column
//! even when the font's advance is fractional; other spans (a symbol from a
//! fallback font, a wide CJK or emoji grapheme) are shaped alone and placed at
//! their column, so a glyph with a foreign advance cannot shift its neighbours.

use gpui::{
    fill, point, px, relative, size, App, Bounds, Element, ElementId, ElementInputHandler, Entity,
    Font, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, PathBuilder, Pixels,
    Point, ShapedLine, SharedString, StrikethroughStyle, Style, TextAlign, TextRun, UnderlineStyle,
    Window,
};

use super::boxdraw::{BoxGlyph, Corner, Weight};
use super::layout::{CellSpan, TextSpan};
use super::rowcache::RowCache;
use super::view::TerminalView;
use crate::theme::Hex;
use flightdeck::contracts::PtySize;
use flightdeck::terminal::grid::CursorShape;

/// The monospace family terminals draw with: the bundled Geist Mono
/// (`crate::fonts`), the same on every OS.
const MONO_FAMILY: &str = crate::fonts::MONO_FAMILY;

/// Terminal text size.
const FONT_SIZE: f32 = 13.0;
/// Line height as a multiple of the font size, when the font's own
/// ascent + descent is tighter than this.
const MIN_LINE_HEIGHT: f32 = 1.25;

/// The measured size of one cell, and where the grid starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    pub origin: Point<Pixels>,
    pub width: Pixels,
    pub height: Pixels,
}

impl CellMetrics {
    /// The `(row, col)` under a window position, clamped to the grid.
    pub fn cell_at(&self, position: Point<Pixels>, rows: u16, cols: u16) -> (u16, u16) {
        let x = ((position.x - self.origin.x) / self.width).floor().max(0.0) as u16;
        let y = ((position.y - self.origin.y) / self.height)
            .floor()
            .max(0.0) as u16;
        (y.min(rows.saturating_sub(1)), x.min(cols.saturating_sub(1)))
    }

    fn cell_origin(&self, row: u16, col: u16) -> Point<Pixels> {
        point(
            self.origin.x + self.width * f32::from(col),
            self.origin.y + self.height * f32::from(row),
        )
    }

    fn cell_bounds(&self, row: u16, col: u16, cols: u16) -> Bounds<Pixels> {
        Bounds::new(
            self.cell_origin(row, col),
            size(self.width * f32::from(cols), self.height),
        )
    }
}

pub fn mono_font() -> Font {
    gpui::font(MONO_FAMILY)
}

/// Measure the cell for the mono font at [`FONT_SIZE`].
pub fn measure_cell(window: &Window) -> (Pixels, Pixels) {
    let font_size = px(FONT_SIZE);
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(&mono_font());
    let width = text_system
        .advance(font_id, font_size, 'm')
        .map(|s| s.width)
        .unwrap_or(px(FONT_SIZE * 0.6));
    let natural = text_system.ascent(font_id, font_size) + text_system.descent(font_id, font_size);
    let height = natural.max(px(FONT_SIZE * MIN_LINE_HEIGHT)).ceil();
    (width, height)
}

/// Paints the grid of the [`TerminalView`] it is given.
pub struct TerminalElement {
    view: Entity<TerminalView>,
    /// Register the view as the window's text input handler while focused,
    /// so typed text and IME compositions arrive through
    /// `EntityInputHandler` (the app; the spike types keys directly).
    text_input: bool,
}

impl TerminalElement {
    pub fn new(view: Entity<TerminalView>, text_input: bool) -> Self {
        Self { view, text_input }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// What prepaint hands paint. The row cache is the view's; the element holds
/// it for the length of one frame (paint hands it back), so painting can read
/// the shaped lines while it has the window and app borrowed.
pub struct Prepared {
    cache: RowCache<Vec<ShapedLine>>,
    selection: Vec<CellSpan>,
    metrics: CellMetrics,
    background: Hex,
    cursor_colour: Hex,
}

impl Element for TerminalElement {
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
        if let Some(probe) = self.view.read(cx).probe() {
            probe.borrow_mut().prepaint_started();
        }
        let (width, height) = measure_cell(window);
        let metrics = CellMetrics {
            origin: bounds.origin,
            width,
            height,
        };
        // The grid follows the element: as many whole cells as fit.
        let cols = (bounds.size.width / width).floor().max(1.0) as u16;
        let rows = (bounds.size.height / height).floor().max(1.0) as u16;
        let focused = self.view.read(cx).focus_handle().is_focused(window);

        let (mut cache, selection, palette) = self.view.update(cx, |view, cx| {
            // Shaped text is only valid for the cell size it was forced to.
            let resized_cells = view
                .metrics()
                .is_none_or(|old| (old.width, old.height) != (width, height));
            view.set_metrics(metrics);
            view.resize(PtySize { rows, cols }, cx);
            let (mut cache, selection) = view.prepare_rows(focused, cx);
            if resized_cells {
                cache.invalidate_derived();
            }
            (cache, selection, *view.palette())
        });

        // Shape what the update re-laid (and everything after a cell resize);
        // every other row paints the lines shaped on an earlier frame.
        let font_size = px(FONT_SIZE);
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
            selection,
            metrics,
            background: palette.bg,
            cursor_colour: palette.cursor,
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
        let m = prepared.metrics;
        let rows = prepared.cache.rows();
        window.paint_quad(fill(bounds, prepared.background.hsla()));
        if self.text_input {
            let focus = self.view.read(cx).focus_handle().clone();
            window.handle_input(
                &focus,
                ElementInputHandler::new(bounds, self.view.clone()),
                cx,
            );
        }

        let backgrounds = rows.iter().flat_map(|r| &r.layout.backgrounds);
        for span in backgrounds.chain(&prepared.selection) {
            window.paint_quad(fill(
                m.cell_bounds(span.row, span.col, span.cols),
                span.color.hsla(),
            ));
        }

        let cursor_colour = prepared.cursor_colour.hsla();
        let cursor = rows.iter().find_map(|r| r.layout.cursor);
        if let Some(cursor) = cursor.filter(|c| c.filled) {
            window.paint_quad(fill(
                m.cell_bounds(cursor.row, cursor.col, cursor.cols),
                cursor_colour,
            ));
        }

        for row in rows {
            let Some(lines) = &row.derived else {
                continue;
            };
            for (span, line) in row.layout.texts.iter().zip(lines) {
                let _ = line.paint(
                    m.cell_origin(span.row, span.col),
                    m.height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        }

        for cell in rows.iter().flat_map(|r| &r.layout.boxes) {
            paint_box(
                &cell.glyph,
                m.cell_bounds(cell.row, cell.col, 1),
                cell.fg.hsla(),
                window,
            );
        }

        if let Some(cursor) = cursor.filter(|c| !c.filled) {
            let cell = m.cell_bounds(cursor.row, cursor.col, cursor.cols);
            let stroke = px(2.);
            match cursor.shape {
                CursorShape::Bar => {
                    window.paint_quad(fill(
                        Bounds::new(cell.origin, size(stroke, cell.size.height)),
                        cursor_colour,
                    ));
                }
                CursorShape::Underline => {
                    let y = cell.origin.y + cell.size.height - stroke;
                    window.paint_quad(fill(
                        Bounds::new(point(cell.origin.x, y), size(cell.size.width, stroke)),
                        cursor_colour,
                    ));
                }
                // An unfocused block: an outline, so the text stays readable.
                CursorShape::Block => paint_outline(cell, px(1.), cursor_colour, window),
            }
        }
        let cache = std::mem::take(&mut prepared.cache);
        self.view.update(cx, |view, _| {
            view.put_row_cache(cache);
            if let Some(probe) = view.probe() {
                probe.borrow_mut().painted();
            }
        });
    }
}

/// Shape and paint one span at its cell, for a caller without a row cache.
pub(crate) fn paint_text(
    span: &TextSpan,
    m: &CellMetrics,
    font_size: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let line = shape_text(span, m.width, font_size, window);
    let _ = line.paint(
        m.cell_origin(span.row, span.col),
        m.height,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// Shape one span's text in its style (see the module docs for the forced
/// advance).
pub(crate) fn shape_text(
    span: &TextSpan,
    cell_width: Pixels,
    font_size: Pixels,
    window: &Window,
) -> ShapedLine {
    let mut font = mono_font();
    if span.bold {
        font = font.bold();
    }
    if span.italic {
        font = font.italic();
    }
    let color = span.fg.hsla();
    let run = TextRun {
        len: span.text.len(),
        font,
        color,
        background_color: None,
        underline: span.underline.then_some(UnderlineStyle {
            thickness: px(1.),
            color: Some(color),
            wavy: false,
        }),
        strikethrough: span.strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: Some(color),
        }),
    };
    // Force the cell advance only on multi-glyph ASCII spans; see the module docs.
    let force_width = (!span.wide && span.text.is_ascii()).then_some(cell_width);
    window.text_system().shape_line(
        SharedString::from(span.text.clone()),
        font_size,
        &[run],
        force_width,
    )
}

/// A 1-pixel-per-`stroke` rectangle outline inside `b`.
fn paint_outline(b: Bounds<Pixels>, stroke: Pixels, color: Hsla, window: &mut Window) {
    let (o, s) = (b.origin, b.size);
    window.paint_quad(fill(Bounds::new(o, size(s.width, stroke)), color));
    window.paint_quad(fill(
        Bounds::new(point(o.x, o.y + s.height - stroke), size(s.width, stroke)),
        color,
    ));
    window.paint_quad(fill(Bounds::new(o, size(stroke, s.height)), color));
    window.paint_quad(fill(
        Bounds::new(point(o.x + s.width - stroke, o.y), size(stroke, s.height)),
        color,
    ));
}

/// Paint a box-drawing / block glyph into cell `b`.
pub(crate) fn paint_box(glyph: &BoxGlyph, b: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let (x, y, w, h) = (b.origin.x, b.origin.y, b.size.width, b.size.height);
    // Stroke widths scale with the cell and are whole pixels, so parallel
    // borders look equally heavy.
    let light = (w / 8.).round().max(px(1.));
    let heavy = light * 2.;
    let stroke = |weight: Weight| match weight {
        Weight::None => px(0.),
        Weight::Light => light,
        Weight::Heavy => heavy,
    };
    // The centre lines, snapped so a 1px stroke is crisp.
    let cx = (x + w / 2.).floor();
    let cy = (y + h / 2.).floor();

    match glyph {
        BoxGlyph::Lines {
            up,
            down,
            left,
            right,
        } => {
            // Arms overlap the centre by the widest crossing stroke, so joints
            // are filled.
            let across_h = stroke(*up).max(stroke(*down));
            let across_v = stroke(*left).max(stroke(*right));
            let rect = |x0: Pixels, y0: Pixels, x1: Pixels, y1: Pixels| {
                Bounds::from_corners(point(x0, y0), point(x1, y1))
            };
            if *left != Weight::None {
                let t = stroke(*left);
                window.paint_quad(fill(
                    rect(x, cy - t / 2., cx + across_h / 2., cy + t / 2.),
                    color,
                ));
            }
            if *right != Weight::None {
                let t = stroke(*right);
                window.paint_quad(fill(
                    rect(cx - across_h / 2., cy - t / 2., x + w, cy + t / 2.),
                    color,
                ));
            }
            if *up != Weight::None {
                let t = stroke(*up);
                window.paint_quad(fill(
                    rect(cx - t / 2., y, cx + t / 2., cy + across_v / 2.),
                    color,
                ));
            }
            if *down != Weight::None {
                let t = stroke(*down);
                window.paint_quad(fill(
                    rect(cx - t / 2., cy - across_v / 2., cx + t / 2., y + h),
                    color,
                ));
            }
        }
        BoxGlyph::Rounded(corner) => {
            // Straight from one edge's midpoint toward the centre, a quarter
            // curve through the corner, straight on to the other edge.
            let (hx, vy) = match corner {
                Corner::DownRight => (x + w, y + h),
                Corner::DownLeft => (x, y + h),
                Corner::UpLeft => (x, y),
                Corner::UpRight => (x + w, y),
            };
            let r = (w / 2.).min(h / 2.);
            let toward = |from: Pixels, to: Pixels| if to > from { from + r } else { from - r };
            let mut path = PathBuilder::stroke(light);
            path.move_to(point(hx, cy));
            path.line_to(point(toward(cx, hx), cy));
            path.curve_to(point(cx, toward(cy, vy)), point(cx, cy));
            path.line_to(point(cx, vy));
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
        BoxGlyph::Blocks(rects) => {
            for (x0, y0, x1, y1) in rects {
                window.paint_quad(fill(
                    Bounds::from_corners(
                        point(x + w * *x0, y + h * *y0),
                        point(x + w * *x1, y + h * *y1),
                    ),
                    color,
                ));
            }
        }
        BoxGlyph::Shade(alpha) => {
            let mut shaded = color;
            shaded.a *= alpha;
            window.paint_quad(fill(b, shaded));
        }
    }
}
