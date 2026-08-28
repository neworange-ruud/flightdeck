//! The input lock: which of several writers may type into the PTY right now
//! (`specs/WEB_INTERFACE.md` D14, as revised for multi-writer input).
//!
//! ## Why a lock at all
//!
//! D14 originally handed out **one** controlling seat precisely so that this
//! problem could be deferred. Lifting that restriction means two people can aim
//! keystrokes at the same PTY, and a terminal has exactly one cursor: if both
//! streams are delivered, the agent reads `helwolrold` and the user reports a
//! bug in the agent. Per-keystroke interleaving is therefore not an option, and
//! neither is line-level batching — D2 requires the raw PTY byte stream and
//! agents read raw keys, so there is no line to batch.
//!
//! What is left is a lock, and the only question is how it is taken and given
//! back. Asking a user to press a "claim input" button before typing is a lock
//! nobody would use; a lock released only by an explicit action is a lock
//! somebody forgets to release. So it is **claimed implicitly by typing and
//! released by idleness**:
//!
//! * The lock is free → the first writer to type takes it.
//! * The holder has been quiet for [`INPUT_LOCK_IDLE_MS`] → the next writer to
//!   type takes it from them.
//! * The holder is mid-burst → every other writer is **refused**, by name. Not
//!   queued for later delivery (which would splice half a word into the agent's
//!   prompt at an unpredictable moment) and not silently dropped (§5.1 forbids a
//!   keystroke vanishing without a trace). The refusal names the holder so the
//!   surface can say *who* is typing rather than merely that something failed.
//! * [`InputArbiter::preempt`] takes it now, and is reached only through an
//!   affordance a human confirmed — `SeatRequest::TakeOver` in the browser
//!   (artboard 2f), `Take Input Lock` in the desktop's palette.
//!
//! ## No surface has precedence
//!
//! [`Writer::Desktop`] is one writer among several and gets **no** privilege
//! here. A surface that can always cut in is exactly the corruption this module
//! removes, and an asymmetric rule is one more thing for a reader of the code —
//! and of the terminal — to get wrong. Symmetric rule, explicit override.
//!
//! ## Where it lives
//!
//! One [`InputArbiter`] behind one mutex, shared by the two threads that can
//! start a PTY write: the tokio task that owns a browser's socket
//! ([`crate::web::server`]) and the TUI thread that owns `AppState`
//! (`src/lib.rs`). Both call [`InputArbiter::claim`] *before* the bytes move, so
//! arbitration order is claim order and the PTY sees whole bursts. The lock
//! exists only while the web server is running: with no browser attached there
//! is exactly one writer and nothing to arbitrate.

use std::sync::{Arc, Mutex};

use crate::web::protocol::ViewerId;

/// How long a holder must be quiet before another writer may take the input
/// lock from it, in milliseconds.
///
/// **This number is the floor on how fast two people can alternate**, so it
/// wants to be short. 400 ms is chosen against three constraints, and it is the
/// smallest value that satisfies all of them:
///
/// 1. **It must be longer than a typist's gap between keystrokes**, or an
///    ordinary burst would be broken in the middle of a word and the other
///    surface would splice into it — the exact corruption this exists to
///    prevent. Fast typing runs 50–150 ms between keys and held-key autorepeat
///    is 30–50 ms; both sit comfortably inside 400 ms.
/// 2. **It must be longer than the host's own drain latency.** A browser's
///    keystrokes cross a channel and are written on the TUI's next render tick
///    (`POLL_TIMEOUT`, 50 ms). If the lock could move while the previous
///    holder's bytes were still queued for the PTY, the queue itself would
///    interleave them. 400 ms is eight ticks.
/// 3. **It must be short enough that a hand-off does not feel like a
///    negotiation.** 400 ms is about one relaxed inter-word pause: stop typing,
///    and the other person can start.
///
/// Its cost is stated where the decision is: two people alternating faster than
/// this hit the floor and are refused until it elapses.
pub const INPUT_LOCK_IDLE_MS: i64 = 400;

/// One surface that may type into a PTY.
///
/// The desktop is in this enum rather than beside it because it is genuinely one
/// of the writers, not the referee — see the module doc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Writer {
    /// The person at the machine, typing into the TUI.
    Desktop,
    /// One attached browser, by the id its socket was given.
    Viewer(ViewerId),
}

/// What one writer holds, and when it last used it.
#[derive(Clone, Debug)]
struct Held {
    who: Writer,
    /// The holder's chip label (`desktop`, `192.168.2.20 · Chrome on macOS`),
    /// already sanitised by the server. Carried here so that a refusal can name
    /// the holder without the refusing thread having to reach into the seat
    /// registry — and so the desktop's own status bar can render it.
    label: String,
    /// Host clock (unix ms) of this holder's most recent keystroke.
    last_ms: i64,
}

/// The answer to [`InputArbiter::claim`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claim {
    /// These bytes may be written. The claimant now holds the lock.
    Granted,
    /// Someone else is mid-burst. **Nothing was written.** The caller owes the
    /// writer an answer that names `label`, because "your keystroke did not
    /// arrive" without a reason is indistinguishable from a broken host.
    Refused {
        /// Who holds it.
        by: Writer,
        /// Their chip label, for the sentence the surface renders.
        label: String,
    },
}

/// Who may type, right now.
///
/// Shared across threads as a [`SharedInputLock`]; see the module doc for why
/// there is exactly one of these and why it only exists while the web server is
/// running.
#[derive(Debug, Default)]
pub struct InputArbiter {
    holder: Option<Held>,
    idle_ms: i64,
}

/// The arbiter as the server and the TUI both hold it.
pub type SharedInputLock = Arc<Mutex<InputArbiter>>;

impl InputArbiter {
    /// A free lock with the shipped idle window.
    pub fn new() -> InputArbiter {
        InputArbiter::with_idle_ms(INPUT_LOCK_IDLE_MS)
    }

    /// A free lock with a caller-chosen idle window. Tests use it to drive the
    /// hand-off without sleeping; nothing in the product does.
    pub fn with_idle_ms(idle_ms: i64) -> InputArbiter {
        InputArbiter {
            holder: None,
            idle_ms,
        }
    }

    /// A fresh [`SharedInputLock`].
    pub fn shared() -> SharedInputLock {
        Arc::new(Mutex::new(InputArbiter::new()))
    }

    /// Ask to type. `label` is how the holder will be named to everyone else.
    ///
    /// Granted when the lock is free, when `who` already holds it, or when the
    /// current holder has been quiet for at least the idle window. Otherwise
    /// refused, naming the holder — and **the caller must not write the bytes**.
    pub fn claim(&mut self, who: &Writer, label: &str, now_ms: i64) -> Claim {
        match &self.holder {
            Some(held)
                if &held.who != who && now_ms.saturating_sub(held.last_ms) < self.idle_ms =>
            {
                Claim::Refused {
                    by: held.who.clone(),
                    label: held.label.clone(),
                }
            }
            // Free, ours already, or theirs but gone quiet: take it and mark the
            // burst live. Re-labelling on every claim is deliberate — a browser
            // that re-attaches with a better self-description should not be
            // named by a stale label for as long as it keeps typing.
            _ => {
                self.holder = Some(Held {
                    who: who.clone(),
                    label: label.to_string(),
                    last_ms: now_ms,
                });
                Claim::Granted
            }
        }
    }

    /// Take the lock regardless of who holds it, because a human said so.
    ///
    /// This is the *only* way past a live burst, and it is never reachable from
    /// a keystroke: the browser gets here through 2f's confirmed `Take over`
    /// (`SeatRequest::TakeOver`) and the desktop through a palette command. It
    /// deliberately does not exist as a per-surface privilege — see the module
    /// doc.
    pub fn preempt(&mut self, who: &Writer, label: &str, now_ms: i64) {
        self.holder = Some(Held {
            who: who.clone(),
            label: label.to_string(),
            last_ms: now_ms,
        });
    }

    /// Drop a holder that has gone quiet, so the published holder is the truth
    /// rather than the last person to have typed.
    ///
    /// Returns whether anything changed, which is what the caller announces on.
    /// Expiry is not required for correctness — [`InputArbiter::claim`] steals
    /// an idle lock anyway — it exists so both surfaces can show *free* instead
    /// of naming somebody who stopped typing a minute ago.
    pub fn expire(&mut self, now_ms: i64) -> bool {
        let stale = self
            .holder
            .as_ref()
            .is_some_and(|held| now_ms.saturating_sub(held.last_ms) >= self.idle_ms);
        if stale {
            self.holder = None;
        }
        stale
    }

    /// Give the lock up because the writer has gone — a socket closed, or the
    /// web server stopped. A no-op when somebody else already holds it.
    pub fn release(&mut self, who: &Writer) {
        if self.holder.as_ref().is_some_and(|held| &held.who == who) {
            self.holder = None;
        }
    }

    /// Who holds the lock, or `None` when it is free.
    pub fn holder(&self) -> Option<&Writer> {
        self.holder.as_ref().map(|held| &held.who)
    }

    /// The holder's chip label, for the sentence a surface renders.
    pub fn holder_label(&self) -> Option<&str> {
        self.holder.as_ref().map(|held| held.label.as_str())
    }
}

#[cfg(test)]
mod tests;
