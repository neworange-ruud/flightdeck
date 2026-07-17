//! Pure builders that turn FlightDeck's headless read-model into the outbound
//! feed types the phone renders.
//!
//! Everything here is **pure**: functions take a borrowed [`AppState`] (plus the
//! project's cached git status and the current clock) and produce
//! [`flightdeck_remote_protocol`] value types. No I/O, no channels, no crypto —
//! the [`crate::remote::bridge`] layer owns those and calls into here.
//!
//! The mapping mirrors the TUI exactly:
//! * per-session status comes from [`RuntimeTab::display_status`] (the same
//!   read-model the sidebar uses), collapsed into the protocol's four-state
//!   [`AgentStatus`];
//! * the project roll-up follows the PRD §4 precedence
//!   (needs-input > working > manual > idle);
//! * git indicators come from the same [`WorktreeStatus`] cache the info bar
//!   reads.
//!
//! Deltas are handled by [`FeedState`]: it remembers the last value it reported
//! for every session/project so each tick emits only what changed, and a full
//! [`StateSnapshot`] is produced on (re)connect or an explicit phone request.

use std::collections::HashMap;

use crate::agents::status::DisplayStatus;
use crate::app::state::AppState;
use crate::contracts::{InterpretedStatus, ManualStatus};
use crate::git::status::WorktreeStatus;
use crate::tui::render::GitStatusCache;

use flightdeck_remote_protocol::{
    AgentStatus, AgentType, GitIndicators, GitStatusDetail, ProjectRollup, ProjectState, RollupDot,
    SessionState, SessionStatusDelta, StateSnapshot, StatusRollup,
};
use flightdeck_remote_protocol::{ProjectId, SessionId};

// ---------------------------------------------------------------------------
// Scalar mappers (pure, exhaustively unit-tested)
// ---------------------------------------------------------------------------

/// Map a FlightDeck agent key to the protocol's agent-CLI enum. The key is
/// config-driven and free-form, so this is a name heuristic: anything that is
/// not recognisably Codex or OpenCode is reported as Claude Code (the default
/// backend). This never panics and has no I/O.
pub fn agent_type_of(agent_key: &str) -> AgentType {
    let k = agent_key.to_ascii_lowercase();
    if k.contains("codex") {
        AgentType::Codex
    } else if k.contains("opencode") {
        AgentType::Opencode
    } else {
        AgentType::ClaudeCode
    }
}

/// Collapse the desktop's rich [`DisplayStatus`] into the phone's four-state
/// [`AgentStatus`], preserving TUI semantics:
///
/// * a manual override wins (cyan), exactly as the TUI paints it;
/// * otherwise the interpreted status drives: working/starting/running →
///   working, waiting/needs-attention → needs-input, everything settled → idle.
///
/// `Failed` maps to `idle` for the row status (the turn has ended and the agent
/// is not asking a question); the error is surfaced separately as an
/// [`flightdeck_remote_protocol::EventKind::Error`] event.
pub fn agent_status(ds: DisplayStatus) -> AgentStatus {
    if let Some(m) = ds.manual {
        return AgentStatus::Manual {
            label: manual_label(m).to_string(),
        };
    }
    match ds.interpreted {
        InterpretedStatus::Starting | InterpretedStatus::Running | InterpretedStatus::Working => {
            AgentStatus::Working
        }
        InterpretedStatus::WaitingForInput | InterpretedStatus::NeedsAttention => {
            AgentStatus::NeedsInput
        }
        InterpretedStatus::Idle
        | InterpretedStatus::Completed
        | InterpretedStatus::Failed
        | InterpretedStatus::Stopped
        | InterpretedStatus::SessionLost
        | InterpretedStatus::Recovered
        | InterpretedStatus::Unknown => AgentStatus::Idle,
    }
}

/// The user-visible label for a manual override (matches the TUI wording).
fn manual_label(m: ManualStatus) -> &'static str {
    m.as_str()
}

/// Build the compact git indicators for a session row from the cached worktree
/// status, falling back to the tab's branch name and zeroed counts when the
/// cache has not been populated yet (e.g. immediately after a tab is created).
pub fn git_indicators(ws: Option<&WorktreeStatus>, fallback_branch: &str) -> GitIndicators {
    match ws {
        Some(s) => GitIndicators {
            branch: Some(s.branch.clone()),
            added: s.changes.added,
            modified: s.changes.modified,
            removed: s.changes.deleted,
            ahead: s.ahead,
            behind: s.behind,
            drift: s.base_drift,
            has_upstream: s.upstream.is_some(),
        },
        None => GitIndicators {
            branch: if fallback_branch.is_empty() {
                None
            } else {
                Some(fallback_branch.to_string())
            },
            added: 0,
            modified: 0,
            removed: 0,
            ahead: 0,
            behind: 0,
            drift: 0,
            has_upstream: false,
        },
    }
}

/// Build the full read-only git status for a session's worktree (design §5.5)
/// from the same cached [`WorktreeStatus`] the info bar reads.
///
/// The scalar fields (branch, base, upstream, ahead/behind, drift) come straight
/// from the cache. The per-file list is **not** available from the cache — the
/// desktop's status collection stores only per-category *counts*
/// ([`crate::git::status::WorktreeChanges`]), not paths or line deltas — so
/// `files` is left empty here. The compact per-category counts already travel on
/// every session row as [`GitIndicators`]; populating `files` with real paths
/// and line counts requires extending `WorktreeStatus`, tracked as follow-up.
pub fn git_status_detail(
    session_id: &SessionId,
    ws: Option<&WorktreeStatus>,
    fallback_branch: &str,
) -> GitStatusDetail {
    match ws {
        Some(s) => GitStatusDetail {
            session_id: session_id.clone(),
            branch: Some(s.branch.clone()),
            base_branch: Some(s.base_branch.clone()),
            has_upstream: s.upstream.is_some(),
            ahead: s.ahead,
            behind: s.behind,
            drift: s.base_drift,
            files: Vec::new(),
        },
        None => GitStatusDetail {
            session_id: session_id.clone(),
            branch: (!fallback_branch.is_empty()).then(|| fallback_branch.to_string()),
            base_branch: None,
            has_upstream: false,
            ahead: 0,
            behind: 0,
            drift: 0,
            files: Vec::new(),
        },
    }
}

/// Compute a project roll-up from its sessions, following the PRD §4 precedence
/// for the dominant dot: needs-input > working > manual > idle.
pub fn rollup(sessions: &[SessionState]) -> StatusRollup {
    let mut working = 0u32;
    let mut idle = 0u32;
    let mut needs_input = 0u32;
    let mut manual = 0u32;
    for s in sessions {
        match &s.status {
            AgentStatus::Working => working += 1,
            AgentStatus::Idle => idle += 1,
            AgentStatus::NeedsInput => needs_input += 1,
            AgentStatus::Manual { .. } => manual += 1,
        }
    }
    let agent_count = sessions.len() as u32;
    let dot = if needs_input > 0 {
        RollupDot::NeedsInput
    } else if working > 0 {
        RollupDot::Working
    } else if manual > 0 {
        RollupDot::Manual
    } else {
        RollupDot::Idle
    };
    StatusRollup {
        dot,
        summary: rollup_summary(working, idle, needs_input, manual, agent_count),
        working,
        idle,
        needs_input,
        manual,
        agent_count,
    }
}

/// Plain-language roll-up summary, e.g. `1 needs input · 1 working · 3 agents`.
fn rollup_summary(working: u32, idle: u32, needs_input: u32, manual: u32, total: u32) -> String {
    if total == 0 {
        return "no agents".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    if needs_input > 0 {
        parts.push(format!("{needs_input} needs input"));
    }
    if working > 0 {
        parts.push(format!("{working} working"));
    }
    if manual > 0 {
        parts.push(format!("{manual} manual"));
    }
    if idle > 0 {
        parts.push(format!("{idle} idle"));
    }
    let unit = if total == 1 { "agent" } else { "agents" };
    parts.push(format!("{total} {unit}"));
    parts.join(" · ")
}

// ---------------------------------------------------------------------------
// Turn timing
// ---------------------------------------------------------------------------

/// Wall-clock timer for a session's current/last turn. A session is "in a turn"
/// while it is [`AgentStatus::Working`]; the elapsed time freezes when it leaves
/// the working state so the phone keeps showing how long the last turn took.
#[derive(Debug, Clone, Copy, Default)]
pub struct TurnTimer {
    /// When the current working span began (clock-millis), if working now.
    working_since_ms: Option<u64>,
    /// Elapsed seconds captured at the end of the last working span.
    frozen_secs: u64,
}

impl TurnTimer {
    /// Advance the timer for the observed `status` at `now_ms` and return the
    /// running time to report. Entering `working` starts a span; leaving it
    /// freezes the elapsed seconds.
    pub fn observe(&mut self, status: &AgentStatus, now_ms: u64) -> u64 {
        let working = matches!(status, AgentStatus::Working);
        if working {
            let since = *self.working_since_ms.get_or_insert(now_ms);
            self.frozen_secs = now_ms.saturating_sub(since) / 1000;
        } else if self.working_since_ms.take().is_some() {
            // Just left the working state — keep the last measured duration.
        }
        self.frozen_secs
    }
}

// ---------------------------------------------------------------------------
// Snapshot construction
// ---------------------------------------------------------------------------

/// Extra per-session inputs the pure builder cannot derive from [`AppState`]
/// alone: the running time (from a [`TurnTimer`]) and the pending-question
/// preview (captured by the transcript layer when the agent stops for input).
pub struct SessionExtras {
    /// Wall-clock running time to report for this session.
    pub running_time_secs: u64,
    /// The pending-question preview, when the agent is waiting for input.
    pub pending_question: Option<String>,
}

/// Build one [`SessionState`] row from a tab's read-model plus its extras.
pub fn build_session_state(
    project_id: &ProjectId,
    tab_id: &str,
    name: &str,
    agent_key: &str,
    ds: DisplayStatus,
    git: GitIndicators,
    extras: SessionExtras,
) -> SessionState {
    let status = agent_status(ds);
    // Only carry a pending question while the agent is actually waiting.
    let pending_question = if matches!(status, AgentStatus::NeedsInput) {
        extras.pending_question
    } else {
        None
    };
    SessionState {
        session_id: SessionId::new(tab_id),
        project_id: project_id.clone(),
        name: name.to_string(),
        agent_type: agent_type_of(agent_key),
        status,
        git,
        running_time_secs: extras.running_time_secs,
        pending_question,
    }
}

/// Build the sessions for one project from its [`AppState`] and git cache. The
/// `extras` closure supplies the running time and pending-question preview for
/// each tab id (the bridge wires this to its turn timers and transcript state).
pub fn build_project_state(
    project_id: &ProjectId,
    project_name: &str,
    state: &AppState,
    cache: &GitStatusCache,
    now_ms: u64,
    mut extras: impl FnMut(&str, &AgentStatus) -> SessionExtras,
) -> ProjectState {
    let mut sessions = Vec::with_capacity(state.tabs.len());
    for tab in state.tabs.iter() {
        let ds = tab.display_status(now_ms);
        let git = git_indicators(cache.get(&tab.meta.id), &tab.meta.branch);
        // Peek the status first so the extras provider can decide (e.g. only
        // capture a preview when needs-input).
        let status = agent_status(ds);
        let e = extras(&tab.meta.id, &status);
        sessions.push(build_session_state(
            project_id,
            &tab.meta.id,
            &tab.meta.name,
            &tab.meta.agent,
            ds,
            git,
            e,
        ));
    }
    let rollup = rollup(&sessions);
    ProjectState {
        project_id: project_id.clone(),
        name: project_name.to_string(),
        rollup,
        sessions,
    }
}

// ---------------------------------------------------------------------------
// Delta tracking
// ---------------------------------------------------------------------------

/// Remembers the last value reported to the phone for every session and project
/// so each tick can emit only the changes. A full snapshot resets it.
#[derive(Default)]
pub struct FeedState {
    sessions: HashMap<SessionId, SessionState>,
    rollups: HashMap<ProjectId, StatusRollup>,
}

/// What a diff against the current world produced.
pub struct FeedDelta {
    /// Per-session status changes to send.
    pub status: Vec<SessionStatusDelta>,
    /// Project roll-ups that changed.
    pub rollups: Vec<ProjectRollup>,
    /// True when the set of sessions or projects changed (add/remove). The
    /// bridge upgrades this to a full snapshot, since the delta protocol has no
    /// "session removed" message.
    pub set_changed: bool,
}

impl FeedState {
    /// Forget everything (used before recording a fresh snapshot).
    pub fn clear(&mut self) {
        self.sessions.clear();
        self.rollups.clear();
    }

    /// Record a full snapshot as the new baseline. Emits no deltas.
    pub fn record_snapshot(&mut self, snap: &StateSnapshot) {
        self.clear();
        for p in &snap.projects {
            self.rollups.insert(p.project_id.clone(), p.rollup.clone());
            for s in &p.sessions {
                self.sessions.insert(s.session_id.clone(), s.clone());
            }
        }
    }

    /// Diff the current world (`snap`) against the last-reported baseline,
    /// updating the baseline and returning what changed.
    pub fn diff(&mut self, snap: &StateSnapshot) -> FeedDelta {
        let mut status = Vec::new();
        let mut rollups = Vec::new();
        let mut set_changed = false;

        let mut seen_sessions = 0usize;
        for p in &snap.projects {
            match self.rollups.get(&p.project_id) {
                Some(prev) if prev == &p.rollup => {}
                _ => {
                    rollups.push(ProjectRollup {
                        project_id: p.project_id.clone(),
                        rollup: p.rollup.clone(),
                    });
                    self.rollups.insert(p.project_id.clone(), p.rollup.clone());
                }
            }
            for s in &p.sessions {
                seen_sessions += 1;
                match self.sessions.get(&s.session_id) {
                    Some(prev) if prev == s => {}
                    Some(_) => {
                        status.push(session_delta(s));
                        self.sessions.insert(s.session_id.clone(), s.clone());
                    }
                    None => {
                        set_changed = true;
                        self.sessions.insert(s.session_id.clone(), s.clone());
                    }
                }
            }
        }

        // A shrink (session or project removed) also forces a snapshot.
        if seen_sessions != self.sessions.len() {
            set_changed = true;
        }
        let live_projects: std::collections::HashSet<&ProjectId> =
            snap.projects.iter().map(|p| &p.project_id).collect();
        if live_projects.len() != self.rollups.len() {
            set_changed = true;
        }

        // Prune baselines for anything no longer present so the next diff is
        // consistent after the bridge sends its forced snapshot.
        if set_changed {
            let live_sessions: std::collections::HashSet<&SessionId> = snap
                .projects
                .iter()
                .flat_map(|p| p.sessions.iter().map(|s| &s.session_id))
                .collect();
            self.sessions.retain(|k, _| live_sessions.contains(k));
            self.rollups.retain(|k, _| live_projects.contains(k));
        }

        FeedDelta {
            status,
            rollups,
            set_changed,
        }
    }
}

/// Build a full status delta for a session (all fields resent; the phone
/// applies them wholesale — see the protocol note on `pending_question`).
fn session_delta(s: &SessionState) -> SessionStatusDelta {
    SessionStatusDelta {
        session_id: s.session_id.clone(),
        project_id: s.project_id.clone(),
        status: s.status.clone(),
        running_time_secs: Some(s.running_time_secs),
        pending_question: s.pending_question.clone(),
    }
}

#[cfg(test)]
mod tests;
