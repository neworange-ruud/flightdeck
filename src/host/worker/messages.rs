//! [`HostMessageLog`]: what the TUI would show in its status line, kept for
//! the worker's window (`specs/WORKER_PRD.md` §4.5).
//!
//! Messages go straight to an attached window. With none attached, the last
//! [`HOST_MESSAGES_KEPT`] are kept unread and replayed on the next attach; the
//! Workers list shows that there are unread messages.

use std::collections::VecDeque;

use crate::web::protocol::{HostMessage, MessageLevel};

/// How many messages a worker keeps while no window is attached.
pub const HOST_MESSAGES_KEPT: usize = 100;

/// A ring of recent host messages with an unread mark.
///
/// Ids are process-unique and increase, so "read" is one watermark rather than
/// a flag per message: everything at or below `read_up_to` has been seen.
#[derive(Debug)]
pub struct HostMessageLog {
    next_id: u64,
    ring: VecDeque<HostMessage>,
    read_up_to: u64,
}

impl Default for HostMessageLog {
    fn default() -> Self {
        HostMessageLog {
            next_id: 1,
            ring: VecDeque::new(),
            read_up_to: 0,
        }
    }
}

impl HostMessageLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Keep one message, unread, dropping the oldest past
    /// [`HOST_MESSAGES_KEPT`]; returns it as the wire carries it.
    pub fn push(
        &mut self,
        level: MessageLevel,
        text: impl Into<String>,
        at_ms: i64,
    ) -> HostMessage {
        let message = HostMessage {
            id: self.next_id,
            at_ms,
            level,
            text: text.into(),
        };
        self.next_id += 1;
        self.ring.push_back(message.clone());
        while self.ring.len() > HOST_MESSAGES_KEPT {
            self.ring.pop_front();
        }
        message
    }

    /// The kept messages no window has seen, oldest first.
    pub fn unread(&self) -> Vec<HostMessage> {
        self.ring
            .iter()
            .filter(|m| m.id > self.read_up_to)
            .cloned()
            .collect()
    }

    /// Mark everything read (a window attached and received them).
    pub fn mark_all_read(&mut self) {
        self.read_up_to = self.next_id - 1;
    }

    /// How many kept messages no window has seen.
    pub fn unread_count(&self) -> u32 {
        self.ring.iter().filter(|m| m.id > self.read_up_to).count() as u32
    }
}

/// The level a status-line sentence reads as: the TUI words its failures
/// "Error: …", "Could not …" or "… did not start", and its refusals
/// "Refused: …". Everything else is information.
pub fn level_of(text: &str) -> MessageLevel {
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("error")
        || lower.starts_with("could not")
        || lower.contains("did not start")
    {
        MessageLevel::Error
    } else if lower.starts_with("refused") {
        MessageLevel::Warning
    } else {
        MessageLevel::Info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_unread_until_marked_and_ids_increase() {
        let mut log = HostMessageLog::new();
        let a = log.push(MessageLevel::Info, "a", 1);
        let b = log.push(MessageLevel::Error, "b", 2);
        assert_eq!((a.id, b.id), (1, 2));
        assert_eq!(log.unread_count(), 2);
        assert_eq!(log.unread(), vec![a, b]);
        log.mark_all_read();
        assert_eq!(log.unread_count(), 0);
        assert!(log.unread().is_empty());
        let c = log.push(MessageLevel::Warning, "c", 3);
        assert_eq!(log.unread(), vec![c]);
    }

    #[test]
    fn the_ring_keeps_the_newest_hundred() {
        let mut log = HostMessageLog::new();
        for i in 0..(HOST_MESSAGES_KEPT as i64 + 5) {
            log.push(MessageLevel::Info, format!("m{i}"), i);
        }
        let unread = log.unread();
        assert_eq!(unread.len(), HOST_MESSAGES_KEPT);
        assert_eq!(unread[0].text, "m5");
        assert_eq!(log.unread_count(), HOST_MESSAGES_KEPT as u32);
    }

    #[test]
    fn levels_follow_the_status_line_wording() {
        assert_eq!(level_of("Error: push failed"), MessageLevel::Error);
        assert_eq!(level_of("Could not open project: x"), MessageLevel::Error);
        assert_eq!(
            level_of("Web interface did not start: busy"),
            MessageLevel::Error
        );
        assert_eq!(level_of("Refused: dirty tree"), MessageLevel::Warning);
        assert_eq!(level_of("Opened project 'x'."), MessageLevel::Info);
    }
}
