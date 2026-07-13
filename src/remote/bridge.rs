//! Per-tick orchestration of the desktop → phone outbound feed.
//!
//! The [`RemoteBridge`] ties the pure feed builder ([`crate::remote::feed`]),
//! the transcript reconstruction ([`crate::remote::transcript`]) and the typed
//! event derivation ([`crate::remote::notifier`]) together. Once per render tick
//! the event loop calls [`RemoteBridge::tick`] with a read-only view of every
//! open project; the bridge:
//!
//! 1. detects per-session status edges (finish / needs-input / error) and emits
//!    typed [`AgentEvent`]s, honouring a startup grace window;
//! 2. captures the pending-question preview when an agent stops for input;
//! 3. builds the current [`StateSnapshot`] and diffs it against what the phone
//!    last saw, sending a full snapshot on (re)connect / request / structural
//!    change, or minimal [`StatusUpdate`]/[`RollupUpdate`] deltas otherwise;
//! 4. flushes any newly reconstructed transcript items as `TranscriptAppend`;
//! 5. answers `request_transcript`.
//!
//! Everything is serialized to JSON (the E2E *plaintext*) and handed to a
//! [`SealFn`] — the seam the crypto task plugs its `E2eChannel` into. Until then
//! a [`passthrough`] sealer (base64, no encryption) lets the whole path run and
//! be tested end to end. Sealed bytes leave as [`RemoteOutbound::SendEnvelope`].
//!
//! When no pairing is active the bridge does no sending and produces no
//! messages — but PTY bytes teed via [`RemoteBridge::tee_primary`] still build
//! the transcript, so a phone that pairs later gets a populated history.

use std::collections::HashMap;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use crate::app::state::AppState;
use crate::remote::feed::{self, FeedState, SessionExtras, TurnTimer};
use crate::remote::notifier::{build_event, EventArming, EventClass, EventContext};
use crate::remote::transcript::TranscriptBuilder;
use crate::remote::{RemoteInbound, RemoteOutbound};
use crate::tui::render::GitStatusCache;

use flightdeck_remote_protocol::{
    AgentStatus, CommandBody, DeepLink, DesktopToPhone, EventId, PairingId, PhoneCommand,
    ProjectId, SessionId, StateSnapshot,
};

/// Seals E2E plaintext for the wire. Given the JSON plaintext, returns
/// `(nonce_b64, ciphertext_b64)` for a [`RemoteOutbound::SendEnvelope`], or
/// `None` to drop the message. The crypto task supplies the real AEAD sealer;
/// [`passthrough_seal`] is the test/dev stand-in.
pub type SealFn = Box<dyn Fn(&[u8]) -> Option<(String, String)> + Send>;

/// Opens an inbound envelope: given `(nonce_b64, ciphertext_b64)`, returns the
/// JSON plaintext bytes, or `None` if it cannot be opened. Paired with
/// [`SealFn`]; [`passthrough_open`] is the test/dev stand-in.
pub type OpenFn = Box<dyn Fn(&str, &str) -> Option<Vec<u8>> + Send>;

/// A no-crypto sealer: the plaintext is base64-encoded as the "ciphertext" with
/// an empty nonce. For local dev and tests only — the crypto task replaces it.
pub fn passthrough_seal() -> SealFn {
    Box::new(|plain: &[u8]| Some((String::new(), STANDARD.encode(plain))))
}

/// The inverse of [`passthrough_seal`].
pub fn passthrough_open() -> OpenFn {
    Box::new(|_nonce: &str, ciphertext: &str| STANDARD.decode(ciphertext).ok())
}

/// A read-only view of one open project, passed into [`RemoteBridge::tick`].
pub struct ProjectView<'a> {
    /// Stable project id (derived from the project name by the caller).
    pub id: ProjectId,
    /// Display name.
    pub name: &'a str,
    /// The project's headless state (read-only).
    pub state: &'a AppState,
    /// The project's git-status cache (read-only).
    pub cache: &'a GitStatusCache,
}

/// Owns all outbound-feed state for the desktop side of one relay link.
pub struct RemoteBridge {
    feed: FeedState,
    transcripts: HashMap<SessionId, TranscriptBuilder>,
    timers: HashMap<SessionId, TurnTimer>,
    arming: HashMap<SessionId, EventArming>,
    previews: HashMap<SessionId, Option<String>>,
    prev_status: HashMap<SessionId, AgentStatus>,
    event_seq: u64,
    pairing: Option<PairingId>,
    snapshot_needed: bool,
    grace_until_ms: u64,
    pending_transcript_requests: Vec<(SessionId, Option<u64>)>,
    pending_commands: Vec<PhoneCommand>,
    seal: SealFn,
    open: OpenFn,
}

impl RemoteBridge {
    /// Build a bridge with an explicit sealer/opener and a startup grace window
    /// (events before `grace_until_ms` are tracked but not sent, matching the
    /// TUI's startup notification suppression).
    pub fn new(seal: SealFn, open: OpenFn, grace_until_ms: u64) -> Self {
        RemoteBridge {
            feed: FeedState::default(),
            transcripts: HashMap::new(),
            timers: HashMap::new(),
            arming: HashMap::new(),
            previews: HashMap::new(),
            prev_status: HashMap::new(),
            event_seq: 0,
            pairing: None,
            snapshot_needed: true,
            grace_until_ms,
            pending_transcript_requests: Vec::new(),
            pending_commands: Vec::new(),
            seal,
            open,
        }
    }

    /// A bridge using the no-crypto [`passthrough_seal`]/[`passthrough_open`].
    pub fn passthrough(grace_until_ms: u64) -> Self {
        Self::new(passthrough_seal(), passthrough_open(), grace_until_ms)
    }

    /// Tee a chunk of a session's primary-terminal PTY bytes into its transcript
    /// builder. Cheap and always safe to call; builds history even before a
    /// phone pairs.
    pub fn tee_primary(&mut self, session_id: &str, bytes: &[u8], at_ms: i64) {
        let sid = SessionId::new(session_id);
        self.transcripts
            .entry(sid.clone())
            .or_insert_with(|| TranscriptBuilder::new(sid))
            .push_bytes(bytes, at_ms);
    }

    /// Handle one inbound relay event. Link/presence changes that mark a pairing
    /// active request a fresh snapshot; envelopes are opened and parsed. Data
    /// requests (snapshot / transcript) are serviced by the bridge; every other
    /// command is queued for the command-bridge task via
    /// [`Self::take_pending_commands`].
    pub fn handle_inbound(&mut self, msg: RemoteInbound) {
        match msg {
            RemoteInbound::Paired { pairing_id, .. } => {
                self.pairing = Some(pairing_id);
                self.snapshot_needed = true;
            }
            RemoteInbound::Envelope(env) => {
                if self.pairing.is_none() {
                    self.pairing = Some(env.pairing_id.clone());
                }
                if let Some(plain) = (self.open)(&env.nonce, &env.ciphertext) {
                    if let Ok(cmd) = serde_json::from_slice::<PhoneCommand>(&plain) {
                        self.route_command(cmd);
                    }
                }
            }
            RemoteInbound::Presence { .. } | RemoteInbound::Link(_) => {}
        }
    }

    /// Route a parsed phone command: service data requests here; queue the rest.
    fn route_command(&mut self, cmd: PhoneCommand) {
        match &cmd.body {
            CommandBody::RequestSnapshot { .. } => {
                self.snapshot_needed = true;
            }
            CommandBody::RequestTranscript {
                session_id,
                from_index,
            } => {
                self.pending_transcript_requests
                    .push((session_id.clone(), *from_index));
            }
            _ => self.pending_commands.push(cmd),
        }
    }

    /// Drain commands the bridge did not service itself (for the command-bridge
    /// task). Idempotent acking and application live there.
    pub fn take_pending_commands(&mut self) -> Vec<PhoneCommand> {
        std::mem::take(&mut self.pending_commands)
    }

    /// Whether a phone pairing is currently active.
    pub fn is_paired(&self) -> bool {
        self.pairing.is_some()
    }

    /// The one-tick pass: derive events, build/diff state, flush transcript, and
    /// answer transcript requests. Sends via `send`. A no-op (beyond edge
    /// bookkeeping) when no pairing is active.
    pub fn tick(
        &mut self,
        projects: &[ProjectView<'_>],
        now_ms: u64,
        send: &mut dyn FnMut(RemoteOutbound),
    ) {
        // Pre-pass: per-session edge detection (events + needs-input preview).
        let mut events = Vec::new();
        for pv in projects {
            for tab in pv.state.tabs.iter() {
                let sid = SessionId::new(&tab.meta.id);
                let ds = tab.display_status(now_ms);
                let interpreted = ds.interpreted;
                let status = feed::agent_status(ds);

                // Needs-input edge → capture preview + inline permission prompt.
                let was_needs = matches!(self.prev_status.get(&sid), Some(AgentStatus::NeedsInput));
                let now_needs = matches!(status, AgentStatus::NeedsInput);
                if now_needs && !was_needs {
                    let builder = self
                        .transcripts
                        .entry(sid.clone())
                        .or_insert_with(|| TranscriptBuilder::new(sid.clone()));
                    let preview = builder.on_needs_input(now_ms as i64);
                    self.previews.insert(sid.clone(), preview);
                } else if !now_needs && was_needs {
                    self.previews.remove(&sid);
                }

                // Event edge (arming always advances; grace only gates sending).
                let arm = self.arming.entry(sid.clone()).or_default();
                if let Some(class) = arm.observe(interpreted) {
                    if now_ms >= self.grace_until_ms && self.pairing.is_some() {
                        events.push(self.make_event(class, pv, tab, &sid, now_ms));
                    }
                }

                self.prev_status.insert(sid.clone(), status);
            }
        }

        // Nothing to transmit without a pairing (state kept for the next pair).
        if self.pairing.is_none() {
            return;
        }

        // Send typed events first (most urgent).
        for ev in events {
            self.send_msg(DesktopToPhone::Event(ev), send);
        }

        // Build the current world and reconcile against what the phone saw.
        let snap = self.build_snapshot(projects, now_ms);
        let delta = self.feed.diff(&snap);
        if self.snapshot_needed || delta.set_changed {
            self.feed.record_snapshot(&snap);
            self.snapshot_needed = false;
            self.send_msg(DesktopToPhone::Snapshot(snap), send);
        } else {
            if !delta.status.is_empty() {
                self.send_msg(
                    DesktopToPhone::StatusUpdate(flightdeck_remote_protocol::StatusUpdate {
                        updates: delta.status,
                    }),
                    send,
                );
            }
            if !delta.rollups.is_empty() {
                self.send_msg(
                    DesktopToPhone::Rollup(flightdeck_remote_protocol::RollupUpdate {
                        projects: delta.rollups,
                    }),
                    send,
                );
            }
        }

        // Flush any newly reconstructed transcript items.
        let mut appends = Vec::new();
        for builder in self.transcripts.values_mut() {
            if let Some(feed) = builder.take_appended() {
                appends.push(feed);
            }
        }
        for feed in appends {
            self.send_msg(DesktopToPhone::TranscriptAppend(feed), send);
        }

        // Answer transcript requests.
        let requests = std::mem::take(&mut self.pending_transcript_requests);
        for (sid, from_index) in requests {
            if let Some(builder) = self.transcripts.get(&sid) {
                self.send_msg(DesktopToPhone::Transcript(builder.load(from_index)), send);
            }
        }
    }

    /// Build the full snapshot for the current world, folding in turn timing and
    /// pending-question previews.
    fn build_snapshot(&mut self, projects: &[ProjectView<'_>], now_ms: u64) -> StateSnapshot {
        let mut out = Vec::with_capacity(projects.len());
        for pv in projects {
            // Split the borrow: the extras closure needs `timers`/`previews`.
            let timers = &mut self.timers;
            let previews = &self.previews;
            let project = feed::build_project_state(
                &pv.id,
                pv.name,
                pv.state,
                pv.cache,
                now_ms,
                |tab_id, status| {
                    let sid = SessionId::new(tab_id);
                    let running_time_secs = timers
                        .entry(sid.clone())
                        .or_default()
                        .observe(status, now_ms);
                    let pending_question = if matches!(status, AgentStatus::NeedsInput) {
                        previews.get(&sid).cloned().flatten()
                    } else {
                        None
                    };
                    SessionExtras {
                        running_time_secs,
                        pending_question,
                    }
                },
            );
            out.push(project);
        }
        StateSnapshot {
            server_time_ms: now_ms as i64,
            projects: out,
        }
    }

    /// Assemble a typed [`AgentEvent`] for a settled edge.
    fn make_event(
        &mut self,
        class: EventClass,
        pv: &ProjectView<'_>,
        tab: &crate::app::state::RuntimeTab,
        sid: &SessionId,
        now_ms: u64,
    ) -> flightdeck_remote_protocol::AgentEvent {
        self.event_seq += 1;
        let event_id = EventId::new(format!("ev:{}", self.event_seq));
        let deep_link = DeepLink {
            project_id: pv.id.clone(),
            session_id: sid.clone(),
            item_id: None,
        };
        let ws = pv.cache.get(&tab.meta.id);
        let files_changed = ws.map(|s| s.changes.total()).unwrap_or(0);
        let ready_to_push = ws
            .map(|s| s.changes.is_empty() && s.ahead > 0)
            .unwrap_or(false);
        let ctx = EventContext {
            event_id,
            deep_link,
            occurred_at_ms: now_ms as i64,
            session_name: tab.meta.name.clone(),
            preview: self.previews.get(sid).cloned().flatten(),
            files_changed,
            ready_to_push,
            error_message: None,
        };
        build_event(class, ctx)
    }

    /// Seal a message and enqueue it as an outbound envelope for the pairing.
    fn send_msg(&self, msg: DesktopToPhone, send: &mut dyn FnMut(RemoteOutbound)) {
        let Some(pairing_id) = self.pairing.clone() else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(&msg) else {
            return;
        };
        if let Some((nonce, ciphertext)) = (self.seal)(&bytes) {
            send(RemoteOutbound::SendEnvelope {
                pairing_id,
                nonce,
                ciphertext,
            });
        }
    }
}

#[cfg(test)]
mod tests;
