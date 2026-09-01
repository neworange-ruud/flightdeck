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
//! messages — but the transcript is still reconstructed from the agent's
//! session file via [`RemoteBridge::sync_transcript`], so a phone that pairs
//! later gets a populated history. While unpaired that reconstruction is
//! throttled to [`UNPAIRED_TRANSCRIPT_SYNC_INTERVAL_MS`] rather than run every
//! render tick — nobody is there yet to receive it (remote-control-0ef.13) —
//! but the instant a pairing becomes active the throttle is bypassed and a
//! sync runs unconditionally, so a late-pairing phone still gets full history
//! on that very tick.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use crate::agents::setup::{status_backend, StatusBackend};
use crate::app::state::AppState;
use crate::contracts::AgentDef;
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

/// How long to wait for a racing Claude `AskUserQuestion` to be ingested from
/// the JSONL before treating a needs-input edge as a real permission prompt and
/// synthesizing the binary allow/deny fallback (remote-control-qa1). The
/// PreToolUse hook flips the status to `waiting` at essentially the same instant
/// the tool_use is written, so the ingest normally wins within a tick or two;
/// this bound only delays a genuine permission prompt, never a question.
const PROMPT_SETTLE_MS: u64 = 750;

/// Cadence used to gate [`RemoteBridge::sync_transcript`] while no phone is
/// paired. Resolving the agent's JSONL path and stat/reading it incrementally
/// on every render tick is wasted filesystem work when there is no peer to
/// receive the reconstructed history (remote-control-0ef.13). Once a phone IS
/// paired the sync runs every tick as before (see the `is_paired()` check in
/// [`RemoteBridge::tick`]), and that same check unconditionally bypasses this
/// throttle on the tick a pairing becomes active, so a late-pairing phone
/// still receives full history immediately rather than waiting out the
/// window.
const UNPAIRED_TRANSCRIPT_SYNC_INTERVAL_MS: i64 = 3_000;

/// How long an outbound envelope may sit un-acknowledged by the phone before the
/// peer stops counting as *live* (remote-control-5qu).
///
/// The phone acks every envelope it opens and decodes, immediately, so a healthy
/// round trip is well under a second; a whole minute of total silence with
/// envelopes outstanding means the phone is not receiving them. It is measured on
/// the injected clock (`now_ms` from [`RemoteBridge::tick`]), never on wall time,
/// so it is testable without sleeping.
const PEER_ACK_TIMEOUT_MS: u64 = 60_000;

/// Hard bound on how far the outbound stream may run ahead of the peer's ack
/// cursor before the peer stops counting as live, regardless of the clock. The
/// relay holds ~1000 un-acked envelopes per pairing before it starts shedding
/// them, so tripping at half of that stops the desktop well before the relay has
/// to drop anything. This is the "never let `out_seq` run thousands of envelopes
/// ahead of `last_acked_by_peer`" guard from remote-control-5qu; the reported
/// case reached 33,000.
const MAX_UNACKED_ENVELOPES: u64 = 500;

/// How many consecutive [`RemoteInbound::SeqRealign`]s may excuse an un-acked
/// backlog before the ack deadline is allowed to stand anyway.
///
/// A realign proves the *relay* rejected our envelopes, so their silence is not
/// the phone's fault (remote-control-zv3) — but it must not become an unbounded
/// loophole: a stream that realigns forever without ever collecting an ack is
/// indistinguishable from a dead peer, and after this many attempts the safer
/// reading is "dark". Any genuine ack resets the count, and zv3's realign
/// converges on the very next envelope, so a healthy stream never spends more
/// than one.
const MAX_REALIGN_CREDITS: u32 = 3;

/// Whether a phone peer is attached to the active pairing **and** still proving
/// it by acknowledging what we send (remote-control-5qu).
///
/// The relay `pong`-driven link indicator measures the desktop↔relay hop only, so
/// it reads healthy while the phone is dark — which is exactly why the reported
/// failure ran for 17 days unnoticed. This is the peer-side half of the story,
/// derived from end-to-end ack evidence.
///
/// **No surface renders it today.** It is the readable name for the
/// `peer_present`/`peer_dark` pair that [`RemoteBridge::tick`] gates the
/// per-tick feed on, and it exists so that the state machine those two private
/// flags make up can be asserted as three named states rather than as two
/// booleans. If a desktop indicator for the peer hop is ever built, this is the
/// value it reads — but a doc that said it already did is what
/// `specs/WEB_INTERFACE.md` §6.5 R26 was written about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerLiveness {
    /// No phone is attached to the pairing (or there is no pairing at all): the
    /// relay has told us the peer is absent, or has never said it is present.
    NoPeer,
    /// A phone is attached and nothing contradicts it: either it has acked
    /// everything we sent, or the outstanding envelopes are still inside the ack
    /// window. Also the state when this relay does not forward peer acks at all
    /// (an older relay), where the desktop deliberately makes no claim.
    Live,
    /// The relay says a phone is attached, but it has not acknowledged our
    /// envelopes for [`PEER_ACK_TIMEOUT_MS`] (or is [`MAX_UNACKED_ENVELOPES`]
    /// behind, or the relay is shedding our queue). The link to the relay is up;
    /// the *peer* is dark. The per-tick feed is suspended until it proves
    /// otherwise.
    Dark,
}

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
    /// For Claude sessions: the clock-ms at which the session entered needs-input
    /// with no structured prompt yet, while we wait for a possibly-racing
    /// AskUserQuestion to be ingested from the JSONL before falling back to the
    /// binary allow/deny prompt (remote-control-qa1). Absent once resolved.
    needs_input_since: HashMap<SessionId, u64>,
    event_seq: u64,
    pairing: Option<PairingId>,
    /// Whether the relay link is currently up (authenticated). While it is down
    /// the bridge PAUSES all seal/queue work: sealing StatusUpdate/Rollup/shell/
    /// transcript envelopes every render tick during a relay outage burns crypto
    /// and CPU and, because the client thread is not draining the outbound channel
    /// while reconnecting, grows it without bound until the whole backlog floods
    /// out on reconnect (remote-control-0ef.10). Defaults to `true` so callers
    /// that never forward link state (the unit tests) behave exactly as before;
    /// production toggles it from [`RemoteInbound::Link`]. Reconnect-replay is
    /// preserved: a reconnect re-arms `snapshot_needed` via `Paired`, so a single
    /// fresh snapshot — not a stale backlog — is sent when the link returns.
    link_up: bool,
    /// Whether a phone peer is currently attached to the pairing. While no phone
    /// is attached the bridge does NOT seal+send the per-tick snapshot/status/
    /// rollup deltas — during the 2026-07-22 incident the desktop sealed and sent
    /// a status_update every second for hours into an empty relay queue with no
    /// peer to receive it (remote-control-uqa). Presence is authoritative even
    /// across a desktop reconnect: the relay announces an already-connected
    /// peer's presence to the freshly-attached leg (session.rs on attach). A
    /// phone (re)attaching re-arms `snapshot_needed` so it still gets a full,
    /// current snapshot the moment it connects. Defaults to `false` (no phone
    /// until one attaches); unit tests that drive presence set it explicitly.
    peer_present: bool,
    /// Whether this relay forwards peer acks at all — set by the FIRST
    /// [`RemoteInbound::PeerAck`] seen on this connection, for any pairing, and
    /// never cleared. Until then the ack deadline below stays DISARMED and the
    /// bridge behaves exactly as it did before remote-control-5qu.
    ///
    /// This is the compatibility seam, and it is load-bearing. The relay never
    /// used to forward the phone's `ack` to the desktop at all, which is why
    /// `last_acked_by_peer` sat at 0 for 33,000 envelopes. A desktop that
    /// enforced an ack deadline against such a relay would declare EVERY phone
    /// dark within a minute, so the guard only arms once the relay has
    /// demonstrated it relays acks. The relay echoes the stored cursor for each
    /// activated pairing right after `auth_ok` — even when it is 0 — so against
    /// an upgraded relay this arms within a second of connecting, before any
    /// envelope is sent, and the "never acked even once" case is caught too.
    peer_acks_observed: bool,
    /// Highest outbound `seq` the peer has acknowledged (the app-side mirror of
    /// `Pairing::last_acked_by_peer`), for the ACTIVE pairing only.
    peer_acked_seq: u64,
    /// Outbound `seq` at or below which un-acked envelopes are not held against
    /// the peer, because we have no evidence either way and their silence is not
    /// attributable to this phone: envelopes a previous process sent (seeded from
    /// the resumed high-water mark in [`Self::install_channel`]) and envelopes the
    /// relay itself rejected (rebased in the `SeqRealign` arm). Liveness is only
    /// ever judged on envelopes this session actually put on the wire.
    ack_baseline_seq: u64,
    /// Clock-ms (injected clock) from which the current ack window is measured —
    /// armed on the first tick at which an envelope is outstanding, cleared the
    /// moment the peer catches up or produces any other evidence of life. `None`
    /// means "nothing is owed", which is the state a legitimately quiet phone
    /// with nothing to ack sits in forever.
    ack_wait_since_ms: Option<u64>,
    /// Whether the peer has stopped proving it is there (see [`PeerLiveness`]).
    /// ANDed with `peer_present` to gate the per-tick feed, so it can only ever
    /// make that gate stricter — never looser (remote-control-uqa).
    peer_dark: bool,
    /// Consecutive `SeqRealign`s that have excused the un-acked backlog without
    /// an intervening ack; bounded by [`MAX_REALIGN_CREDITS`].
    realign_credits: u32,
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
    /// Clock-ms (from the caller-supplied `now_ms`, i.e. the injected clock —
    /// see [`crate::contracts::Clock`]) of the last transcript sync performed
    /// while unpaired, throttling [`Self::sync_transcript`] to
    /// [`UNPAIRED_TRANSCRIPT_SYNC_INTERVAL_MS`] when no phone is present
    /// (remote-control-0ef.13). `None` before the first unpaired sync, so the
    /// very first tick always syncs. Irrelevant once paired: `tick()` forces a
    /// sync every tick while `is_paired()` is true, regardless of this timer.
    last_unpaired_sync_ms: Option<i64>,
    /// Keystrokes queued to inject into a session's primary PTY after a short
    /// delay: `(session, due_ms, bytes)`. Used for Claude's multi-select submit
    /// Enter, which must arrive AFTER the Tab-driven switch to the Confirm tab
    /// has rendered or the Ink TUI drops it (remote-control-dc9). Flushed by
    /// `service_remote_commands` once `due_ms` passes.
    deferred_pty: Vec<(SessionId, u64, Vec<u8>)>,
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
            needs_input_since: HashMap::new(),
            event_seq: 0,
            pairing: None,
            link_up: true,
            peer_present: false,
            peer_acks_observed: false,
            peer_acked_seq: 0,
            ack_baseline_seq: 0,
            ack_wait_since_ms: None,
            peer_dark: false,
            realign_credits: 0,
            snapshot_needed: true,
            grace_until_ms,
            pending_transcript_requests: Vec::new(),
            pending_commands: Vec::new(),
            shells: ShellManager::new(),
            seal,
            open,
            home: None,
            out_seq: 0,
            last_unpaired_sync_ms: None,
            deferred_pty: Vec::new(),
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
        // Whatever is already outstanding at a session boundary is not this
        // phone's fault to answer for. At startup `resume_from_seq` is the
        // persisted `last_sent_seq` — envelopes a PREVIOUS process sent, which the
        // phone may well have acked before the restart — and a mid-session repeat
        // claim means the phone just authenticated at the relay and redeemed a
        // token, which is direct proof of life. Judging liveness only on
        // envelopes we send from here on is what keeps a restarted desktop from
        // declaring a perfectly healthy phone dark on its first tick.
        self.forgive_unacked();
    }

    /// Revert to the no-crypto passthrough and forget the active pairing — used
    /// when the user unpairs, so the desktop stops sealing to a peer that is no
    /// longer trusted and is ready to pair afresh.
    pub fn reset_to_passthrough(&mut self) {
        self.seal = passthrough_seal();
        self.open = passthrough_open();
        self.out_seq = 0;
        self.reset_ack_tracking();
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

    /// Queue keystrokes to inject into `session`'s primary PTY once `due_ms`
    /// passes (see `deferred_pty`). Used for Claude's multi-select submit Enter.
    pub fn enqueue_deferred_pty(&mut self, session: SessionId, due_ms: u64, bytes: Vec<u8>) {
        self.deferred_pty.push((session, due_ms, bytes));
    }

    /// Remove and return every queued deferred PTY write whose `due_ms` is at or
    /// before `now_ms`, as `(session, bytes)`. Order preserved.
    pub fn take_due_deferred_pty(&mut self, now_ms: u64) -> Vec<(SessionId, Vec<u8>)> {
        let mut due = Vec::new();
        self.deferred_pty.retain(|(session, due_ms, bytes)| {
            if *due_ms <= now_ms {
                due.push((session.clone(), bytes.clone()));
                false
            } else {
                true
            }
        });
        due
    }

    /// Tee a coalesced read of a child terminal's PTY bytes into the shell
    /// manager (a no-op unless that child backs the session's live remote
    /// shell). Called from the per-tick PTY drain; cheap and always safe.
    pub fn shell_pump(&mut self, session_id: &str, child_index: usize, bytes: &[u8]) {
        self.shells
            .pump(&SessionId::new(session_id), child_index, bytes);
    }

    /// Reconstruct a session's transcript from the agent's own conversation
    /// store, ingesting anything written since the last call. Safe to call
    /// often; builds history even before a phone pairs. A no-op when the home dir
    /// is unset or the agent has no locatable store (an OpenCode agent on Windows,
    /// an unknown agent, or before the agent has written its first record).
    /// Called from [`Self::tick`] with the session's `agent` kind and absolute
    /// `worktree`, gated by `sync_transcripts_this_tick`
    /// (remote-control-0ef.13) so this filesystem work is throttled while no
    /// phone is paired rather than run every render tick.
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
                    // A different peer with its own ack cursor: everything the
                    // old one owed us is meaningless now.
                    self.reset_ack_tracking();
                }
                self.pairing = Some(pairing_id);
                self.snapshot_needed = true;
            }
            // The peer's view of this pairing is stale (it restarted its stream,
            // or the relay shed envelopes it needed). Re-send a full snapshot so
            // it rebuilds from known-good state instead of from deltas it may
            // have missed.
            //
            // `out_seq` is deliberately left alone. Rewinding it to 0 here was
            // the old recovery for a relay that came back with an empty
            // watermark (remote-control-bbf); against a relay that *persists* its
            // watermark the rewind is rejected, which drove another resync, which
            // rewound again — the livelock in remote-control-arg. The relay now
            // adopts an unknown stream's starting seq, so continuing to count up
            // is both correct and the only thing that terminates.
            RemoteInbound::SeqResync { pairing_id } => {
                if self.pairing.as_ref() == Some(&pairing_id) {
                    self.snapshot_needed = true;
                }
            }
            // Our outbound stream ran ahead of the relay's watermark and every
            // envelope since has been dropped. Realign to the seq the relay named
            // so the next one is accepted, and re-arm a full snapshot: the peer
            // missed everything we sent while we were ahead, and a `status_update`
            // delta can only mutate sessions it already knows.
            //
            // `next_seq` is the relay's `high_water + 1`, so assigning
            // `out_seq = next_seq - 1` makes the next envelope exactly `next_seq`.
            // Unlike `SeqResync` this deliberately DOES move `out_seq`, because
            // here the outbound counter is the thing that is wrong — the whole
            // reason the stream could never recover before (remote-control-zv3).
            // Guarded on the active pairing so a late advisory for a pairing we
            // have already replaced cannot rewind the live one.
            RemoteInbound::SeqRealign {
                pairing_id,
                next_seq,
            } => {
                if self.pairing.as_ref() == Some(&pairing_id) {
                    self.out_seq = next_seq.saturating_sub(1);
                    self.snapshot_needed = true;
                    // The relay REJECTED those envelopes, so they never reached
                    // the phone and their silence says nothing about it: a
                    // rejected stream is a different fault from a dead peer even
                    // though both produce "no acks" (remote-control-5qu vs zv3).
                    // Rebase the ack tracking onto the renumbered stream and give
                    // the realigned stream a fresh window — bounded, so a
                    // stream that only ever realigns still ends up dark.
                    if self.realign_credits < MAX_REALIGN_CREDITS {
                        self.realign_credits += 1;
                        self.ack_baseline_seq = self.out_seq;
                        self.peer_acked_seq = self.peer_acked_seq.min(self.out_seq);
                        self.credit_peer_liveness();
                    }
                }
            }
            // The offer (code shown) does not itself activate a pairing for the
            // outbound feed — the phone has not joined yet. Handled by the
            // pairing overlay, not the bridge.
            RemoteInbound::PairingOffered { .. } => {}
            // A handshake that never reached `auth_ok` is purely a UI concern
            // (the pairing overlay explains itself). The bridge already pauses
            // on anything other than `Link(Connected)`, and this arrives with no
            // `Link` transition of its own, so there is nothing to do here.
            RemoteInbound::HandshakeFailed { .. } => {}
            RemoteInbound::Envelope(env) => {
                if self.pairing.is_none() {
                    self.pairing = Some(env.pairing_id.clone());
                }
                // The phone sent us something: it is unambiguously there, even if
                // this particular envelope fails to open. Credit it before the
                // open attempt so a crypto fault cannot be mistaken for absence.
                if self.pairing.as_ref() == Some(&env.pairing_id) {
                    self.credit_peer_liveness();
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
            // Track whether a phone peer is attached so `tick` can skip sealing
            // per-tick deltas to nobody (remote-control-uqa). A phone becoming
            // present re-arms `snapshot_needed` so it receives a fresh full
            // snapshot on attach rather than waiting for the next change.
            RemoteInbound::Presence { peer, state, .. } => {
                if peer == Role::Phone {
                    let now_present =
                        matches!(state, flightdeck_remote_protocol::PresenceState::Connected);
                    // Every `Connected` re-arms, not just a false→true edge
                    // (remote-control-e9l). A repeat `Connected` means a NEW
                    // phone leg superseded the old one in the relay's registry,
                    // and the relay deliberately sends no `Disconnected` for a
                    // superseded leg (it would libel the newer leg — see
                    // `Registry::detach`). So a phone whose socket died
                    // half-open — the normal case when iOS suspends the app —
                    // reattaches with `peer_present` still `true` here and,
                    // under an edge-triggered re-arm, never got a fresh
                    // snapshot. It then sat on whatever session set it last
                    // saw: `status_update` deltas can only change sessions the
                    // phone already knows, never add or remove one, and nothing
                    // else re-arms periodically.
                    if now_present {
                        self.snapshot_needed = true;
                    }
                    self.peer_present = now_present;
                    // A presence frame is fresh word from the relay about the
                    // peer's leg, so the peer gets a clean ack window either way:
                    // an attach (or re-attach) deserves a chance to prove itself,
                    // and a detach/re-attach cycle must not inherit the previous
                    // leg's stall. The BACKLOG bound is deliberately NOT reset
                    // here, so a phone that reattaches over and over without ever
                    // receiving anything is still caught.
                    self.credit_peer_liveness();
                }
            }
            // The relay forwarded the phone's cumulative ack for OUR stream: the
            // one piece of end-to-end evidence that the phone is really
            // receiving. Arms the guard (see `peer_acks_observed`) and, when the
            // cursor actually advances, credits the peer with being alive.
            RemoteInbound::PeerAck { pairing_id, cursor } => {
                // Arming is a property of the RELAY, not of one pairing, so it is
                // set for any pairing and never cleared.
                self.peer_acks_observed = true;
                if self.pairing.as_ref() == Some(&pairing_id) && cursor > self.peer_acked_seq {
                    self.peer_acked_seq = cursor;
                    self.realign_credits = 0;
                    self.credit_peer_liveness();
                }
            }
            // The relay is shedding our un-acked envelopes: the peer has stopped
            // draining ~1000 of them, which is proof enough on its own. Applies
            // whether or not the ack guard is armed — every deployed relay
            // already sends this advisory, and the desktop used to ignore it.
            RemoteInbound::PeerBacklog { pairing_id } => {
                if self.pairing.as_ref() == Some(&pairing_id) && !self.peer_dark {
                    self.peer_dark = true;
                    crate::remote::debuglog::log(&format!(
                        "bridge PEER DARK (relay queue overflow) pairing={} out_seq={} acked={}",
                        pairing_id.as_str(),
                        self.out_seq,
                        self.peer_acked_seq
                    ));
                }
            }
            // Track link state so `tick` can pause seal/queue while the relay is
            // unreachable (remote-control-0ef.10). Only a live, authenticated link
            // (`Connected`) permits sending; Connecting/Disconnected/Incompatible
            // all pause. On reconnect the client re-emits `Paired`, which re-arms
            // `snapshot_needed`, so a fresh snapshot is sent — not a stale backlog.
            RemoteInbound::Link(state) => {
                self.link_up = matches!(state, crate::remote::RemoteLinkState::Connected { .. });
            }
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

    /// Whether the attached phone is still earning its presence — see
    /// [`PeerLiveness`], which also records who reads this (today: the tests,
    /// and nothing on screen). `Dark` means the relay link is up but the phone
    /// is not receiving, which used to be indistinguishable from a healthy link
    /// (remote-control-5qu).
    pub fn peer_liveness(&self) -> PeerLiveness {
        if !self.peer_present || self.pairing.is_none() {
            PeerLiveness::NoPeer
        } else if self.peer_dark {
            PeerLiveness::Dark
        } else {
            PeerLiveness::Live
        }
    }

    /// The highest outbound `seq` we have no reason to think the phone is
    /// missing: what it has acked, or what predates this session's judgement.
    fn ack_floor(&self) -> u64 {
        self.peer_acked_seq.max(self.ack_baseline_seq)
    }

    /// Record fresh evidence that the peer is alive: it stops being dark, and the
    /// ack window restarts from the next tick that still has something
    /// outstanding. Does NOT forgive the backlog itself, so the
    /// [`MAX_UNACKED_ENVELOPES`] bound survives a flapping peer.
    fn credit_peer_liveness(&mut self) {
        self.peer_dark = false;
        self.ack_wait_since_ms = None;
    }

    /// Stop holding the currently-outstanding envelopes against the peer (a
    /// session boundary, not evidence about any single envelope).
    fn forgive_unacked(&mut self) {
        self.ack_baseline_seq = self.ack_baseline_seq.max(self.out_seq);
        self.credit_peer_liveness();
    }

    /// Forget everything we knew about the peer's ack position — used when the
    /// pairing itself changes, so a new phone's cursor never inherits the old
    /// one's. `peer_acks_observed` deliberately survives: it describes the relay.
    fn reset_ack_tracking(&mut self) {
        self.peer_acked_seq = 0;
        self.ack_baseline_seq = 0;
        self.realign_credits = 0;
        self.credit_peer_liveness();
    }

    /// Re-evaluate ack-based peer liveness for this tick (remote-control-5qu).
    ///
    /// "No news" is read direction-sensitively, which is the whole design:
    /// * nothing outstanding (`out_seq <= ack_floor`) ⇒ the peer owes us nothing,
    ///   so silence proves nothing and the window is disarmed. A connected phone
    ///   with nothing to ack is therefore NEVER declared absent.
    /// * something outstanding ⇒ silence is measured. Past
    ///   [`PEER_ACK_TIMEOUT_MS`], or [`MAX_UNACKED_ENVELOPES`] behind, the peer
    ///   is dark.
    ///
    /// Disarmed entirely until the relay has proved it forwards peer acks
    /// (`peer_acks_observed`), so an older relay behaves exactly as before.
    fn update_peer_liveness(&mut self, now_ms: u64) {
        if !self.peer_acks_observed {
            return;
        }
        let floor = self.ack_floor();
        if self.out_seq <= floor {
            self.ack_wait_since_ms = None;
            return;
        }
        // Normally already armed by `send_msg` at the instant the envelope was
        // sealed; this covers a cursor that went outstanding any other way.
        let since = *self.ack_wait_since_ms.get_or_insert(now_ms);
        let waited_ms = now_ms.saturating_sub(since);
        let backlog = self.out_seq - floor;
        if !self.peer_dark && (waited_ms >= PEER_ACK_TIMEOUT_MS || backlog >= MAX_UNACKED_ENVELOPES)
        {
            self.peer_dark = true;
            crate::remote::debuglog::log(&format!(
                "bridge PEER DARK (no ack) pairing={:?} out_seq={} acked={} unacked={} waited_ms={}",
                self.pairing.as_ref().map(|p| p.as_str()),
                self.out_seq,
                self.peer_acked_seq,
                backlog,
                waited_ms
            ));
        }
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
        // Whether this tick performs the (filesystem-touching) transcript sync
        // below. Paired: sync every tick, as before — a phone is actively
        // receiving history and responsiveness matters. Unpaired: throttle to
        // `UNPAIRED_TRANSCRIPT_SYNC_INTERVAL_MS`, since nobody is there to
        // receive it (remote-control-0ef.13). `currently_paired` alone forces
        // this true on the very tick a pairing becomes active — no separate
        // "just paired" bookkeeping needed — so a late-pairing phone still
        // gets a sync (and thus full history) immediately on (re)pair.
        let now_i64 = now_ms as i64;
        let currently_paired = self.is_paired();
        let sync_transcripts_this_tick = currently_paired
            || match self.last_unpaired_sync_ms {
                None => true,
                Some(last) => now_i64.saturating_sub(last) >= UNPAIRED_TRANSCRIPT_SYNC_INTERVAL_MS,
            };
        if sync_transcripts_this_tick && !currently_paired {
            self.last_unpaired_sync_ms = Some(now_i64);
        }

        // Pre-pass: per-session edge detection (events + needs-input preview).
        let mut events = Vec::new();
        for pv in projects {
            for tab in pv.state.tabs.iter() {
                let sid = SessionId::new(&tab.meta.id);

                // Reconstruct the transcript from the agent's session file. Done
                // here, before the pairing gate below, so a phone that pairs
                // later still receives the accumulated history (remote-control-72k).
                // Throttled while unpaired; see `sync_transcripts_this_tick` above.
                let worktree = pv.state.repo_root.join(&tab.meta.worktree_path_relative);
                if sync_transcripts_this_tick {
                    self.sync_transcript(&tab.meta.id, &tab.meta.agent, &worktree, now_i64);
                }

                // Where the agent's status hook/plugin actually writes its
                // sidecars: the tab's worktree in a normal run, a redirect
                // target (outside the project) in an isolated one. Derived from
                // `status_file` (`<status_root>/.flightdeck/agent-status`)
                // rather than recomputed, so this can never drift from the root
                // `prepare_status_launch` was actually given. Falls back to the
                // worktree before the tab's first spawn sets `status_file`.
                let status_root = tab
                    .status_file
                    .as_deref()
                    .and_then(|f| f.parent()?.parent())
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| worktree.clone());

                let ds = tab.display_status(now_ms);
                let interpreted = ds.interpreted;
                let status = feed::agent_status(ds);

                // Needs-input edge → capture preview + inline permission prompt.
                let was_needs = matches!(self.prev_status.get(&sid), Some(AgentStatus::NeedsInput));
                let now_needs = matches!(status, AgentStatus::NeedsInput);
                // Claude asks questions via an AskUserQuestion tool_use in the
                // JSONL (ingested above); the other backends do not, so only
                // Claude can race a status flip against its own transcript
                // record. Identified by backend rather than by "not the ones we
                // know", so a newly added backend is never silently read as
                // Claude and handed Claude's sidecar.
                let backend = match pv.state.registry.get(&tab.meta.agent) {
                    Some(def) => status_backend(def),
                    // No registry entry (a tab restored before the config is
                    // loaded): the key itself is the command name for every
                    // built-in whose key and binary match.
                    None => status_backend(&AgentDef {
                        command: tab.meta.agent.clone(),
                        ..AgentDef::default()
                    }),
                };
                let is_opencode = matches!(backend, Some(StatusBackend::OpenCode));
                let is_claude = matches!(backend, Some(StatusBackend::Claude));
                if now_needs && !was_needs {
                    // Read the agent's prompt sidecar BEFORE `on_needs_input` so a
                    // captured structured prompt supplants the binary fallback:
                    // OpenCode's plugin writes `agent-prompt.json`; Claude's
                    // PreToolUse hook writes `agent-question.json` for an
                    // AskUserQuestion. The sidecar is written before the status
                    // flips to `waiting`, so it is present on this edge and the
                    // real question shows immediately — never the binary card whose
                    // keystroke would answer it (remote-control-qa1).
                    let sidecar = if is_opencode {
                        read_prompt_sidecar(&status_root)
                    } else if is_claude {
                        read_claude_question_sidecar(&status_root)
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
                    if builder.has_structured_prompt() {
                        // A structured prompt is ready now (an OpenCode sidecar, or
                        // a Claude AskUserQuestion already ingested this tick) —
                        // surface it immediately.
                        let preview = builder.on_needs_input(now_ms as i64);
                        self.previews.insert(sid.clone(), preview);
                    } else if is_claude {
                        // Claude is waiting but no question has been ingested yet.
                        // It may be a real permission prompt OR an AskUserQuestion
                        // whose JSONL line has not landed. DEFER the binary
                        // fallback: emitting it now would show a bogus allow/deny
                        // card whose "Allow once" keystroke ("1") the live question
                        // selector consumes as an answer (remote-control-qa1).
                        self.needs_input_since.insert(sid.clone(), now_ms);
                    } else {
                        // OpenCode/Codex: no JSONL-ingested question can race, so
                        // the binary fallback (or sidecar) is correct immediately.
                        let preview = builder.on_needs_input(now_ms as i64);
                        self.previews.insert(sid.clone(), preview);
                    }
                    if is_opencode {
                        let _ = std::fs::remove_file(prompt_sidecar_path(&status_root));
                    }
                    if is_claude {
                        // Consume the sidecar so a later, unrelated wait never
                        // reuses this question.
                        let _ = std::fs::remove_file(claude_question_sidecar_path(&status_root));
                    }
                } else if now_needs && was_needs {
                    // Still waiting with a deferred Claude binary fallback: resolve
                    // it once the AskUserQuestion has been ingested (surface the
                    // question, no binary) or the settle window elapses (it was a
                    // real permission prompt after all → binary now).
                    if let Some(&since) = self.needs_input_since.get(&sid) {
                        let builder = self
                            .transcripts
                            .entry(sid.clone())
                            .or_insert_with(|| TranscriptBuilder::new(sid.clone()));
                        if builder.has_open_prompt() {
                            self.needs_input_since.remove(&sid);
                            self.previews
                                .insert(sid.clone(), builder.open_prompt_preview());
                        } else if now_ms.saturating_sub(since) >= PROMPT_SETTLE_MS {
                            self.needs_input_since.remove(&sid);
                            let preview = builder.on_needs_input(now_ms as i64);
                            self.previews.insert(sid.clone(), preview);
                        }
                    }
                } else if !now_needs && was_needs {
                    self.previews.remove(&sid);
                    self.needs_input_since.remove(&sid);
                    // The prompt was answered (agent left needs-input) — clear the
                    // open-prompt dedup guard so the NEXT question in this session
                    // surfaces as a fresh prompt instead of being suppressed as a
                    // duplicate and reusing the old frame (remote-control-dc9).
                    if let Some(builder) = self.transcripts.get_mut(&sid) {
                        builder.clear_open_prompt();
                    }
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

        // Nothing to transmit without a pairing (state kept for the next pair), or
        // while the relay link is down — pause seal/queue during an outage so we
        // do not burn crypto/CPU sealing into an outbound channel the client is
        // not draining (unbounded growth + a reconnect flood), remote-control-0ef.10.
        // All per-session/transcript bookkeeping above still runs, so a reconnect
        // sends fresh, current state.
        if self.pairing.is_none() || !self.link_up {
            return;
        }

        let sent_at = now_ms as i64;

        // Re-evaluate whether the phone is still earning its presence, BEFORE
        // anything is sealed this tick (remote-control-5qu).
        self.update_peer_liveness(now_ms);

        // Send typed events first (most urgent).
        for ev in events {
            self.send_msg(DesktopToPhone::Event(ev), sent_at, send);
        }

        // Per-tick snapshot / status / rollup deltas go out only when a phone
        // peer is actually attached. With no phone present the desktop otherwise
        // seals + sends a status_update into an empty relay queue every tick with
        // no one to receive it — observed spamming ~once a second for hours during
        // the 2026-07-22 incident (remote-control-uqa). A phone attaching re-arms
        // `snapshot_needed` (see the presence handler), so it still gets a full,
        // current snapshot the instant it connects. Events, transcript flushes and
        // shell output below are edge/request-driven, not per-tick, so they are
        // unaffected.
        // `!peer_dark` makes this gate STRICTLY narrower than the presence check
        // remote-control-uqa introduced: presence is what the relay asserts, dark
        // is what the phone has failed to earn by acking. The reported failure had
        // presence latched `true` with no way to clear it — the relay deliberately
        // sends no `Disconnected` for a superseded leg — so this per-tick feed
        // shipped 33,000 envelopes to a phone that never received one.
        //
        // Events, transcript flushes, transcript replies, shell output and command
        // acks below stay ungated: they are edge/request-driven rather than
        // per-tick (so they cannot spam), and they double as free liveness probes
        // — an ack for any one of them revives the feed without needing a presence
        // frame from the relay.
        if self.peer_present && !self.peer_dark {
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
            // Start the ack clock on the FIRST envelope the peer owes us an
            // answer for (remote-control-5qu). Arming here rather than on the
            // next tick means the deadline measures the envelope's real age, and
            // a burst that all goes out on one tick shares one deadline. Cleared
            // by `credit_peer_liveness` / catch-up, so a quiet phone never has a
            // clock running against it.
            if self.ack_wait_since_ms.is_none() && seq > self.ack_floor() {
                self.ack_wait_since_ms = Some(now_ms.max(0) as u64);
            }
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
    /// Whether the question accepts multiple selections (checklist). The
    /// OpenCode runtime plugin probes `multiple`/`multiSelect` and writes this.
    #[serde(default)]
    multiple: bool,
}

#[derive(serde::Deserialize)]
struct PromptSidecarOption {
    #[serde(default)]
    label: String,
    #[serde(default)]
    description: Option<String>,
}

/// Path of the OpenCode prompt sidecar under `status_root` (sibling of the
/// `agent-status` file the poller reads — the same root [`prepare_status_launch`]
/// was given, normally the worktree but redirected for an isolated run).
///
/// [`prepare_status_launch`]: crate::agents::setup::prepare_status_launch
fn prompt_sidecar_path(status_root: &Path) -> PathBuf {
    status_root.join(".flightdeck").join("agent-prompt.json")
}

/// Path of the Claude AskUserQuestion sidecar under `status_root` (see
/// [`prompt_sidecar_path`]). Written by the Claude `PreToolUse`/`AskUserQuestion`
/// hook (which pipes the hook's stdin — the `{tool_name, tool_input, …}`
/// payload — to this file) at the instant the question is asked, BEFORE it
/// flips the status to `waiting`. This gives the desktop the question
/// deterministically on the needs-input edge, rather than waiting for Claude to
/// write the tool_use to its JSONL — which it does only AFTER the user answers,
/// so the binary fallback would otherwise win the race and its "Allow"
/// keystroke would answer the live question (remote-control-qa1).
fn claude_question_sidecar_path(status_root: &Path) -> PathBuf {
    status_root.join(".flightdeck").join("agent-question.json")
}

/// Read and parse the Claude AskUserQuestion sidecar into a [`StructuredPrompt`],
/// or `None` when it is absent/malformed. The file holds the raw PreToolUse hook
/// payload, so the `tool_input` field is the AskUserQuestion input the transcript
/// parser already understands.
fn read_claude_question_sidecar(status_root: &Path) -> Option<StructuredPrompt> {
    let raw = std::fs::read_to_string(claude_question_sidecar_path(status_root)).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let input = parsed.get("tool_input")?;
    crate::remote::transcript::parse_ask_user_question(input)
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
fn read_prompt_sidecar(status_root: &Path) -> Option<StructuredPrompt> {
    let raw = std::fs::read_to_string(prompt_sidecar_path(status_root)).ok()?;
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
                multi_select: parsed.multiple,
                // OpenCode's sidecar carries a single question; the flat fields
                // above describe it, so the multi-question list stays empty.
                questions: Vec::new(),
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
                // Permissions are always a single binary choice.
                multi_select: false,
                questions: Vec::new(),
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
