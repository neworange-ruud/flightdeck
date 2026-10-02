//! The held-input queue: keystrokes are queued, never dropped, never reordered
//! and never doubled across a reconnect (`specs/WEB_INTERFACE.md` turn 2 §5.1).
//!
//! The browser's rule, in Rust (`webui/src/wire/socket.ts`): every `Input` and
//! `Command` takes the next number from **one** counter; an `Input` stays held
//! until the host acks it; after a reconnect the host's
//! [`Snapshot::last_input_seq`](crate::web::protocol::Snapshot::last_input_seq)
//! says what it already applied, everything at or below it is dropped and the
//! rest is replayed in order before anything new is sent.
//!
//! Pure — no socket, no clock. [`super::link`] owns the socket and calls
//! [`InputQueue::on_snapshot`] *before* it forwards the snapshot, so a key
//! typed during the hand-over cannot overtake the replay (the host drops a seq
//! at or below its watermark, so an overtaken key would be lost, not late).

use std::collections::{BTreeSet, VecDeque};

use crate::web::protocol::{AckOutcome, ClientMsg, Command, Input, TerminalId};

/// One keystroke burst waiting for its ack.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    seq: u64,
    terminal_id: TerminalId,
    data: Vec<u8>,
}

/// The queue and its sequence counter.
#[derive(Debug, Default)]
pub struct InputQueue {
    seq: u64,
    held: VecDeque<Held>,
    /// Whether frames may go straight out: attached, and the replay after the
    /// last snapshot already sent.
    live: bool,
    /// Seqs minted for commands that have not been answered yet. An ack for one
    /// of these says nothing about keystrokes: the server answers some commands
    /// itself (`request_snapshot`), ahead of an input the host has not applied
    /// yet, so treating it as "everything up to here applied" would release a
    /// keystroke that may still be lost.
    commands: BTreeSet<u64>,
}

/// What an ack answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AckFor {
    /// A keystroke burst (or one this queue no longer holds).
    Input,
    /// A command this queue minted.
    Command,
}

impl InputQueue {
    /// An empty queue, not yet live.
    pub fn new() -> InputQueue {
        InputQueue::default()
    }

    /// Queue keystrokes for `terminal_id`. Returns the frame to send now when
    /// the link is live; otherwise the bytes wait for the next snapshot.
    pub fn push(&mut self, terminal_id: TerminalId, data: Vec<u8>) -> Option<ClientMsg> {
        self.seq += 1;
        let held = Held {
            seq: self.seq,
            terminal_id,
            data,
        };
        let frame = self.live.then(|| input_frame(&held));
        self.held.push_back(held);
        frame
    }

    /// A command frame with the next seq, or `None` when the link is down.
    /// Commands are not held: a command chosen while disconnected acted on a
    /// view that may no longer be true, so it is refused rather than replayed.
    pub fn command(&mut self, name: &str, args: Option<serde_json::Value>) -> Option<ClientMsg> {
        if !self.live {
            return None;
        }
        self.seq += 1;
        self.commands.insert(self.seq);
        Some(ClientMsg::Command(Command {
            seq: self.seq,
            name: name.to_string(),
            args,
        }))
    }

    /// The seq the last [`InputQueue::command`] or [`InputQueue::push`] took.
    pub fn last_seq(&self) -> u64 {
        self.seq
    }

    /// A snapshot arrived: drop what the host already applied, continue the
    /// counter past its watermark, go live, and return the replay in order.
    pub fn on_snapshot(&mut self, last_input_seq: u64) -> Vec<ClientMsg> {
        self.held.retain(|h| h.seq > last_input_seq);
        self.seq = self.seq.max(last_input_seq);
        self.live = true;
        self.held.iter().map(input_frame).collect()
    }

    /// The link dropped: hold everything until the next snapshot. Unanswered
    /// commands will never be answered on the new connection.
    pub fn on_disconnect(&mut self) {
        self.live = false;
        self.commands.clear();
    }

    /// An ack for `seq`. `Applied` releases it and everything before it (acks
    /// are in order); `Rejected`/`Ignored` releases that one burst only — the
    /// host has said it will never apply it, so replaying it would be wrong.
    pub fn on_ack(&mut self, seq: u64, outcome: AckOutcome) -> AckFor {
        if self.commands.remove(&seq) {
            return AckFor::Command;
        }
        match outcome {
            AckOutcome::Applied => self.held.retain(|h| h.seq > seq),
            AckOutcome::Rejected | AckOutcome::Ignored => self.held.retain(|h| h.seq != seq),
        }
        AckFor::Input
    }

    /// Forget a command the host answered with an error frame instead of an ack.
    pub fn forget_command(&mut self, seq: u64) -> bool {
        self.commands.remove(&seq)
    }

    /// How many bursts are waiting for an ack.
    pub fn held_len(&self) -> usize {
        self.held.len()
    }

    /// Whether frames go straight out.
    pub fn is_live(&self) -> bool {
        self.live
    }
}

fn input_frame(held: &Held) -> ClientMsg {
    ClientMsg::Input(Input {
        seq: held.seq,
        terminal_id: held.terminal_id.clone(),
        data: held.data.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seqs(frames: &[ClientMsg]) -> Vec<u64> {
        frames
            .iter()
            .map(|f| match f {
                ClientMsg::Input(i) => i.seq,
                ClientMsg::Command(c) => c.seq,
                _ => 0,
            })
            .collect()
    }

    #[test]
    fn keys_typed_offline_are_held_and_replayed_in_order() {
        let mut q = InputQueue::new();
        assert!(q.push("t".into(), b"a".to_vec()).is_none());
        assert!(q.push("t".into(), b"b".to_vec()).is_none());
        let replay = q.on_snapshot(0);
        assert_eq!(seqs(&replay), vec![1, 2]);
        assert!(q.is_live());
        assert!(q.push("t".into(), b"c".to_vec()).is_some());
    }

    #[test]
    fn the_snapshot_watermark_drops_what_the_host_already_applied() {
        let mut q = InputQueue::new();
        q.on_snapshot(0);
        for b in [b"a", b"b", b"c"] {
            q.push("t".into(), b.to_vec());
        }
        q.on_disconnect();
        // The host applied 1 and 2 before the link dropped; 3 was lost in flight.
        let replay = q.on_snapshot(2);
        assert_eq!(seqs(&replay), vec![3]);
        // Exactly once: a second snapshot after the ack replays nothing.
        q.on_ack(3, AckOutcome::Applied);
        assert!(q.on_snapshot(3).is_empty());
    }

    #[test]
    fn the_counter_continues_past_a_higher_watermark() {
        let mut q = InputQueue::new();
        q.on_snapshot(41);
        let ClientMsg::Input(input) = q.push("t".into(), b"x".to_vec()).unwrap() else {
            panic!("an input frame");
        };
        assert_eq!(input.seq, 42);
    }

    #[test]
    fn commands_share_the_counter_and_are_not_held() {
        let mut q = InputQueue::new();
        assert!(
            q.command("restart_agent", None).is_none(),
            "offline: refused"
        );
        q.on_snapshot(0);
        q.push("t".into(), b"a".to_vec());
        let cmd = q.command("restart_agent", None).unwrap();
        assert_eq!(seqs(&[cmd]), vec![2]);
        assert_eq!(q.held_len(), 1, "only the keystroke waits for an ack");
    }

    #[test]
    fn a_command_ack_never_releases_an_unacked_keystroke() {
        let mut q = InputQueue::new();
        q.on_snapshot(0);
        q.push("t".into(), b"a".to_vec());
        q.command("request_snapshot", None);
        assert_eq!(q.on_ack(2, AckOutcome::Applied), AckFor::Command);
        assert_eq!(q.held_len(), 1, "seq 1 is still waiting for its own ack");
        assert_eq!(q.on_ack(1, AckOutcome::Applied), AckFor::Input);
        assert_eq!(q.held_len(), 0);
    }

    #[test]
    fn a_rejected_burst_is_released_without_releasing_the_others() {
        let mut q = InputQueue::new();
        q.on_snapshot(0);
        q.push("t".into(), b"a".to_vec());
        q.push("t".into(), b"b".to_vec());
        q.on_ack(1, AckOutcome::Rejected);
        q.on_disconnect();
        assert_eq!(seqs(&q.on_snapshot(0)), vec![2]);
    }
}
