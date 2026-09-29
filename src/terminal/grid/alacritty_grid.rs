//! [`TerminalGrid`] over `alacritty_terminal`: the desktop app's emulator.
//!
//! Only `alacritty_terminal`'s emulation core is used (`Term` + the `vte`
//! processor). Its own PTY and event loop (`tty`, `event_loop`) are not: the
//! PTY stays behind [`crate::contracts::PtySession`] like every other terminal
//! in FlightDeck, so this grid is fed exactly the bytes the vt100 one would be.
//!
//! `Term` reports side effects (query replies, title changes, bells) through an
//! `EventListener`. The listener here only queues them; [`AlacrittyGrid`]
//! drains the queue after each parse and turns the ones that need an answer
//! into PTY replies, because answering a colour query needs the grid's colours,
//! which the listener cannot see.

use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape as AnsiCursorShape, NamedColor, Processor, Rgb, StdSyncHandler,
};

use super::{
    xterm_rgb, CellAttrs, CellWidth, CursorShape, GridCell, GridColor, GridCursor, GridDamage,
    GridView, MouseEncoding, MouseMode, TerminalGrid, TerminalModes,
};
use crate::tui::selection::Selection;

/// The viewport size in the shape `Term` wants.
struct Size {
    rows: usize,
    cols: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// `Term`'s event sink: queue everything, decide later (see the module docs).
#[derive(Clone, Default)]
struct Queue(Arc<Mutex<Vec<Event>>>);

impl EventListener for Queue {
    fn send_event(&self, event: Event) {
        if let Ok(mut events) = self.0.lock() {
            events.push(event);
        }
    }
}

/// An `alacritty_terminal` emulator behind [`TerminalGrid`].
pub struct AlacrittyGrid {
    term: Term<Queue>,
    processor: Processor<StdSyncHandler>,
    events: Queue,
    replies: Vec<u8>,
    selection: Option<Selection>,
    /// What the front-end paints the default foreground / background with, for
    /// answering OSC 10 / 11 queries (see [`TerminalGrid::set_default_colors`]).
    default_fg: Rgb,
    default_bg: Rgb,
    /// Damage history for [`TerminalGrid::damage_since`]: `Term`'s own damage
    /// is folded in after every change (see [`AlacrittyGrid::fold_damage`]),
    /// stamping each changed viewport row with the fold's sequence number.
    damage_seq: u64,
    /// The last fold that damaged everything.
    full_at: u64,
    /// Per viewport row, the last fold that damaged it.
    row_seq: Vec<u64>,
}

impl AlacrittyGrid {
    /// A blank `rows` x `cols` grid keeping `scrollback` history rows.
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        let events = Queue::default();
        let config = Config {
            scrolling_history: scrollback,
            ..Config::default()
        };
        let term = Term::new(config, &size(rows, cols), events.clone());
        let mut grid = Self {
            term,
            processor: Processor::new(),
            events,
            replies: Vec::new(),
            selection: None,
            // A neutral dark default until the front-end says otherwise.
            default_fg: Rgb {
                r: 0xe5,
                g: 0xe5,
                b: 0xe5,
            },
            default_bg: Rgb { r: 0, g: 0, b: 0 },
            // A new grid is fully damaged: the first read of anyone is Full.
            damage_seq: 1,
            full_at: 1,
            row_seq: vec![1; usize::from(rows.max(1))],
        };
        // Take in Term's own "everything is new", so the first change after
        // creation is reported as just that change.
        grid.fold_damage();
        grid
    }

    /// Move `Term`'s damage into the grid's own history and reset it. Called
    /// after every operation that can change what is visible, so readers
    /// never need `&mut` and never steal each other's damage.
    ///
    /// `Term` already covers the old and new cursor cells (and always the
    /// current one), marks everything on a scroll of the viewport, a resize or
    /// a mode that repaints the screen, and reports partial damage in viewport
    /// rows.
    fn fold_damage(&mut self) {
        let rows = self.term.screen_lines();
        self.damage_seq += 1;
        let seq = self.damage_seq;
        if self.row_seq.len() != rows {
            self.row_seq = vec![seq; rows];
            self.full_at = seq;
        }
        match self.term.damage() {
            TermDamage::Full => self.full_at = seq,
            TermDamage::Partial(lines) => {
                for bounds in lines {
                    if let Some(row) = self.row_seq.get_mut(bounds.line) {
                        *row = seq;
                    }
                }
            }
        }
        self.term.reset_damage();
    }

    /// Flush a synchronized update (`?2026`) whose timeout has passed. `Term`
    /// buffers everything between BSU and ESU so a program's frame lands at
    /// once; a program that never sends ESU must not freeze the screen.
    /// Returns whether it flushed one.
    fn flush_expired_sync(&mut self) -> bool {
        let expired = self
            .processor
            .sync_timeout()
            .sync_timeout()
            .is_some_and(|deadline| deadline <= std::time::Instant::now());
        if expired {
            self.processor.stop_sync(&mut self.term);
        }
        expired
    }

    /// Turn queued `Term` events into PTY replies.
    fn drain_events(&mut self) {
        let events = match self.events.0.lock() {
            Ok(mut events) => std::mem::take(&mut *events),
            Err(_) => return,
        };
        for event in events {
            match event {
                Event::PtyWrite(text) => self.replies.extend_from_slice(text.as_bytes()),
                Event::ColorRequest(index, format) => {
                    let rgb = self.resolve_palette(index);
                    self.replies.extend_from_slice(format(rgb).as_bytes());
                }
                // Title, bell, clipboard (OSC 52), pixel-size queries: nothing
                // in FlightDeck consumes them yet.
                _ => {}
            }
        }
    }

    /// The colour at `index` of Term's palette (0..=255 the indexed colours,
    /// 256 foreground, 257 background, 258 cursor): the program's own override
    /// if it set one (OSC 4/10/11/12), else the front-end's default.
    fn resolve_palette(&self, index: usize) -> Rgb {
        if let Some(rgb) = self.term.colors()[index] {
            return rgb;
        }
        match index {
            0..=255 => {
                let (r, g, b) = xterm_rgb(index as u8);
                Rgb { r, g, b }
            }
            i if i == NamedColor::Background as usize => self.default_bg,
            _ => self.default_fg,
        }
    }

    /// Map a cell colour, applying any palette override the program set so a
    /// front-end sees the colour the program meant.
    fn color(&self, c: AnsiColor) -> GridColor {
        let overridden =
            |index: usize| self.term.colors()[index].map(|Rgb { r, g, b }| GridColor::Rgb(r, g, b));
        match c {
            AnsiColor::Spec(Rgb { r, g, b }) => GridColor::Rgb(r, g, b),
            AnsiColor::Indexed(i) => overridden(i as usize).unwrap_or(GridColor::Indexed(i)),
            AnsiColor::Named(named) => {
                let index = named as usize;
                if index < 16 {
                    return overridden(index).unwrap_or(GridColor::Indexed(index as u8));
                }
                match named {
                    // `Term` only stores these after a renderer-side mapping;
                    // map them back to their base colour (the DIM flag already
                    // carries the dimming).
                    NamedColor::DimBlack
                    | NamedColor::DimRed
                    | NamedColor::DimGreen
                    | NamedColor::DimYellow
                    | NamedColor::DimBlue
                    | NamedColor::DimMagenta
                    | NamedColor::DimCyan
                    | NamedColor::DimWhite => {
                        GridColor::Indexed((index - NamedColor::DimBlack as usize) as u8)
                    }
                    NamedColor::Foreground
                    | NamedColor::BrightForeground
                    | NamedColor::DimForeground => {
                        overridden(NamedColor::Foreground as usize).unwrap_or(GridColor::Default)
                    }
                    NamedColor::Background => {
                        overridden(NamedColor::Background as usize).unwrap_or(GridColor::Default)
                    }
                    _ => GridColor::Default,
                }
            }
        }
    }
}

fn size(rows: u16, cols: u16) -> Size {
    // `Term` indexes `columns - 1` freely; a zero-sized grid would underflow.
    Size {
        rows: usize::from(rows.max(1)),
        cols: usize::from(cols.max(1)),
    }
}

impl TerminalGrid for AlacrittyGrid {
    fn process(&mut self, bytes: &[u8]) {
        let _ = self.flush_expired_sync();
        self.processor.advance(&mut self.term, bytes);
        self.drain_events();
        self.fold_damage();
    }

    fn tick(&mut self) {
        let flushed = self.flush_expired_sync();
        self.drain_events();
        // A tick changes the screen only by releasing a held update. Folding
        // otherwise would stamp the cursor's row (which Term always reports)
        // on every tick, so an idle terminal would never read as undamaged.
        if flushed {
            self.fold_damage();
        }
    }

    fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.selection = None;
        self.term.resize(size(rows, cols));
        self.fold_damage();
    }

    fn scrollback_len(&self) -> usize {
        self.term.grid().history_size()
    }

    fn set_scrollback(&mut self, rows: usize) {
        let target = rows.min(self.scrollback_len());
        let delta = target as i64 - self.scrollback() as i64;
        if delta != 0 {
            self.term.scroll_display(Scroll::Delta(delta as i32));
            self.fold_damage();
        }
    }

    fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    fn set_selection(&mut self, selection: Option<Selection>) {
        self.selection = selection;
    }

    fn damage_since(&self, since: u64) -> (GridDamage, u64) {
        let now = self.damage_seq;
        if since < self.full_at {
            return (GridDamage::Full, now);
        }
        let rows = (0..self.row_seq.len())
            .filter(|&row| self.row_seq[row] > since)
            .map(|row| row as u16)
            .collect();
        (GridDamage::Rows(rows), now)
    }

    fn holds_output(&self) -> bool {
        self.processor.sync_timeout().sync_timeout().is_some()
    }

    fn set_default_colors(&mut self, fg: (u8, u8, u8), bg: (u8, u8, u8)) {
        self.default_fg = Rgb {
            r: fg.0,
            g: fg.1,
            b: fg.2,
        };
        self.default_bg = Rgb {
            r: bg.0,
            g: bg.1,
            b: bg.2,
        };
    }
}

impl GridView for AlacrittyGrid {
    fn size(&self) -> (u16, u16) {
        let grid = self.term.grid();
        (grid.screen_lines() as u16, grid.columns() as u16)
    }

    fn visit_row(&self, row: u16, f: &mut dyn FnMut(u16, &GridCell<'_>)) {
        let grid = self.term.grid();
        if usize::from(row) >= grid.screen_lines() {
            return;
        }
        // Viewport row 0 is `Line(-display_offset)`: negative lines are history.
        let line = Line(i32::from(row) - grid.display_offset() as i32);
        let cells = &grid[line];
        let mut text = String::new();
        for col in 0..grid.columns() {
            let cell = &cells[Column(col)];
            let flags = cell.flags;
            let width = if flags.contains(Flags::WIDE_CHAR) {
                CellWidth::Wide
            } else if flags.contains(Flags::WIDE_CHAR_SPACER) {
                CellWidth::WideContinuation
            } else {
                CellWidth::Narrow
            };
            text.clear();
            // A spacer (either half of a wrapped or split wide character) and
            // concealed text (SGR 8) draw nothing; a tab keeps its `\t` in the
            // cell for copy-out and draws as a blank.
            let blank = flags.intersects(
                Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER | Flags::HIDDEN,
            ) || cell.c == '\t';
            if !blank {
                text.push(cell.c);
                if let Some(zerowidth) = cell.zerowidth() {
                    text.extend(zerowidth);
                }
            }
            f(
                col as u16,
                &GridCell {
                    text: &text,
                    fg: self.color(cell.fg),
                    bg: self.color(cell.bg),
                    attrs: CellAttrs {
                        bold: flags.contains(Flags::BOLD),
                        dim: flags.contains(Flags::DIM),
                        italic: flags.contains(Flags::ITALIC),
                        underline: flags.intersects(Flags::ALL_UNDERLINES),
                        inverse: flags.contains(Flags::INVERSE),
                        strikethrough: flags.contains(Flags::STRIKEOUT),
                    },
                    width,
                },
            );
        }
    }

    fn cursor(&self) -> GridCursor {
        let grid = self.term.grid();
        let mut point: Point = grid.cursor.point;
        // On the right half of a wide character the cursor belongs on its left.
        if grid[point].flags.contains(Flags::WIDE_CHAR_SPACER) && point.column.0 > 0 {
            point.column -= 1;
        }
        let style = self.term.cursor_style();
        let shape = match style.shape {
            AnsiCursorShape::Underline => CursorShape::Underline,
            AnsiCursorShape::Beam => CursorShape::Bar,
            AnsiCursorShape::Block | AnsiCursorShape::HollowBlock | AnsiCursorShape::Hidden => {
                CursorShape::Block
            }
        };
        GridCursor {
            row: point.line.0.max(0) as u16,
            col: point.column.0 as u16,
            shape,
            blinking: style.blinking,
            visible: self.term.mode().contains(TermMode::SHOW_CURSOR)
                && style.shape != AnsiCursorShape::Hidden,
        }
    }

    fn modes(&self) -> TerminalModes {
        let mode = *self.term.mode();
        let mouse_mode = if mode.contains(TermMode::MOUSE_MOTION) {
            MouseMode::AnyMotion
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            MouseMode::ButtonMotion
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseMode::PressRelease
        } else {
            MouseMode::None
        };
        let mouse_encoding = if mode.contains(TermMode::SGR_MOUSE) {
            MouseEncoding::Sgr
        } else if mode.contains(TermMode::UTF8_MOUSE) {
            MouseEncoding::Utf8
        } else {
            MouseEncoding::Default
        };
        TerminalModes {
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
            mouse_mode,
            mouse_encoding,
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            app_keypad: mode.contains(TermMode::APP_KEYPAD),
        }
    }

    fn scrollback(&self) -> usize {
        self.term.grid().display_offset()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One reader of a grid's damage, as a view keeps it.
    #[derive(Default)]
    struct Reader(u64);

    impl Reader {
        fn read(&mut self, grid: &AlacrittyGrid) -> GridDamage {
            let (damage, now) = grid.damage_since(self.0);
            self.0 = now;
            damage
        }

        fn rows(&mut self, grid: &AlacrittyGrid) -> Vec<u16> {
            match self.read(grid) {
                GridDamage::Rows(rows) => rows,
                GridDamage::Full => panic!("expected partial damage, got Full"),
            }
        }
    }

    #[test]
    fn damage_starts_full_then_names_only_the_rows_that_changed() {
        let mut grid = AlacrittyGrid::new(6, 20, 100);
        let mut r = Reader::default();
        assert_eq!(r.read(&grid), GridDamage::Full);
        // Nothing happened since: nothing to redo.
        assert_eq!(r.rows(&grid), Vec::<u16>::new());

        // Write on row 3 and leave the cursor on row 4: the old cursor row,
        // the written row and the new cursor row.
        grid.process(b"\x1b[4;1Hhello\x1b[5;1H");
        assert_eq!(r.rows(&grid), vec![0, 3, 4]);
        // An idle tick changes nothing.
        grid.tick();
        assert_eq!(r.rows(&grid), Vec::<u16>::new());
    }

    #[test]
    fn two_readers_each_see_every_change() {
        let mut grid = AlacrittyGrid::new(6, 20, 100);
        let (mut view, mut tile) = (Reader::default(), Reader::default());
        let _ = view.read(&grid);
        let _ = tile.read(&grid);
        grid.process(b"\x1b[3;1Hone");
        // The view reads first; the tile must still see row 2.
        assert!(view.rows(&grid).contains(&2));
        grid.process(b"\x1b[6;1Htwo");
        let tile_rows = tile.rows(&grid);
        assert!(
            tile_rows.contains(&2) && tile_rows.contains(&5),
            "{tile_rows:?}"
        );
        // The view already had row 2's text; it sees the new line (row 5) and
        // the row the cursor left (2 again), nothing else.
        assert_eq!(view.rows(&grid), vec![2, 5]);
    }

    #[test]
    fn scrolling_resizing_and_clearing_damage_everything() {
        let mut grid = AlacrittyGrid::new(4, 20, 100);
        let mut r = Reader::default();
        for i in 0..10 {
            grid.process(format!("line {i}\r\n").as_bytes());
        }
        let _ = r.read(&grid);
        grid.set_scrollback(2);
        assert_eq!(r.read(&grid), GridDamage::Full);
        grid.set_scrollback(0);
        let _ = r.read(&grid);

        grid.resize(5, 20);
        assert_eq!(r.read(&grid), GridDamage::Full);

        grid.process(b"\x1b[2J");
        assert_eq!(r.read(&grid), GridDamage::Full);
    }

    #[test]
    fn a_synchronized_update_is_held_until_it_ends() {
        let mut grid = AlacrittyGrid::new(3, 10, 100);
        assert!(!grid.holds_output());
        grid.process(b"\x1b[?2026hhidden");
        assert!(grid.holds_output());
        assert_eq!(grid.row_text(0, 0, 9), "");
        grid.process(b"\x1b[?2026l");
        assert!(!grid.holds_output());
        assert_eq!(grid.row_text(0, 0, 9), "hidden");
    }

    #[test]
    fn output_that_scrolls_the_screen_damages_every_row() {
        let mut grid = AlacrittyGrid::new(3, 10, 100);
        let mut r = Reader::default();
        grid.process(b"a\r\nb\r\nc");
        let _ = r.read(&grid);
        // A newline on the last row moves every visible line up one.
        grid.process(b"\r\nd");
        let damage = r.read(&grid);
        let all = damage == GridDamage::Full || damage == GridDamage::Rows(vec![0, 1, 2]);
        assert!(all, "a scroll must repaint every row, got {damage:?}");
    }
}
