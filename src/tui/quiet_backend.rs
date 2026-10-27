//! A ratatui backend wrapper that writes nothing when nothing changed.
//!
//! The event loop draws every `POLL_TIMEOUT` (50 ms), changed or not, so time
//! derived text (spinners, elapsed times, message expiry) keeps moving without
//! every source having to report itself. ratatui already diffs the cell
//! buffer, but each `Terminal::draw` still ends in unconditional backend calls:
//! the crossterm backend resets colours even for an empty diff, and the cursor
//! is hidden, or shown and moved, every frame. An idle FlightDeck therefore
//! sent its terminal 20 identical writes a second.
//!
//! Most terminals shrug that off. Windows Terminal without a GPU (a VM, for
//! instance) repaints in software on every cursor write, and FlightDeck's
//! typing then lagged while a plain shell in the same window stayed instant.
//!
//! [`QuietBackend`] drops those no-op writes: an empty diff never reaches the
//! inner backend, and a cursor visibility or position that is already in
//! effect is not sent again. A frame that changes nothing writes no bytes.

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

/// Wraps a [`Backend`] and suppresses writes that would not change the screen.
///
/// What the wrapper remembers is only what it sent itself. A drawn cell moves
/// the real cursor, and a clear or appended lines may too, so each of those
/// forgets the position; the next `set_cursor_position` is then sent for real.
/// Something that changes the cursor behind the wrapper's back must build a
/// fresh backend, as re-initialising the terminal after `$EDITOR` does.
#[derive(Debug)]
pub struct QuietBackend<B> {
    inner: B,
    /// `Some(visible)` once a show or hide went out; `None` until then.
    cursor_visible: Option<bool>,
    /// Where the cursor was last put, while nothing has moved it since.
    cursor_position: Option<Position>,
}

impl<B> QuietBackend<B> {
    /// Wrap `inner`, knowing nothing yet about the cursor.
    pub fn new(inner: B) -> Self {
        Self {
            inner,
            cursor_visible: None,
            cursor_position: None,
        }
    }

    /// The wrapped backend.
    pub fn inner(&self) -> &B {
        &self.inner
    }

    fn set_visible(
        &mut self,
        visible: bool,
        send: impl FnOnce(&mut B) -> Result<(), B::Error>,
    ) -> Result<(), B::Error>
    where
        B: Backend,
    {
        if self.cursor_visible == Some(visible) {
            return Ok(());
        }
        send(&mut self.inner)?;
        self.cursor_visible = Some(visible);
        Ok(())
    }
}

impl<B: Backend> Backend for QuietBackend<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut content = content.peekable();
        if content.peek().is_none() {
            return Ok(());
        }
        self.cursor_position = None;
        self.inner.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.cursor_position = None;
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.set_visible(false, B::hide_cursor)
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.set_visible(true, B::show_cursor)
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let position = position.into();
        if self.cursor_position == Some(position) {
            return Ok(());
        }
        self.inner.set_cursor_position(position)?;
        self.cursor_position = Some(position);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.cursor_position = None;
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.cursor_position = None;
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::widgets::Paragraph;
    use ratatui::Terminal;

    /// A [`TestBackend`] that also logs every call that would write to a real
    /// terminal, so a test can assert that a quiet frame wrote nothing.
    struct Recording {
        screen: TestBackend,
        writes: Vec<String>,
    }

    impl Recording {
        fn new() -> Self {
            Self {
                screen: TestBackend::new(20, 4),
                writes: Vec::new(),
            }
        }
    }

    impl Backend for Recording {
        type Error = <TestBackend as Backend>::Error;

        fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            let cells: Vec<_> = content.collect();
            self.writes.push(format!("draw {}", cells.len()));
            self.screen.draw(cells.into_iter())
        }

        fn hide_cursor(&mut self) -> Result<(), Self::Error> {
            self.writes.push("hide".to_string());
            self.screen.hide_cursor()
        }

        fn show_cursor(&mut self) -> Result<(), Self::Error> {
            self.writes.push("show".to_string());
            self.screen.show_cursor()
        }

        fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
            self.screen.get_cursor_position()
        }

        fn set_cursor_position<P: Into<Position>>(
            &mut self,
            position: P,
        ) -> Result<(), Self::Error> {
            let position = position.into();
            self.writes
                .push(format!("move {},{}", position.x, position.y));
            self.screen.set_cursor_position(position)
        }

        fn clear(&mut self) -> Result<(), Self::Error> {
            self.writes.push("clear".to_string());
            self.screen.clear()
        }

        fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
            self.writes.push("clear region".to_string());
            self.screen.clear_region(clear_type)
        }

        fn size(&self) -> Result<Size, Self::Error> {
            self.screen.size()
        }

        fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
            self.screen.window_size()
        }

        fn flush(&mut self) -> Result<(), Self::Error> {
            self.screen.flush()
        }
    }

    fn terminal() -> Terminal<QuietBackend<Recording>> {
        Terminal::new(QuietBackend::new(Recording::new())).unwrap()
    }

    /// Draw `text` with the cursor at `cursor` (hidden when `None`) and return
    /// what reached the inner backend during that one frame.
    fn frame(
        terminal: &mut Terminal<QuietBackend<Recording>>,
        text: &str,
        cursor: Option<(u16, u16)>,
    ) -> Vec<String> {
        let before = terminal.backend().inner().writes.len();
        terminal
            .draw(|f| {
                f.render_widget(Paragraph::new(text), f.area());
                if let Some(c) = cursor {
                    f.set_cursor_position(c);
                }
            })
            .unwrap();
        terminal.backend().inner().writes[before..].to_vec()
    }

    #[test]
    fn an_unchanged_frame_writes_nothing() {
        let mut t = terminal();
        assert_eq!(
            frame(&mut t, "hello", Some((3, 1))),
            ["draw 5", "show", "move 3,1"]
        );
        assert!(frame(&mut t, "hello", Some((3, 1))).is_empty());
        assert!(frame(&mut t, "hello", Some((3, 1))).is_empty());
    }

    #[test]
    fn an_unchanged_frame_with_a_hidden_cursor_writes_nothing() {
        let mut t = terminal();
        assert_eq!(frame(&mut t, "hello", None), ["draw 5", "hide"]);
        assert!(frame(&mut t, "hello", None).is_empty());
    }

    #[test]
    fn a_moved_cursor_is_sent_without_redrawing_cells() {
        let mut t = terminal();
        frame(&mut t, "hello", Some((3, 1)));
        assert_eq!(frame(&mut t, "hello", Some((4, 1))), ["move 4,1"]);
    }

    #[test]
    fn a_visibility_change_is_sent() {
        let mut t = terminal();
        frame(&mut t, "hello", Some((3, 1)));
        assert_eq!(frame(&mut t, "hello", None), ["hide"]);
        // Shown again at the same spot: the position is still in effect.
        assert_eq!(frame(&mut t, "hello", Some((3, 1))), ["show"]);
    }

    #[test]
    fn drawn_cells_resend_the_cursor_position_even_when_it_is_unchanged() {
        // Writing a cell moves the real cursor, so the position the frame asks
        // for must go out again although it equals the last one sent.
        let mut t = terminal();
        frame(&mut t, "hello", Some((3, 1)));
        assert_eq!(frame(&mut t, "help!", Some((3, 1))), ["draw 2", "move 3,1"]);
    }

    #[test]
    fn a_clear_resends_the_cursor_position() {
        let mut t = terminal();
        frame(&mut t, "hello", Some((3, 1)));
        t.clear().unwrap();
        let writes = frame(&mut t, "hello", Some((3, 1)));
        assert_eq!(writes.last().map(String::as_str), Some("move 3,1"));
    }
}
