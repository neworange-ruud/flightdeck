//! Remote terminals as [`PtySession`]s (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md`
//! §2.2).
//!
//! A [`StreamPty`] stands in for a PTY the client does not own: its output is
//! the bytes the host streamed for that terminal id, its input becomes
//! [`ClientMsg::Input`](crate::web::protocol::ClientMsg::Input) frames through
//! the held-input queue, and resizing is a no-op because the host owns PTY
//! geometry (D4). Wrapped in the core's terminal model, it lets a front-end
//! render a remote terminal with exactly the element it renders a local one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::contracts::{ProcessState, PtySession, PtySize, Result};
use crate::web::protocol::{TermBytes, TerminalId, TerminalView};

use super::link::Outbound;

/// One terminal's delivered-but-unread bytes and its lifecycle.
#[derive(Debug)]
struct Feed {
    pending: Vec<u8>,
    state: ProcessState,
    /// Bumped when the host's stream for this id started over (the host
    /// restarted, or the id was reused), so whoever renders it knows the bytes
    /// already parsed belong to a different stream and must be discarded.
    generation: u64,
    /// Bytes older than the host's replay ring were skipped to reach these.
    truncated: bool,
}

impl Default for Feed {
    fn default() -> Self {
        Feed {
            pending: Vec::new(),
            state: ProcessState::Running,
            generation: 0,
            truncated: false,
        }
    }
}

type SharedFeed = Arc<Mutex<Feed>>;

fn lock(feed: &SharedFeed) -> std::sync::MutexGuard<'_, Feed> {
    feed.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A remote terminal behind the [`PtySession`] seam.
pub struct StreamPty {
    terminal_id: TerminalId,
    feed: SharedFeed,
    outbound: Outbound,
}

impl StreamPty {
    /// The wire id this stands for.
    pub fn terminal_id(&self) -> &TerminalId {
        &self.terminal_id
    }

    /// See [`RemoteTerminals::generation`].
    pub fn generation(&self) -> u64 {
        lock(&self.feed).generation
    }
}

impl PtySession for StreamPty {
    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.outbound
            .input(self.terminal_id.clone(), bytes.to_vec());
        Ok(())
    }

    /// D4: the host owns PTY geometry; a viewer's size never resizes a PTY.
    fn resize(&mut self, _size: PtySize) -> Result<()> {
        Ok(())
    }

    fn try_read_output(&mut self) -> Result<Vec<u8>> {
        Ok(std::mem::take(&mut lock(&self.feed).pending))
    }

    fn send_ctrl_c(&mut self) -> Result<()> {
        self.write_input(&[0x03])
    }

    fn process_state(&self) -> ProcessState {
        lock(&self.feed).state
    }

    /// A remote terminal is ended by the host's own commands (close child,
    /// close session), never by killing a process tree from here.
    fn terminate_tree(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Every remote terminal's feed, keyed by wire id.
#[derive(Default)]
pub struct RemoteTerminals {
    feeds: HashMap<TerminalId, SharedFeed>,
}

impl RemoteTerminals {
    pub fn new() -> RemoteTerminals {
        RemoteTerminals::default()
    }

    /// Bytes the link delivered (already de-duplicated against the cursor).
    pub fn deliver(&mut self, frame: TermBytes) {
        let feed = self.feeds.entry(frame.terminal_id).or_default();
        let mut feed = lock(feed);
        feed.truncated |= frame.truncated;
        feed.pending.extend_from_slice(&frame.data);
    }

    /// The host's stream for `terminal_id` started over: drop what is pending
    /// and bump the generation.
    pub fn reset(&mut self, terminal_id: &TerminalId) {
        let feed = self.feeds.entry(terminal_id.clone()).or_default();
        let mut feed = lock(feed);
        feed.pending.clear();
        feed.truncated = false;
        feed.generation += 1;
    }

    /// A [`PtySession`] for `terminal_id`. Each call returns a handle onto the
    /// same feed, so a front-end that rebuilds its terminal keeps reading
    /// where the last one stopped.
    pub fn pty(&mut self, terminal_id: &TerminalId, outbound: &Outbound) -> StreamPty {
        let feed = self.feeds.entry(terminal_id.clone()).or_default().clone();
        StreamPty {
            terminal_id: terminal_id.clone(),
            feed,
            outbound: outbound.clone(),
        }
    }

    /// The stream generation of `terminal_id` (0 until its first reset).
    pub fn generation(&self, terminal_id: &TerminalId) -> u64 {
        self.feeds
            .get(terminal_id)
            .map_or(0, |f| lock(f).generation)
    }

    /// Whether bytes older than the host's ring were skipped for this terminal.
    pub fn truncated(&self, terminal_id: &TerminalId) -> bool {
        self.feeds
            .get(terminal_id)
            .is_some_and(|f| lock(f).truncated)
    }

    /// Mirror each terminal's lifecycle from the host's views, and forget
    /// feeds for terminals the host no longer has.
    pub fn sync<'a>(&mut self, views: impl IntoIterator<Item = &'a TerminalView>) {
        let mut seen = std::collections::HashSet::new();
        for view in views {
            seen.insert(view.terminal_id.clone());
            let feed = self.feeds.entry(view.terminal_id.clone()).or_default();
            lock(feed).state = if view.alive {
                ProcessState::Running
            } else {
                ProcessState::Exited(view.exit_code.unwrap_or(0))
            };
        }
        self.feeds.retain(|id, _| seen.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::client::link::Outbound;
    use crate::web::protocol::{ClientMsg, Geometry, TerminalRole};

    fn view(id: &str, alive: bool, exit_code: Option<i32>) -> TerminalView {
        TerminalView {
            terminal_id: id.into(),
            session_id: crate::contracts::TabId("tab".to_string()),
            role: TerminalRole::Primary,
            title: "agent".to_string(),
            geometry: Geometry { cols: 80, rows: 24 },
            byte_len: 0,
            replay_from: 0,
            alive,
            exit_code,
        }
    }

    #[test]
    fn delivered_bytes_are_read_once_through_the_pty_seam() {
        let (outbound, _rx) = Outbound::detached();
        let mut terminals = RemoteTerminals::new();
        let mut pty = terminals.pty(&"t".into(), &outbound);
        terminals.deliver(TermBytes::live("t".into(), 0, b"hello".to_vec()));
        assert_eq!(pty.try_read_output().unwrap(), b"hello");
        assert!(pty.try_read_output().unwrap().is_empty());
    }

    #[test]
    fn typing_becomes_an_input_frame_for_that_terminal() {
        let (outbound, mut rx) = Outbound::detached();
        outbound.go_live_for_test();
        let mut terminals = RemoteTerminals::new();
        let mut pty = terminals.pty(&"t".into(), &outbound);
        pty.write_input(b"ls\r").unwrap();
        let Some(ClientMsg::Input(input)) = rx.try_recv().ok().and_then(|o| o.into_frame()) else {
            panic!("an input frame went out");
        };
        assert_eq!(input.terminal_id.as_str(), "t");
        assert_eq!(input.data, b"ls\r");
    }

    #[test]
    fn lifecycle_follows_the_hosts_view_and_closed_terminals_are_forgotten() {
        let (outbound, _rx) = Outbound::detached();
        let mut terminals = RemoteTerminals::new();
        let pty = terminals.pty(&"t".into(), &outbound);
        terminals.sync([&view("t", false, Some(3))]);
        assert_eq!(pty.process_state(), ProcessState::Exited(3));
        terminals.sync(std::iter::empty());
        assert_eq!(terminals.generation(&"t".into()), 0);
        assert!(terminals.feeds.is_empty());
    }

    #[test]
    fn a_reset_discards_pending_bytes_and_bumps_the_generation() {
        let (outbound, _rx) = Outbound::detached();
        let mut terminals = RemoteTerminals::new();
        let pty = terminals.pty(&"t".into(), &outbound);
        terminals.deliver(TermBytes::live("t".into(), 0, b"old".to_vec()));
        terminals.reset(&"t".into());
        assert_eq!(pty.generation(), 1);
        let mut pty = pty;
        assert!(pty.try_read_output().unwrap().is_empty());
    }
}
