//! Remote-shell session manager (PRD §5.4, minimal v1).
//!
//! The phone can open **one** shell per agent session. The desktop backs each
//! remote shell with an ordinary child terminal in that session's worktree —
//! spawned through the very same [`crate::terminal::session::Session`] machinery
//! the desktop uses for its own shells (the event loop in `src/lib.rs` performs
//! the spawn/write/close I/O; this module is pure state).
//!
//! This module owns:
//! * the registry `session -> live shell` (with the one-shell-per-session cap);
//! * the per-shell outbound chunk sequence;
//! * the pure chunking of raw PTY bytes into wire [`ShellOutput`] messages;
//! * the derivation of [`ShellEvent`]s (opened / exited / closed).
//!
//! It produces [`DesktopToPhone`] messages into an outbound queue that
//! [`crate::remote::bridge::RemoteBridge::tick`] drains, seals, and sends inside
//! the E2E envelope path — so shell traffic only ever leaves the machine while a
//! pairing is active, exactly like every other feed.
//!
//! A PTY multiplexes stdout and stderr onto one stream, so every chunk is
//! reported as [`ShellStream::Stdout`]; the `stderr` variant exists in the
//! protocol but the desktop never has a separated stderr to report here.

use std::collections::HashMap;

use flightdeck_remote_protocol::{
    DesktopToPhone, SessionId, ShellEvent, ShellEventKind, ShellId, ShellOutput, ShellStream,
};

/// Target size of one outbound shell-output chunk. Reads are coalesced per tick
/// (one PTY read per shell per tick) and then split so no single [`ShellOutput`]
/// carries more than this many bytes, keeping the relay envelopes small and the
/// phone's terminal responsive under a burst of output.
pub const SHELL_CHUNK_BYTES: usize = 4096;

/// Split `text` into pieces of at most `max` bytes each, never breaking a UTF-8
/// character. Returns borrowed slices in order; an empty input yields no pieces.
pub fn split_chunks(text: &str, max: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + max).min(text.len());
        // Back up to the nearest char boundary so we never split a codepoint.
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            // A single codepoint longer than `max` (only possible with a tiny
            // `max`); include the whole char rather than loop forever.
            end = (start + max).min(text.len());
            while end < text.len() && !text.is_char_boundary(end) {
                end += 1;
            }
        }
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

/// One live remote shell.
struct ShellState {
    /// The phone-generated shell id (echoed on every output/event).
    shell_id: ShellId,
    /// Index of the backing child terminal within the session's `Session`.
    child_index: usize,
    /// Next output chunk sequence (chunks start at 1).
    out_seq: u64,
    /// Whether the backing process has exited (drained no more; slot stays
    /// occupied until an explicit `shell_close`).
    exited: bool,
}

/// The registry of remote shells plus the outbound message queue.
#[derive(Default)]
pub struct ShellManager {
    shells: HashMap<SessionId, ShellState>,
    outbound: Vec<DesktopToPhone>,
}

impl ShellManager {
    /// An empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a shell slot is already taken for `session` (the one-shell cap).
    /// A shell whose process has exited still occupies the slot until the phone
    /// closes it, so a second open is refused until then.
    pub fn has_shell(&self, session: &SessionId) -> bool {
        self.shells.contains_key(session)
    }

    /// Register a freshly spawned shell and queue its `opened` event. The caller
    /// must have checked [`Self::has_shell`] first (the cap) and spawned the
    /// child terminal, passing its index here.
    pub fn opened(
        &mut self,
        session: SessionId,
        shell_id: ShellId,
        child_index: usize,
        cols: u16,
        rows: u16,
    ) {
        self.outbound.push(DesktopToPhone::ShellEvent(ShellEvent {
            session_id: session.clone(),
            shell_id: shell_id.clone(),
            kind: ShellEventKind::Opened { cols, rows },
        }));
        self.shells.insert(
            session,
            ShellState {
                shell_id,
                child_index,
                out_seq: 0,
                exited: false,
            },
        );
    }

    /// Whether `session` has a live shell with exactly this `shell_id`. Used to
    /// reject input/interrupt/close for a stale or unknown shell id.
    pub fn matches(&self, session: &SessionId, shell_id: &ShellId) -> bool {
        self.shells
            .get(session)
            .is_some_and(|s| &s.shell_id == shell_id)
    }

    /// The backing child-terminal index for `session`'s shell, if any.
    pub fn child_index(&self, session: &SessionId) -> Option<usize> {
        self.shells.get(session).map(|s| s.child_index)
    }

    /// Feed a coalesced read of raw PTY bytes for `session`'s child terminal at
    /// `child_index` into the outbound queue as one or more [`ShellOutput`]
    /// chunks. A no-op unless the session's live shell is backed by exactly that
    /// child and has not exited (so unrelated desktop child terminals of the
    /// same session are ignored).
    pub fn pump(&mut self, session: &SessionId, child_index: usize, bytes: &[u8]) {
        let Some(state) = self.shells.get_mut(session) else {
            return;
        };
        if state.exited || state.child_index != child_index || bytes.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(bytes);
        for piece in split_chunks(&text, SHELL_CHUNK_BYTES) {
            state.out_seq += 1;
            self.outbound.push(DesktopToPhone::ShellOutput(ShellOutput {
                session_id: session.clone(),
                shell_id: state.shell_id.clone(),
                stream: ShellStream::Stdout,
                seq: state.out_seq,
                data: piece.to_string(),
            }));
        }
    }

    /// Mark `session`'s shell (backed by `child_index`) as exited and queue the
    /// `exited` event once. A no-op if the shell is unknown, already exited, or
    /// backed by a different child.
    pub fn mark_exit(&mut self, session: &SessionId, child_index: usize, code: Option<i32>) {
        let Some(state) = self.shells.get_mut(session) else {
            return;
        };
        if state.exited || state.child_index != child_index {
            return;
        }
        state.exited = true;
        self.outbound.push(DesktopToPhone::ShellEvent(ShellEvent {
            session_id: session.clone(),
            shell_id: state.shell_id.clone(),
            kind: ShellEventKind::Exited { code },
        }));
    }

    /// Close `session`'s shell if its id matches, queueing the `closed` event and
    /// returning the backing child-terminal index so the caller can terminate and
    /// remove it. Returns `None` (no event) when there is no matching shell.
    pub fn close(&mut self, session: &SessionId, shell_id: &ShellId) -> Option<usize> {
        if !self.matches(session, shell_id) {
            return None;
        }
        let state = self.shells.remove(session)?;
        self.outbound.push(DesktopToPhone::ShellEvent(ShellEvent {
            session_id: session.clone(),
            shell_id: state.shell_id,
            kind: ShellEventKind::Closed,
        }));
        Some(state.child_index)
    }

    /// The `(session, child_index)` of every shell still awaiting exit, for the
    /// per-tick process-state poll.
    pub fn active_shells(&self) -> Vec<(SessionId, usize)> {
        self.shells
            .iter()
            .filter(|(_, s)| !s.exited)
            .map(|(sid, s)| (sid.clone(), s.child_index))
            .collect()
    }

    /// Drain the queued outbound messages for the bridge to seal and send.
    pub fn take_outbound(&mut self) -> Vec<DesktopToPhone> {
        std::mem::take(&mut self.outbound)
    }

    /// Forget every shell and queued message (used on unpair — the backing child
    /// terminals are left as ordinary desktop shells).
    pub fn clear(&mut self) {
        self.shells.clear();
        self.outbound.clear();
    }
}

#[cfg(test)]
mod tests;
