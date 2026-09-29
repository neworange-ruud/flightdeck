//! Box-drawing and block-element characters drawn as geometry, not glyphs.
//!
//! A font's `─` or `█` rarely fills the cell exactly: its line height, side
//! bearings and stroke weight are the font's, so borders show seams between
//! rows and joints that miss by a pixel. Drawing the common ones from the cell
//! rectangle makes every border meet its neighbours. Anything not listed here
//! (dashed lines, doubles, mixed weights, diagonals) falls back to the font.
//!
//! This module is pure: it only says WHAT to draw, in fractions of a cell. The
//! element turns that into pixel rectangles and paths.

/// Stroke weight of one arm of a line-drawing character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Weight {
    #[default]
    None,
    Light,
    Heavy,
}

/// Which way a rounded corner turns (named by the two arms it joins).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    /// `╭`
    DownRight,
    /// `╮`
    DownLeft,
    /// `╯`
    UpLeft,
    /// `╰`
    UpRight,
}

/// A rectangle in fractions of the cell: `(x0, y0, x1, y1)`, origin top-left.
pub type Frac = (f32, f32, f32, f32);

/// What to draw for one cell.
#[derive(Debug, Clone, PartialEq)]
pub enum BoxGlyph {
    /// Straight arms from the cell centre to its edges.
    Lines {
        up: Weight,
        down: Weight,
        left: Weight,
        right: Weight,
    },
    /// A light quarter-circle joining two edge midpoints.
    Rounded(Corner),
    /// Solid rectangles (block elements, quadrants).
    Blocks(Vec<Frac>),
    /// The whole cell at this opacity (`░ ▒ ▓`).
    Shade(f32),
}

/// The drawing for `ch`, or `None` to let the font draw it.
pub fn classify(ch: char) -> Option<BoxGlyph> {
    use Weight::{Heavy as H, Light as L, None as N};
    let lines = |up, down, left, right| {
        Some(BoxGlyph::Lines {
            up,
            down,
            left,
            right,
        })
    };
    let blocks = |rects: &[Frac]| Some(BoxGlyph::Blocks(rects.to_vec()));
    const UL: Frac = (0.0, 0.0, 0.5, 0.5);
    const UR: Frac = (0.5, 0.0, 1.0, 0.5);
    const LL: Frac = (0.0, 0.5, 0.5, 1.0);
    const LR: Frac = (0.5, 0.5, 1.0, 1.0);

    match ch {
        // --- lines: (up, down, left, right) ---------------------------------
        '─' => lines(N, N, L, L),
        '━' => lines(N, N, H, H),
        '│' => lines(L, L, N, N),
        '┃' => lines(H, H, N, N),
        '┌' => lines(N, L, N, L),
        '┏' => lines(N, H, N, H),
        '┐' => lines(N, L, L, N),
        '┓' => lines(N, H, H, N),
        '└' => lines(L, N, N, L),
        '┗' => lines(H, N, N, H),
        '┘' => lines(L, N, L, N),
        '┛' => lines(H, N, H, N),
        '├' => lines(L, L, N, L),
        '┣' => lines(H, H, N, H),
        '┤' => lines(L, L, L, N),
        '┫' => lines(H, H, H, N),
        '┬' => lines(N, L, L, L),
        '┳' => lines(N, H, H, H),
        '┴' => lines(L, N, L, L),
        '┻' => lines(H, N, H, H),
        '┼' => lines(L, L, L, L),
        '╋' => lines(H, H, H, H),
        '╴' => lines(N, N, L, N),
        '╵' => lines(L, N, N, N),
        '╶' => lines(N, N, N, L),
        '╷' => lines(N, L, N, N),
        '╸' => lines(N, N, H, N),
        '╹' => lines(H, N, N, N),
        '╺' => lines(N, N, N, H),
        '╻' => lines(N, H, N, N),
        // --- rounded corners ------------------------------------------------
        '╭' => Some(BoxGlyph::Rounded(Corner::DownRight)),
        '╮' => Some(BoxGlyph::Rounded(Corner::DownLeft)),
        '╯' => Some(BoxGlyph::Rounded(Corner::UpLeft)),
        '╰' => Some(BoxGlyph::Rounded(Corner::UpRight)),
        // --- block elements -------------------------------------------------
        '▀' => blocks(&[(0.0, 0.0, 1.0, 0.5)]),
        '▔' => blocks(&[(0.0, 0.0, 1.0, 0.125)]),
        '▁'..='█' => {
            // U+2581..U+2588: lower 1/8 .. 8/8.
            let eighths = f32::from((ch as u32 - 0x2580) as u8);
            blocks(&[(0.0, 1.0 - eighths / 8.0, 1.0, 1.0)])
        }
        '▉'..='▏' => {
            // U+2589..U+258F: left 7/8 .. 1/8.
            let eighths = f32::from((0x2590 - ch as u32) as u8);
            blocks(&[(0.0, 0.0, eighths / 8.0, 1.0)])
        }
        '▐' => blocks(&[(0.5, 0.0, 1.0, 1.0)]),
        '▕' => blocks(&[(0.875, 0.0, 1.0, 1.0)]),
        '░' => Some(BoxGlyph::Shade(0.25)),
        '▒' => Some(BoxGlyph::Shade(0.5)),
        '▓' => Some(BoxGlyph::Shade(0.75)),
        // --- quadrants ------------------------------------------------------
        '▖' => blocks(&[LL]),
        '▗' => blocks(&[LR]),
        '▘' => blocks(&[UL]),
        '▙' => blocks(&[UL, LL, LR]),
        '▚' => blocks(&[UL, LR]),
        '▛' => blocks(&[UL, UR, LL]),
        '▜' => blocks(&[UL, UR, LR]),
        '▝' => blocks(&[UR]),
        '▞' => blocks(&[UR, LL]),
        '▟' => blocks(&[UR, LL, LR]),
        _ => None,
    }
}

/// The glyph for a cell's text, if it is exactly one drawable character.
pub fn classify_text(text: &str) -> Option<BoxGlyph> {
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    classify(ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn border_characters_are_drawn() {
        for ch in "┌─┐│└┘├┤┬┴┼╭╮╯╰━┃".chars() {
            assert!(classify(ch).is_some(), "{ch:?} should be geometry");
        }
        // Doubles and dashes still come from the font.
        for ch in "═║╔┄┆".chars() {
            assert!(
                classify(ch).is_none(),
                "{ch:?} should fall back to the font"
            );
        }
        assert!(classify('a').is_none());
    }

    #[test]
    fn a_corner_has_exactly_its_two_arms() {
        assert_eq!(
            classify('┌'),
            Some(BoxGlyph::Lines {
                up: Weight::None,
                down: Weight::Light,
                left: Weight::None,
                right: Weight::Light,
            })
        );
    }

    #[test]
    fn eighth_blocks_grow_from_the_bottom_or_left_edge() {
        assert_eq!(
            classify('▁'),
            Some(BoxGlyph::Blocks(vec![(0.0, 0.875, 1.0, 1.0)]))
        );
        assert_eq!(
            classify('█'),
            Some(BoxGlyph::Blocks(vec![(0.0, 0.0, 1.0, 1.0)]))
        );
        assert_eq!(
            classify('▏'),
            Some(BoxGlyph::Blocks(vec![(0.0, 0.0, 0.125, 1.0)]))
        );
        assert_eq!(
            classify('▉'),
            Some(BoxGlyph::Blocks(vec![(0.0, 0.0, 0.875, 1.0)]))
        );
    }

    #[test]
    fn only_single_characters_classify() {
        assert!(classify_text("─").is_some());
        assert!(classify_text("─\u{301}").is_none());
        assert!(classify_text("").is_none());
    }
}
