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
//! messages — but the transcript is still reconstructed each tick from the
//! agent's session file via [`RemoteBridge::sync_transcript`], so a phone that
//! pairs later gets a populated history.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use crate::app::state::AppState;
use crate::remote::feed::{self, FeedState, SessionExtras, TurnTimer};
use crate::remote::notifier::{build_event, EventArming, EventClass, EventContext};
use crate::remote::shell::ShellManager;
use crate::remote::transcript::{StructuredPrompt, TranscriptBuilder};
use crate::remote::{RemoteInbound, RemoteOutbound};
use crate::tui::render::GitStatusCache;

use flightdeck_remote_protocol::{
    AgentStatus, CommandAck, CommandBody, DeepLink, DesktopToPhone, EventId, PairingId,
    PermissionChoice, PermissionOption, PhoneCommand, ProjectId, PromptId, PromptKind, Role,
    SessionId, StateSnapshot,
};

/// Seals E2E plaintext for the wire. Given the JSON plaintext plus the envelope
/// header the payload will travel under (`seq`, `sent_at_ms`), returns
/// `(nonce_b64, ciphertext_b64)` for a [`RemoteOutbound::SendEnvelope`], or
/// `None` to drop the message. `seq`/`sent_at_ms` are passed in because the real
/// AEAD binds them as additional authenticated data (spec §7.1): the sealer and
/// the envelope header must agree exactly, so the bridge assigns the outbound
/// `seq` here and hands the same value to the relay client.
/// [`passthrough_seal`] is the test/dev stand-in (no crypto, ignores the header).
pub type SealFn = Box<dyn Fn(&[u8], u64, i64) -> Option<(String, String)> + Send>;

/// Opens an inbound envelope: given the header (`seq`, `sender`, `sent_at_ms`)
/// and `(nonce_b64, ciphertext_b64)`, returns the JSON plaintext bytes, or
/// `None` if it cannot be opened (wrong key / tamper / bad header). The header
/// fields are the AAD the real AEAD authenticates (spec §7.1). Paired with
/// [`SealFn`]; [`passthrough_open`] is the test/dev stand-in.
pub type OpenFn = Box<dyn Fn(u64, Role, i64, &str, &str) -> Option<Vec<u8>> + Send>;

/// A no-crypto sealer: the plaintext is base64-encoded as the "ciphertext" with
/// an empty nonce. For local dev and tests only — the crypto task replaces it.
/// The `seq`/`sent_at_ms` header is ignored (there is no AAD to bind).
pub fn passthrough_seal() -> SealFn {
    Box::new(|plain: &[u8], _seq: u64, _sent_at_ms: i64| {
        Some((String::new(), STANDARD.encode(plain)))
    })
}

/// The inverse of [`passthrough_seal`]. Ignores the header fields.
pub fn passthrough_open() -> OpenFn {
    Box::new(
        |_seq: u64, _sender: Role, _sent_at_ms: i64, _nonce: &str, ciphertext: &str| {
            STANDARD.decode(ciphertext).ok()
        },
    )
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
    /// Remote-shell registry + outbound queue (PRD §5.4). Its messages are
    /// flushed through the sealed envelope path in [`Self::tick`].
    shells: ShellManager,
    seal: SealFn,
    open: OpenFn,
    /// The user's home directory, used to locate each session's agent JSONL for
    /// transcript reconstruction (remote-control-72k). `None` disables it (tests
    /// and any environment where the home dir is unknown), so the transcript
    /// simply stays empty rather than the bridge guessing a path.
    home: Option<std::path::PathBuf>,
    /// Highest outbound envelope `seq` this bridge has assigned. The next
    /// envelope uses `out_seq + 1` (envelopes start at 1). The bridge is the
    /// sole producer of outbound envelopes for a pairing, so it owns the counter
    /// and hands each assigned `seq` to the relay client, which persists it. On
    /// restart with an established pairing, [`Self::install_channel`] seeds this
    /// from the persisted high-water mark so the phone's dedup never stalls.
    out_seq: u64,
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
            shells: ShellManager::new(),
            seal,
            open,
            home: None,
            out_seq: 0,
        }
    }

    /// Set the home directory used to locate agent session files for transcript
    /// reconstruction (remote-control-72k). Called once at startup from `lib.rs`
    /// with the resolved user home; unset leaves transcripts empty.
    pub fn set_transcript_home(&mut self, home: Option<std::path::PathBuf>) {
        self.home = home;
    }

    /// A bridge using the no-crypto [`passthrough_seal`]/[`passthrough_open`].
    pub fn passthrough(grace_until_ms: u64) -> Self {
        Self::new(passthrough_seal(), passthrough_open(), grace_until_ms)
    }

    /// Swap in the real E2E sealer/opener once a pairing is established (spec
    /// §7.1), seeding the outbound `seq` counter from the persisted high-water
    /// mark (`resume_from_seq`; 0 for a fresh pairing). This is the moment E2E
    /// goes live on the desktop: `lib.rs` calls it at startup for an already
    /// established pairing, and at runtime the instant a phone claims. Accumulated
    /// transcript/feed state is preserved (only the crypto seam is replaced), so
    /// a phone that pairs mid-session still receives a populated history.
    pub fn install_channel(&mut self, seal: SealFn, open: OpenFn, resume_from_seq: u64) {
        self.seal = seal;
        self.open = open;
        // Floor, never regress: installing a channel for an *already-active*
        // pairing (a repeat `pairing_claimed`, or a mid-session re-derivation)
        // must not rewind the outbound counter below what we have already sent,
        // or the phone — which only reset its receive cursor on a genuine first
        // claim, not on a resume — would silently drop every "duplicate" seq and
        // the feed would stall (remote-control-bbf). A genuinely new pairing
        // resets `out_seq` to 0 in `handle_inbound` (on the pairing-id change) or
        // via `reset_to_passthrough` (on unpair), so the max here is 0-vs-0 there.
        self.out_seq = self.out_seq.max(resume_from_seq);
    }

    /// Revert to the no-crypto passthrough and forget the active pairing — used
    /// when the user unpairs, so the desktop stops sealing to a peer that is no
    /// longer trusted and is ready to pair afresh.
    pub fn reset_to_passthrough(&mut self) {
        self.seal = passthrough_seal();
        self.open = passthrough_open();
        self.out_seq = 0;
        self.pairing = None;
        // Forget remote shells; their backing child terminals stay as ordinary
        // desktop shells (the phone is no longer trusted to drive them).
        self.shells.clear();
    }

    /// The remote-shell registry (read-only), for the event loop's cap check.
    pub fn shells(&self) -> &ShellManager {
        &self.shells
    }

    /// Mutable access to the remote-shell registry so the event loop can open /
    /// close shells and register the child terminal it spawned. Outbound shell
    /// messages queued here are flushed (sealed) by [`Self::tick`].
    pub fn shells_mut(&mut self) -> &mut ShellManager {
        &mut self.shells
    }

    /// Tee a coalesced read of a child terminal's PTY bytes into the shell
    /// manager (a no-op unless that child backs the session's live remote
    /// shell). Called from the per-tick PTY drain; cheap and always safe.
    pub fn shell_pump(&mut self, session_id: &str, child_index: usize, bytes: &[u8]) {
        self.shells
            .pump(&SessionId::new(session_id), child_index, bytes);
    }

    /// Reconstruct a session's transcript from the agent's own conversation
    /// store, ingesting anything written since the last call. Cheap and always
    /// safe; builds history even before a phone pairs. A no-op when the home dir
    /// is unset or the agent has no locatable store (an OpenCode agent on Windows,
    /// an unknown agent, or before the agent has written its first record).
    /// Called each tick with the session's `agent` kind and absolute `worktree`.
    pub fn sync_transcript(&mut self, session_id: &str, agent: &str, worktree: &Path, now_ms: i64) {
        let Some(home) = self.home.clone() else {
            return;
        };
        let Some(source) = crate::remote::transcript::resolve_source(agent, worktree, &home) else {
            return;
        };
        let sid = SessionId::new(session_id);
        self.transcripts
            .entry(sid.clone())
            .or_insert_with(|| TranscriptBuilder::new(sid))
            .sync(&source, now_ms);
    }

    /// Handle one inbound relay event. Link/presence changes that mark a pairing
    /// active request a fresh snapshot; envelopes are opened and parsed. Data
    /// requests (snapshot / transcript) are serviced by the bridge; every other
    /// command is queued for the command-bridge task via
    /// [`Self::take_pending_commands`].
    pub fn handle_inbound(&mut self, msg: RemoteInbound) {
        match msg {
            RemoteInbound::Paired { pairing_id, .. }
            | RemoteInbound::PairingClaimed { pairing_id, .. } => {
                // Switching to a *different* pairing than the one we were feeding
                // means a new peer with a fresh receive cursor at 0 — restart the
                // outbound stream from seq 1. Re-confirming the SAME pairing (a
                // resume, or a repeat claim) must NOT rewind `out_seq`, so the
                // phone's resumed cursor keeps matching (remote-control-bbf).
                if self.pairing.is_some() && self.pairing.as_ref() != Some(&pairing_id) {
                    self.out_seq = 0;
                }
                self.pairing = Some(pairing_id);
                self.snapshot_needed = true;
            }
            // The relay lost our outbound seq watermark (restart/redeploy) and
            // rejected an envelope as non-monotonic. Restart this pairing's
            // outbound stream from seq 1 with a fresh full snapshot so a fresh
            // relay accepts it and the phone re-syncs (remote-control-bbf).
            RemoteInbound::SeqResync { pairing_id } => {
                if self.pairing.as_ref() == Some(&pairing_id) {
                    self.out_seq = 0;
                    self.snapshot_needed = true;
                }
            }
            // The offer (code shown) does not itself activate a pairing for the
            // outbound feed — the phone has not joined yet. Handled by the
            // pairing overlay, not the bridge.
            RemoteInbound::PairingOffered { .. } => {}
            RemoteInbound::Envelope(env) => {
                if self.pairing.is_none() {
                    self.pairing = Some(env.pairing_id.clone());
                }
                if let Some(plain) = (self.open)(
                    env.seq,
                    env.sender,
                    env.sent_at_ms,
                    &env.nonce,
                    &env.ciphertext,
                ) {
                    if let Ok(cmd) = serde_json::from_slice::<PhoneCommand>(&plain) {
                        self.route_command(cmd);
                    }
                }
            }
            RemoteInbound::Presence { .. } | RemoteInbound::Link(_) => {}
            // The relay no longer knows our pairing; the client dropped it and
            // will re-offer. Forget it here too and revert to the passthrough
            // sealer so we stop sealing to a dead channel (remote-control-1jy).
            RemoteInbound::PairingRejected { .. } => {
                self.pairing = None;
                self.reset_to_passthrough();
            }
            // The phone unpaired this Mac (spec §10.2). If it was the pairing we
            // were feeding, forget it and revert to the passthrough sealer so we
            // stop sealing to a dead channel; a different pairing is unaffected.
            RemoteInbound::PairingRevoked { pairing_id } => {
                if self.pairing.as_ref() == Some(&pairing_id) {
                    self.pairing = None;
                    self.reset_to_passthrough();
                }
            }
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

    /// The currently pending permission-prompt id for a session, if any (the
    /// most recently minted one). The command bridge validates a phone
    /// `permission_decision` against this so a stale decision is rejected
    /// instead of typed into the wrong prompt.
    pub fn pending_prompt_id(&self, session_id: &str) -> Option<PromptId> {
        self.transcripts
            .get(&SessionId::new(session_id))
            .and_then(|b| b.last_prompt_id())
    }

    /// Seal and enqueue a [`CommandAck`] on the outbound path (the command
    /// bridge acks every drained phone command with its actual outcome).
    /// `now_ms` stamps the envelope header the AEAD binds (spec §7.1).
    pub fn send_ack(&mut self, ack: CommandAck, now_ms: i64, send: &mut dyn FnMut(RemoteOutbound)) {
        self.send_msg(DesktopToPhone::CommandAck(ack), now_ms, send);
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

                // Reconstruct the transcript from the agent's session file. Done
                // here, before the pairing gate below, so a phone that pairs
                // later still receives the accumulated history (remote-control-72k).
                let worktree = pv.state.repo_root.join(&tab.meta.worktree_path_relative);
                self.sync_transcript(&tab.meta.id, &tab.meta.agent, &worktree, now_ms as i64);

                let ds = tab.display_status(now_ms);
                let interpreted = ds.interpreted;
                let status = feed::agent_status(ds);

                // Needs-input edge → capture preview + inline permission prompt.
                let was_needs = matches!(self.prev_status.get(&sid), Some(AgentStatus::NeedsInput));
                let now_needs = matches!(status, AgentStatus::NeedsInput);
                if now_needs && !was_needs {
                    // Only OpenCode writes the prompt sidecar (from its injected
                    // plugin). Read it BEFORE `on_needs_input` so a captured
                    // structured prompt supplants the binary fallback, then remove
                    // it so a stale prompt is never reused on a later edge. We only
                    // ever touch the file for OpenCode sessions.
                    let is_opencode = tab.meta.agent.eq_ignore_ascii_case("opencode");
                    let sidecar = if is_opencode {
                        read_prompt_sidecar(&worktree)
                    } else {
                        None
                    };
                    let builder = self
                        .transcripts
                        .entry(sid.clone())
                        .or_insert_with(|| TranscriptBuilder::new(sid.clone()));
                    if let Some(sp) = sidecar {
                        builder.set_structured_prompt(sp);
                    }
                    let preview = builder.on_needs_input(now_ms as i64);
                    self.previews.insert(sid.clone(), preview);
                    if is_opencode {
                        let _ = std::fs::remove_file(prompt_sidecar_path(&worktree));
                    }
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

        let sent_at = now_ms as i64;

        // Send typed events first (most urgent).
        for ev in events {
            self.send_msg(DesktopToPhone::Event(ev), sent_at, send);
        }

        // Build the current world and reconcile against what the phone saw.
        let snap = self.build_snapshot(projects, now_ms);
        let delta = self.feed.diff(&snap);
        if self.snapshot_needed || delta.set_changed {
            self.feed.record_snapshot(&snap);
            self.snapshot_needed = false;
            self.send_msg(DesktopToPhone::Snapshot(snap), sent_at, send);
            // Alongside a full snapshot, push each session's full git status
            // detail (design §5.5) built from the cached worktree status. This
            // is how the phone learns per-session git detail; there is no
            // dedicated request command, so a `request_snapshot` refreshes it.
            for pv in projects {
                for tab in pv.state.tabs.iter() {
                    let detail = feed::git_status_detail(
                        &SessionId::new(&tab.meta.id),
                        pv.cache.get(&tab.meta.id),
                        &tab.meta.branch,
                    );
                    self.send_msg(DesktopToPhone::GitStatus(detail), sent_at, send);
                }
            }
        } else {
            if !delta.status.is_empty() {
                self.send_msg(
                    DesktopToPhone::StatusUpdate(flightdeck_remote_protocol::StatusUpdate {
                        updates: delta.status,
                    }),
                    sent_at,
                    send,
                );
            }
            if !delta.rollups.is_empty() {
                self.send_msg(
                    DesktopToPhone::Rollup(flightdeck_remote_protocol::RollupUpdate {
                        projects: delta.rollups,
                    }),
                    sent_at,
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
            self.send_msg(DesktopToPhone::TranscriptAppend(feed), sent_at, send);
        }

        // Answer transcript requests. Always reply so the phone is never left
        // hanging: when no session file has been reconstructed for this session
        // (e.g. the agent has not written its log yet), send an empty full-load
        // feed rather than silently dropping the request.
        let requests = std::mem::take(&mut self.pending_transcript_requests);
        for (sid, from_index) in requests {
            let feed = match self.transcripts.get(&sid) {
                Some(builder) => builder.load(from_index),
                None => flightdeck_remote_protocol::TranscriptFeed {
                    session_id: sid.clone(),
                    from_index: from_index.unwrap_or(0),
                    replace: true,
                    items: Vec::new(),
                },
            };
            self.send_msg(DesktopToPhone::Transcript(feed), sent_at, send);
        }

        // Flush remote-shell output/lifecycle messages queued since the last
        // tick (by the PTY drain and the command bridge) through the sealed
        // envelope path — so shell traffic only leaves while paired.
        for msg in self.shells.take_outbound() {
            self.send_msg(msg, sent_at, send);
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
    /// Assigns the next gapless `seq` and stamps `sent_at_ms = now_ms`, sealing
    /// under that exact header (the AEAD binds it, spec §7.1) and handing the
    /// same values to the relay client so the wire envelope matches.
    fn send_msg(&mut self, msg: DesktopToPhone, now_ms: i64, send: &mut dyn FnMut(RemoteOutbound)) {
        let Some(pairing_id) = self.pairing.clone() else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(&msg) else {
            return;
        };
        let seq = self.out_seq + 1;
        if let Some((nonce, ciphertext)) = (self.seal)(&bytes, seq, now_ms) {
            self.out_seq = seq;
            crate::remote::debuglog::log(&format!(
                "bridge SEAL {} pairing={} seq={}",
                msg_kind(&msg),
                pairing_id.as_str(),
                seq
            ));
            send(RemoteOutbound::SendEnvelope {
                pairing_id,
                seq,
                sent_at_ms: now_ms,
                nonce,
                ciphertext,
            });
        }
    }
}

/// The OpenCode prompt sidecar, written by the injected plugin (see
/// [`crate::agents::setup`]) on a `question.asked`/`permission.asked` event.
/// The plugin normalizes OpenCode's (undocumented) `event.properties` into this
/// stable shape, so the reader only depends on `kind`/`text`/`options`.
///
/// EMPIRICAL ASSUMPTION: OpenCode's `event.properties` field names are
/// unverified. The plugin probes several likely names; if it cannot extract
/// options it writes an empty array and this reader returns `None` so the
/// bridge keeps the binary allow/deny fallback.
#[derive(serde::Deserialize)]
struct PromptSidecar {
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    options: Vec<PromptSidecarOption>,
}

#[derive(serde::Deserialize)]
struct PromptSidecarOption {
    #[serde(default)]
    label: String,
    #[serde(default)]
    description: Option<String>,
}

/// Path of the OpenCode prompt sidecar within a worktree (sibling of the
/// `agent-status` file the poller reads).
fn prompt_sidecar_path(worktree: &Path) -> PathBuf {
    worktree.join(".flightdeck").join("agent-prompt.json")
}

/// Classify a permission option's button label into the binary choice it maps
/// to, or `None` when the wording is not clearly allow-ish or deny-ish (in which
/// case the caller drops to the binary fallback — the safe default). Substring
/// matching is deliberate given the unverified OpenCode option schema.
fn classify_permission_choice(label: &str) -> Option<PermissionChoice> {
    const ALLOW: &[&str] = &[
        "allow", "yes", "accept", "approve", "grant", "always", "once", "ok",
    ];
    const DENY: &[&str] = &[
        "deny", "reject", "decline", "cancel", "never", "disallow", "no",
    ];
    let l = label.to_ascii_lowercase();
    if ALLOW.iter().any(|k| l.contains(k)) {
        Some(PermissionChoice::AllowOnce)
    } else if DENY.iter().any(|k| l.contains(k)) {
        Some(PermissionChoice::Deny)
    } else {
        None
    }
}

/// Read and parse the OpenCode prompt sidecar into a [`StructuredPrompt`], or
/// `None` (binary fallback) when the file is absent, malformed, or optionless.
///
/// - `kind == "question"` → [`PromptKind::Question`], `allow_free_text = true`,
///   options carry no binary choice (index/label/description only).
/// - `kind == "permission"` → [`PromptKind::Permission`], `allow_free_text =
///   false`; each option must classify to allow/deny — if any label is unclear
///   the whole structured prompt is abandoned in favour of the binary fallback.
fn read_prompt_sidecar(worktree: &Path) -> Option<StructuredPrompt> {
    let raw = std::fs::read_to_string(prompt_sidecar_path(worktree)).ok()?;
    let parsed: PromptSidecar = serde_json::from_str(&raw).ok()?;
    if parsed.options.is_empty() {
        return None;
    }
    match parsed.kind.as_str() {
        "question" => {
            let options = parsed
                .options
                .into_iter()
                .enumerate()
                .map(|(i, o)| PermissionOption {
                    index: i as u32,
                    choice: None,
                    label: o.label,
                    description: o.description,
                })
                .collect();
            Some(StructuredPrompt {
                kind: PromptKind::Question,
                command: parsed.text,
                options,
                allow_free_text: true,
            })
        }
        // Permissions are binary. Build a structured prompt only when every
        // option maps cleanly to allow/deny; otherwise fall back to binary.
        "permission" => {
            let mut options = Vec::with_capacity(parsed.options.len());
            for (i, o) in parsed.options.into_iter().enumerate() {
                let choice = classify_permission_choice(&o.label)?;
                options.push(PermissionOption {
                    index: i as u32,
                    choice: Some(choice),
                    label: o.label,
                    description: o.description,
                });
            }
            Some(StructuredPrompt {
                kind: PromptKind::Permission,
                command: parsed.text,
                options,
                allow_free_text: false,
            })
        }
        _ => None,
    }
}

/// A short label for a [`DesktopToPhone`] variant, for the diagnostic log.
fn msg_kind(msg: &DesktopToPhone) -> &'static str {
    match msg {
        DesktopToPhone::Snapshot(_) => "snapshot",
        DesktopToPhone::StatusUpdate(_) => "status_update",
        DesktopToPhone::Rollup(_) => "rollup",
        DesktopToPhone::Transcript(_) => "transcript",
        DesktopToPhone::TranscriptAppend(_) => "transcript_append",
        DesktopToPhone::Event(_) => "event",
        DesktopToPhone::GitStatus(_) => "git_status",
        DesktopToPhone::ShellOutput(_) => "shell_output",
        DesktopToPhone::ShellEvent(_) => "shell_event",
        DesktopToPhone::CommandAck(_) => "command_ack",
    }
}

#[cfg(test)]
mod tests;
