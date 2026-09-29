//! Semantic colour tokens for the desktop app.
//!
//! Every colour the GUI paints comes from here, named for its ROLE (a surface,
//! a hairline, body ink, a status) rather than its value, so views never carry
//! a hex literal and a second palette (light mode, an alternate accent) is a
//! new constructor rather than a hunt through the views. The values are the
//! dark "Warp Sidebar" direction from the design brief (A1 artboards).
//!
//! The palette is installed as a GPUI [`Global`]; views read it with
//! [`Palette::global`]. [`init`] also projects the handful of tokens that
//! gpui-component's own widgets read onto its `Theme`, so a stock button or
//! scrollbar dropped into a view sits on the same surfaces as ours.

use gpui::{App, Global, Hsla, Rgba};

/// A colour as the design brief states it: 24-bit `0xRRGGBB`.
///
/// Kept as the raw hex (not a GPUI `Hsla`) so the tokens are `const`, are
/// comparable in tests against the brief verbatim, and so the luminance maths
/// below works on exact sRGB values rather than on a lossy HSL round-trip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hex(pub u32);

impl Hex {
    /// The colour as GPUI's paint type, for `.bg()`, `.text_color()` and friends.
    pub fn hsla(self) -> Hsla {
        Rgba::from(self).into()
    }

    /// WCAG 2.x relative luminance in `0.0..=1.0` (sRGB, D65).
    ///
    /// Used by the tests to prove the status colours differ in LIGHTNESS and
    /// not only in hue — the distinction a colour-blind user, or a greyscale
    /// screenshot, still sees.
    pub fn relative_luminance(self) -> f64 {
        fn linear(channel: u32) -> f64 {
            let c = f64::from(channel & 0xff) / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        let r = linear(self.0 >> 16);
        let g = linear(self.0 >> 8);
        let b = linear(self.0);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    /// WCAG contrast ratio against `other`, always `>= 1.0`.
    pub fn contrast_ratio(self, other: Hex) -> f64 {
        let (a, b) = (self.relative_luminance(), other.relative_luminance());
        let (hi, lo) = if a >= b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }
}

impl From<Hex> for Rgba {
    fn from(hex: Hex) -> Self {
        gpui::rgb(hex.0)
    }
}

/// The GUI's semantic palette. Field docs say where each token is used, so a
/// new view can pick the right one without opening the mockup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    // --- surfaces, back to front ---------------------------------------------
    /// Window chrome: titlebar, git strip, status bar.
    pub surface_window: Hex,
    /// The agents sidebar.
    pub surface_sidebar: Hex,
    /// The terminal / main content well.
    pub surface_terminal: Hex,
    /// Raised: selected row, active tab, buttons.
    pub surface_raised: Hex,
    /// A selected row nested inside a raised one (terminal rows under an agent).
    pub surface_raised_nested: Hex,
    /// Text-input fill (the command field).
    pub surface_input: Hex,

    // --- lines ---------------------------------------------------------------
    /// 1px separators between regions.
    pub hairline: Hex,
    /// Control borders.
    pub border: Hex,
    /// Emphasised border (focused / hovered control).
    pub border_strong: Hex,

    // --- text ----------------------------------------------------------------
    /// Primary text.
    pub ink: Hex,
    /// Secondary text (row subtitles).
    pub ink_2: Hex,
    /// De-emphasised text: section headers, hints.
    pub muted: Hex,
    /// Least emphasis: meta text, idle glyphs.
    pub faint: Hex,
    /// Separator glyphs (`·`, `/`) between inline items.
    pub separator: Hex,
    /// Terminal foreground.
    pub terminal_ink: Hex,

    // --- terminal grid -------------------------------------------------------
    /// The ANSI 16 a terminal program picks from (`ESC[30-37m`, `90-97m`):
    /// 0-7 normal, 8-15 bright. Warm, to sit on `surface_terminal`; indexes
    /// 16-255 and truecolour are not themed.
    pub terminal_ansi: [Hex; 16],
    /// The terminal cursor (block fill / bar / outline).
    pub terminal_cursor: Hex,
    /// Text drawn on top of a block cursor.
    pub terminal_cursor_ink: Hex,
    /// Fill behind selected terminal text.
    pub terminal_selection: Hex,

    // --- accent + status -----------------------------------------------------
    /// The user-selectable accent (default pink; alternates in the brief).
    pub accent: Hex,
    /// An agent is running. Deliberately the accent, as in the mockup.
    pub status_working: Hex,
    /// An agent is waiting for the user (approval, input).
    pub status_attention: Hex,
    /// Fill behind an attention badge/count.
    pub status_attention_bg: Hex,
    /// An agent is alive but idle.
    pub status_idle: Hex,
    /// An agent finished.
    pub status_done: Hex,

    // --- mode pill (status bar) ------------------------------------------------
    /// TERMINAL mode pill fill.
    pub pill_terminal_bg: Hex,
    /// TERMINAL mode pill text.
    pub pill_terminal_ink: Hex,
}

impl Palette {
    /// The dark palette from the design brief, with the default pink accent.
    pub const fn dark() -> Self {
        let accent = Hex(0xd86fb8);
        Self {
            surface_window: Hex(0x1c1917),
            surface_sidebar: Hex(0x201c19),
            surface_terminal: Hex(0x161311),
            surface_raised: Hex(0x2b2622),
            surface_raised_nested: Hex(0x3a322d),
            surface_input: Hex(0x221e1b),

            hairline: Hex(0x2f2a26),
            border: Hex(0x3a3430),
            border_strong: Hex(0x453e39),

            ink: Hex(0xf2efe9),
            ink_2: Hex(0xcfc7bd),
            muted: Hex(0xa39b92),
            faint: Hex(0x8a8279),
            separator: Hex(0x5f5750),
            terminal_ink: Hex(0xe8e2d8),

            terminal_ansi: [
                Hex(0x3a3430), // black
                Hex(0xe06c6c), // red
                Hex(0x8fc97a), // green
                Hex(0xe5c07b), // yellow
                Hex(0x6fa8dc), // blue
                Hex(0xd86fb8), // magenta (the accent)
                Hex(0x6cc4c4), // cyan
                Hex(0xcfc7bd), // white
                Hex(0x6b635c), // bright black
                Hex(0xf08c8c), // bright red
                Hex(0xa9dc96), // bright green
                Hex(0xf0d39a), // bright yellow
                Hex(0x8fc0ec), // bright blue
                Hex(0xe89ad0), // bright magenta
                Hex(0x8edcdc), // bright cyan
                Hex(0xf2efe9), // bright white
            ],
            terminal_cursor: Hex(0xe8e2d8),
            terminal_cursor_ink: Hex(0x161311),
            terminal_selection: Hex(0x45505e),

            accent,
            status_working: accent,
            status_attention: Hex(0xf0b54a),
            status_attention_bg: Hex(0x4a3614),
            status_idle: Hex(0x8a8279),
            status_done: Hex(0x7cc47f),

            pill_terminal_bg: Hex(0x34402f),
            pill_terminal_ink: Hex(0xb9e2a6),
        }
    }

    /// The installed palette. Panics if [`init`] was not called, which is a
    /// start-up ordering bug rather than a runtime condition.
    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}

impl Global for Palette {}

/// Install the palette and align gpui-component's theme with it.
///
/// Must run after `gpui_component::init` (which installs the component theme
/// this edits) and before the first window opens.
pub fn init(cx: &mut App) {
    let palette = Palette::dark();
    cx.set_global(palette);

    // The app is dark-only until a light palette exists. Switching mode loads
    // gpui-component's registered dark theme, and that load replaces colours —
    // so the colour edits go in a SECOND update, as `Theme::update` documents.
    gpui_component::Theme::change(gpui_component::ThemeMode::Dark, None, cx);
    gpui_component::Theme::update(cx, |theme| {
        theme.background = palette.surface_window.hsla();
        theme.foreground = palette.ink.hsla();
        theme.muted_foreground = palette.muted.hsla();
        theme.border = palette.border.hsla();
        theme.title_bar = palette.surface_window.hsla();
        theme.title_bar_border = palette.hairline.hsla();
        theme.sidebar = palette.surface_sidebar.hsla();
        theme.primary = palette.ink.hsla();
        theme.primary_foreground = palette.surface_window.hsla();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimum contrast ratio between any two status colours. 1.1:1 is well
    /// short of text contrast, and deliberately so: the claim under test is
    /// only that no two statuses collapse to the SAME grey when hue is lost,
    /// which is what makes them tellable apart for a colour-blind user
    /// (the glyph shapes carry the rest).
    const MIN_STATUS_SEPARATION: f64 = 1.1;

    /// The four agent status colours, named for assertion messages.
    fn statuses(p: &Palette) -> [(&'static str, Hex); 4] {
        [
            ("working", p.status_working),
            ("attention", p.status_attention),
            ("idle", p.status_idle),
            ("done", p.status_done),
        ]
    }

    #[test]
    fn status_colours_differ_in_lightness_not_only_hue() {
        let palette = Palette::dark();
        let statuses = statuses(&palette);
        for (i, (name_a, a)) in statuses.iter().enumerate() {
            for (name_b, b) in &statuses[i + 1..] {
                let ratio = a.contrast_ratio(*b);
                assert!(
                    ratio >= MIN_STATUS_SEPARATION,
                    "{name_a} ({a:?}, L={:.3}) and {name_b} ({b:?}, L={:.3}) differ only \
                     in hue: contrast {ratio:.3} < {MIN_STATUS_SEPARATION}",
                    a.relative_luminance(),
                    b.relative_luminance(),
                );
            }
        }
    }

    #[test]
    fn status_glyphs_are_visible_on_the_sidebar() {
        // WCAG 1.4.11 asks 3:1 for meaningful non-text graphics; the status
        // glyphs sit on the sidebar surface (and on raised rows).
        let palette = Palette::dark();
        for surface in [palette.surface_sidebar, palette.surface_raised] {
            for (name, colour) in statuses(&palette) {
                let ratio = colour.contrast_ratio(surface);
                assert!(
                    ratio >= 3.0,
                    "{name} ({colour:?}) on {surface:?}: contrast {ratio:.2} < 3.0"
                );
            }
        }
    }

    #[test]
    fn body_ink_meets_text_contrast_on_every_surface() {
        // WCAG AA body text: 4.5:1.
        let p = Palette::dark();
        for surface in [
            p.surface_window,
            p.surface_sidebar,
            p.surface_terminal,
            p.surface_raised,
        ] {
            assert!(p.ink.contrast_ratio(surface) >= 4.5, "ink on {surface:?}");
            assert!(
                p.muted.contrast_ratio(surface) >= 4.5,
                "muted on {surface:?}"
            );
        }
    }

    #[test]
    fn terminal_ansi_colours_read_on_the_terminal_well() {
        // The coloured ANSI entries carry program output (errors, diffs, prompts)
        // so they must read as text; bright black is the "dim/comment" grey and
        // only needs the non-text 3:1. Black (0) is for backgrounds.
        let p = Palette::dark();
        for (i, colour) in p.terminal_ansi.iter().enumerate() {
            let ratio = colour.contrast_ratio(p.surface_terminal);
            let floor = match i {
                0 => continue,
                8 => 3.0,
                _ => 4.5,
            };
            assert!(
                ratio >= floor,
                "ANSI {i} ({colour:?}): {ratio:.2} < {floor}"
            );
        }
        assert!(
            p.terminal_ink.contrast_ratio(p.terminal_selection) >= 4.5,
            "selected text"
        );
        assert!(
            p.terminal_cursor_ink.contrast_ratio(p.terminal_cursor) >= 4.5,
            "text under a block cursor"
        );
    }

    #[test]
    fn relative_luminance_matches_reference_points() {
        assert!((Hex(0x000000).relative_luminance() - 0.0).abs() < 1e-9);
        assert!((Hex(0xffffff).relative_luminance() - 1.0).abs() < 1e-9);
        // Black on white is the textbook 21:1.
        assert!((Hex(0x000000).contrast_ratio(Hex(0xffffff)) - 21.0).abs() < 1e-9);
    }
}
