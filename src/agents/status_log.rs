//! Incremental reading of an agent's append-only status file (SPECS §24).
//!
//! An agent's hook/plugin appends one keyword line per lifecycle event to
//! `.flightdeck/agent-status`, and FlightDeck polls it on every host tick. A
//! [`StatusCursor`] remembers how far the file has been read, so a poll costs
//! one metadata call when nothing changed and reads only the appended bytes
//! when something did. Once everything is consumed and the file has grown past
//! [`STATUS_COMPACT_BYTES`], [`StatusCursor::compact`] empties it, so the file
//! stays small however long the agent runs.

use crate::contracts::{FileSystem, Result};
use std::path::Path;

/// Size past which a fully-read status file is emptied. One event is a short
/// keyword line, so this is thousands of events.
pub const STATUS_COMPACT_BYTES: u64 = 64 * 1024;

/// How many already-read bytes the cursor keeps to recognise its own file. A
/// read starts this far before the offset; when those bytes no longer match,
/// the file was rewritten (not appended to) and is read from the start.
const TAIL_LEN: usize = 16;

/// Read position in a status file: everything before `offset` is consumed.
/// The offset always sits just after a newline, so a line a hook is still
/// writing is never split.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusCursor {
    offset: u64,
    /// The last (up to [`TAIL_LEN`]) bytes before `offset`.
    tail: Vec<u8>,
}

impl StatusCursor {
    /// A cursor past every complete line of `content`, so the history already
    /// in the file is never replayed as fresh events.
    pub fn at_end_of(content: &[u8]) -> StatusCursor {
        let mut cursor = StatusCursor::default();
        cursor.consume(content);
        cursor
    }

    /// The lines appended since the last read, oldest first. An unchanged file
    /// costs only a size check. A file that shrank or no longer starts with
    /// what was read before (an agent relaunch rewrote it) is read whole.
    pub fn read_new(&mut self, fs: &dyn FileSystem, path: &Path) -> Result<Vec<String>> {
        let len = fs.file_len(path)?;
        if len == self.offset {
            return Ok(Vec::new());
        }
        if len < self.offset {
            *self = StatusCursor::default();
        }
        let start = self.offset - self.tail.len() as u64;
        let bytes = fs.read_from(path, start)?;
        let fresh = match bytes.strip_prefix(self.tail.as_slice()) {
            Some(rest) => rest.to_vec(),
            None => {
                *self = StatusCursor::default();
                fs.read_from(path, 0)?
            }
        };
        let complete = self.consume(&fresh);
        Ok(String::from_utf8_lossy(complete)
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// Empty the file once it is fully read and past [`STATUS_COMPACT_BYTES`].
    /// `Ok(true)` when it was emptied. A line appended since the last read
    /// changes the size, which makes the truncate refuse; the next poll reads
    /// that line and the next call tries again.
    pub fn compact(&mut self, fs: &dyn FileSystem, path: &Path) -> Result<bool> {
        if self.offset < STATUS_COMPACT_BYTES {
            return Ok(false);
        }
        let emptied = fs.truncate_if_len(path, self.offset)?;
        if emptied {
            *self = StatusCursor::default();
        }
        Ok(emptied)
    }

    /// Advance past the complete lines at the start of `fresh` and return them.
    fn consume<'a>(&mut self, fresh: &'a [u8]) -> &'a [u8] {
        let end = fresh.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
        let complete = &fresh[..end];
        self.offset += complete.len() as u64;
        self.tail.extend_from_slice(complete);
        let excess = self.tail.len().saturating_sub(TAIL_LEN);
        self.tail.drain(..excess);
        complete
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeFs;

    fn path() -> &'static Path {
        Path::new("/w/.flightdeck/agent-status")
    }

    #[test]
    fn existing_history_is_not_replayed() {
        let fs = FakeFs::new().with_file(path(), "working\nidle\n");
        let mut cursor = StatusCursor::at_end_of(b"working\nidle\n");
        assert!(cursor.read_new(&fs, path()).unwrap().is_empty());
    }

    #[test]
    fn reads_only_appended_lines_in_order() {
        let fs = FakeFs::new().with_file(path(), "idle\n");
        let mut cursor = StatusCursor::at_end_of(b"idle\n");
        fs.append_line(path(), "working").unwrap();
        fs.append_line(path(), "idle").unwrap();
        assert_eq!(cursor.read_new(&fs, path()).unwrap(), ["working", "idle"]);
        fs.append_line(path(), "waiting").unwrap();
        assert_eq!(cursor.read_new(&fs, path()).unwrap(), ["waiting"]);
    }

    #[test]
    fn a_half_written_line_waits_for_its_newline() {
        let fs = FakeFs::new().with_file(path(), "idle\nwork");
        let mut cursor = StatusCursor::at_end_of(b"idle\n");
        assert!(cursor.read_new(&fs, path()).unwrap().is_empty());
        fs.write(path(), "idle\nworking\n").unwrap();
        assert_eq!(cursor.read_new(&fs, path()).unwrap(), ["working"]);
    }

    #[test]
    fn a_rewritten_file_is_read_from_the_start() {
        let fs = FakeFs::new().with_file(path(), "working\nidle\n");
        let mut cursor = StatusCursor::at_end_of(b"working\nidle\n");
        // A relaunch rewrote the file with different, longer content.
        fs.write(path(), "idle\nworking\nwaiting\n").unwrap();
        assert_eq!(
            cursor.read_new(&fs, path()).unwrap(),
            ["idle", "working", "waiting"]
        );
    }

    #[test]
    fn a_shrunk_file_is_read_from_the_start() {
        let fs = FakeFs::new().with_file(path(), "working\nidle\n");
        let mut cursor = StatusCursor::at_end_of(b"working\nidle\n");
        fs.write(path(), "idle\n").unwrap();
        assert_eq!(cursor.read_new(&fs, path()).unwrap(), ["idle"]);
    }

    #[test]
    fn a_small_file_is_left_alone() {
        let fs = FakeFs::new().with_file(path(), "idle\n");
        let mut cursor = StatusCursor::at_end_of(b"idle\n");
        assert!(!cursor.compact(&fs, path()).unwrap());
        assert_eq!(fs.file_contents(path()).as_deref(), Some("idle\n"));
    }

    #[test]
    fn a_large_fully_read_file_is_emptied_and_reading_continues() {
        let history = "working\n".repeat(STATUS_COMPACT_BYTES as usize / 8 + 1);
        let fs = FakeFs::new().with_file(path(), &history);
        let mut cursor = StatusCursor::at_end_of(history.as_bytes());
        assert!(cursor.compact(&fs, path()).unwrap());
        assert_eq!(fs.file_contents(path()).as_deref(), Some(""));

        fs.append_line(path(), "idle").unwrap();
        assert_eq!(cursor.read_new(&fs, path()).unwrap(), ["idle"]);
    }

    #[test]
    fn compaction_refuses_while_an_unread_line_is_pending() {
        let history = "working\n".repeat(STATUS_COMPACT_BYTES as usize / 8 + 1);
        let fs = FakeFs::new().with_file(path(), &history);
        let mut cursor = StatusCursor::at_end_of(history.as_bytes());
        fs.append_line(path(), "idle").unwrap();

        assert!(!cursor.compact(&fs, path()).unwrap());
        assert_eq!(cursor.read_new(&fs, path()).unwrap(), ["idle"]);
        assert!(cursor.compact(&fs, path()).unwrap());
    }
}
