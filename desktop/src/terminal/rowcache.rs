//! Damage tracking for the terminal element (beads `remote-control-bmej.3.9`).
//!
//! The element used to lay out and shape every row on every frame. A
//! [`RowCache`] keeps each row's [`RowLayout`] (and, through `S`, whatever the
//! element derives from it, which is GPUI's shaped text) and redoes a row only
//! when something it depends on changed:
//!
//! - the emulator says the row changed ([`GridDamage`], from
//!   `TerminalGrid::take_damage`; alacritty_terminal tracks it natively and
//!   also reports the old and new cursor rows),
//! - the cursor moved onto or off the row, or changed shape or focus (the glyph
//!   under a filled block is inked differently), which is checked here too, so
//!   correctness never rests on the emulator's cursor bookkeeping alone,
//! - the grid size or the palette changed, or the cache is handed a different
//!   grid (the app switched to another agent's terminal): everything.
//!
//! The selection is not part of a row: it is an overlay the element builds
//! every frame from `layout::selection_spans`, which is cheap.
//!
//! `S` is generic so the cache logic is tested without a window; the element
//! uses `Vec<ShapedLine>`.

use flightdeck::terminal::grid::{GridDamage, GridView};

use super::layout::{self, RowCursor, RowLayout, TermPalette};

/// One cached row.
#[derive(Debug)]
pub struct CachedRow<S> {
    pub layout: RowLayout,
    /// What the element derived from `layout`; dropped whenever the row is
    /// laid out again, and by [`RowCache::invalidate_derived`].
    pub derived: Option<S>,
    /// The cursor this layout was made with.
    cursor: Option<RowCursor>,
}

/// Per-row layout cache for one terminal. See the module docs.
#[derive(Debug)]
pub struct RowCache<S> {
    rows: Vec<CachedRow<S>>,
    /// Which grid the rows came from (an opaque id the caller picks).
    source: Option<usize>,
    size: (u16, u16),
    palette: Option<TermPalette>,
    /// Rows laid out by the most recent [`RowCache::update`].
    pub last_laid_out: usize,
}

impl<S> Default for RowCache<S> {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            source: None,
            size: (0, 0),
            palette: None,
            last_laid_out: 0,
        }
    }
}

impl<S> RowCache<S> {
    /// Bring the cache up to date with `grid`, given the damage the emulator
    /// reported since the previous update. `source` names the grid: a
    /// different one than last time lays out everything. Returns the rows
    /// laid out.
    pub fn update(
        &mut self,
        source: usize,
        grid: &dyn GridView,
        damage: GridDamage,
        palette: &TermPalette,
        focused: bool,
    ) -> usize {
        let size = grid.size();
        let (rows, _) = size;
        let everything = damage == GridDamage::Full
            || self.source != Some(source)
            || self.size != size
            || self.palette.as_ref() != Some(palette)
            || self.rows.len() != usize::from(rows);
        if everything {
            self.rows = (0..rows)
                .map(|_| CachedRow {
                    layout: RowLayout::default(),
                    derived: None,
                    cursor: None,
                })
                .collect();
            self.size = size;
            self.source = Some(source);
            self.palette = Some(*palette);
        }
        let damaged = match &damage {
            GridDamage::Rows(rows) => rows.as_slice(),
            GridDamage::Full => &[],
        };
        let cursor = layout::cursor_placement(grid, focused);
        let mut laid_out = 0;
        for (row, cached) in (0..rows).zip(self.rows.iter_mut()) {
            let on_row = cursor.filter(|(r, _)| *r == row).map(|(_, c)| c);
            let dirty =
                everything || damaged.binary_search(&row).is_ok() || cached.cursor != on_row;
            if dirty {
                cached.layout = layout::layout_row(grid, row, on_row, palette);
                cached.cursor = on_row;
                cached.derived = None;
                laid_out += 1;
            }
        }
        self.last_laid_out = laid_out;
        laid_out
    }

    pub fn rows(&self) -> &[CachedRow<S>] {
        &self.rows
    }

    pub fn rows_mut(&mut self) -> &mut [CachedRow<S>] {
        &mut self.rows
    }

    /// Drop every row's derived data (the cell metrics changed, so shaped text
    /// no longer fits) while keeping the layouts.
    pub fn invalidate_derived(&mut self) {
        for row in &mut self.rows {
            row.derived = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Palette;
    use flightdeck::terminal::grid::{Emulator, TerminalGrid};

    fn palette() -> TermPalette {
        TermPalette::from_palette(&Palette::dark())
    }

    /// A grid, a cache, and a helper that runs one frame's update.
    struct Harness {
        grid: Box<dyn TerminalGrid>,
        cache: RowCache<u32>,
        palette: TermPalette,
    }

    impl Harness {
        fn new(rows: u16, cols: u16) -> Self {
            Self {
                grid: Emulator::Alacritty.build(rows, cols),
                cache: RowCache::default(),
                palette: palette(),
            }
        }

        fn frame(&mut self, focused: bool) -> usize {
            let damage = self.grid.take_damage();
            let n = self
                .cache
                .update(1, self.grid.as_ref(), damage, &self.palette, focused);
            // Mark every row's derived data, so a test sees which were dropped.
            for row in self.cache.rows_mut() {
                row.derived.get_or_insert(7);
            }
            n
        }

        /// The cached rows must always equal a fresh whole-frame layout.
        fn assert_matches_fresh_layout(&self, focused: bool) {
            let fresh = layout::layout(self.grid.as_ref(), None, &self.palette, focused);
            let mut texts = Vec::new();
            let mut backgrounds = Vec::new();
            let mut boxes = Vec::new();
            let mut cursor = None;
            for row in self.cache.rows() {
                texts.extend(row.layout.texts.iter().cloned());
                backgrounds.extend(row.layout.backgrounds.iter().cloned());
                boxes.extend(row.layout.boxes.iter().cloned());
                cursor = cursor.or(row.layout.cursor);
            }
            assert_eq!(texts, fresh.texts);
            assert_eq!(backgrounds, fresh.backgrounds);
            assert_eq!(boxes, fresh.boxes);
            assert_eq!(cursor, fresh.cursor);
        }
    }

    #[test]
    fn the_first_frame_lays_out_every_row_and_an_idle_one_only_the_cursor_row() {
        let mut h = Harness::new(10, 40);
        h.grid.process(b"one\r\ntwo\r\n$ ");
        assert_eq!(h.frame(true), 10);
        // Nothing changed: alacritty still names the cursor's row.
        assert_eq!(h.frame(true), 1);
        h.assert_matches_fresh_layout(true);
    }

    #[test]
    fn typing_relays_only_the_row_being_typed_on() {
        let mut h = Harness::new(10, 40);
        h.grid.process(b"$ ");
        h.frame(true);
        h.grid.process(b"e");
        assert_eq!(h.frame(true), 1);
        assert_eq!(h.cache.rows()[0].layout.texts[0].text, "$");
        assert!(h.cache.rows()[0].layout.texts.iter().any(|t| t.text == "e"));
        // Rows the frame did not touch keep their derived data.
        assert!(h.cache.rows()[5].derived.is_some());
        h.assert_matches_fresh_layout(true);
    }

    #[test]
    fn a_newline_relays_the_old_and_new_cursor_rows() {
        let mut h = Harness::new(10, 40);
        h.grid.process(b"$ ls");
        h.frame(true);
        h.grid.process(b"\r\nfile\r\n$ ");
        let laid = h.frame(true);
        assert!((3..=4).contains(&laid), "laid out {laid} rows");
        h.assert_matches_fresh_layout(true);
    }

    #[test]
    fn output_that_scrolls_relays_everything_and_stays_correct() {
        let mut h = Harness::new(4, 20);
        for i in 0..4 {
            h.grid.process(format!("line {i}\r\n").as_bytes());
        }
        h.frame(true);
        h.grid.process(b"more\r\nand more\r\n");
        assert_eq!(h.frame(true), 4);
        h.assert_matches_fresh_layout(true);
    }

    #[test]
    fn focus_changes_relay_the_cursor_row_even_without_damage() {
        let mut h = Harness::new(5, 20);
        h.grid.process(b"abc");
        h.frame(true);
        // Losing focus turns the filled block into an outline, which re-inks
        // the glyph under it; no byte arrived, so only the cursor check sees it.
        let damage = GridDamage::Rows(Vec::new());
        let laid = h
            .cache
            .update(1, h.grid.as_ref(), damage, &h.palette, false);
        assert_eq!(laid, 1);
        assert_eq!(
            h.cache.rows()[0].layout.cursor.map(|c| c.filled),
            Some(false)
        );
        h.assert_matches_fresh_layout(false);
    }

    #[test]
    fn scrolling_the_viewport_hides_the_cursor_and_relays_everything() {
        let mut h = Harness::new(3, 20);
        for i in 0..8 {
            h.grid.process(format!("l{i}\r\n").as_bytes());
        }
        h.frame(true);
        h.grid.set_scrollback(2);
        assert_eq!(h.frame(true), 3);
        assert!(h.cache.rows().iter().all(|r| r.layout.cursor.is_none()));
        h.assert_matches_fresh_layout(true);
    }

    #[test]
    fn resizing_or_a_new_palette_relays_everything() {
        let mut h = Harness::new(4, 20);
        h.grid.process(b"hi");
        h.frame(true);
        h.grid.resize(6, 30);
        assert_eq!(h.frame(true), 6);
        h.palette.fg = crate::theme::Hex(0x123456);
        let damage = GridDamage::Rows(Vec::new());
        assert_eq!(
            h.cache.update(1, h.grid.as_ref(), damage, &h.palette, true),
            6
        );
        h.assert_matches_fresh_layout(true);
    }

    #[test]
    fn a_different_grid_relays_everything_even_with_partial_damage() {
        let mut h = Harness::new(4, 20);
        h.grid.process(b"first");
        h.frame(true);
        // Another terminal comes on screen (the app switched agents): its
        // partial damage says nothing about rows the cache holds for the first.
        let mut other = Emulator::Alacritty.build(4, 20);
        other.process(b"second");
        let _ = other.take_damage();
        let damage = GridDamage::Rows(Vec::new());
        assert_eq!(
            h.cache.update(2, other.as_ref(), damage, &h.palette, true),
            4
        );
        assert_eq!(h.cache.rows()[0].layout.texts[0].text, "second");
    }

    #[test]
    fn invalidating_derived_data_keeps_the_layouts() {
        let mut h = Harness::new(3, 10);
        h.grid.process(b"x");
        h.frame(true);
        h.cache.invalidate_derived();
        assert!(h.cache.rows().iter().all(|r| r.derived.is_none()));
        assert_eq!(h.cache.rows()[0].layout.texts[0].text, "x");
    }
}
