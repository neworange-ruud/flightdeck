//! [`HostMessageLog`]: what the TUI would show in its status line, kept for
//! the worker's window (`specs/WORKER_PRD.md` §4.5).
//!
//! Messages go straight to an attached window. With none attached, the last
//! [`HOST_MESSAGES_KEPT`] are kept unread and replayed on the next attach; the
//! Workers list shows that there are unread messages.

// stub: filled by C (`push(level, text, at_ms) -> HostMessage` and
// `unread() -> Vec<HostMessage>` need protocol v7's `HostMessage` and
// `MessageLevel`, to be wired after P0a merges)

/// How many messages a worker keeps while no window is attached.
pub const HOST_MESSAGES_KEPT: usize = 100;

/// A ring of recent host messages with an unread mark.
#[derive(Debug, Default)]
pub struct HostMessageLog {
    // stub: filled by C (next_id, ring: VecDeque<HostMessage>, read_up_to)
    _stub: (),
}

impl HostMessageLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark everything read (a window attached and received them).
    pub fn mark_all_read(&mut self) {
        // stub: filled by C
    }

    /// How many kept messages no window has seen.
    pub fn unread_count(&self) -> u32 {
        // stub: filled by C
        0
    }
}
