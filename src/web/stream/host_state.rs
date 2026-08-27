//! The desktop → wire adapter: `AppState` facts as protocol v1 views.
//!
//! Kept beside [`super::TerminalStreams`] because the two are called from the
//! same place — the TUI's tick — and because the byte-stream facts
//! (`byte_len`, `replay_from`) that every [`TerminalView`] carries live in the
//! registry next door.
//!
//! The functions here take **facts, not FlightDeck types wherever a FlightDeck
//! type would be private**. `src/lib.rs`'s `Project` is a private struct, so an
//! adapter that took `&Project` could not be called from a test or from
//! anywhere but `lib.rs`. Passing the handful of fields explicitly costs a
//! struct literal at the call site and buys a unit-testable conversion.
//!
//! # R2 — resolved here, by mapping (`specs/WEB_INTERFACE.md` §6.5)
//!
//! `webui/src/state/model.ts` models per-session git as a three-way union
//! (`known` / `no_upstream` / `unknown`) and carries a session-level
//! `lifecycleNote`. Protocol v1 carries two bools ([`GitBar::has_upstream`],
//! [`GitBar::collected`]) plus [`SessionView::lifecycle_reporting`]. R2 asked
//! whoever wired the socket to either widen the wire or map, and to say which.
//!
//! **Mapped, in this adapter.** Two reasons, in order of weight:
//!
//! 1. **The wire already carries the lifecycle fact as data.**
//!    [`SessionView::lifecycle_reporting`] plus
//!    [`SessionView::agent_display_name`] is precisely what turn 2 §5.1 asks
//!    for: the host sends a fact (`false`, `"Codex CLI"`), the browser writes
//!    the sentence (`unknown → unknown · Codex CLI reports no lifecycle`).
//!    Half of R2 needs no widening at all — the note in the backlog issue
//!    predates that field.
//! 2. **The two git bools are a faithful encoding of the three-way union, and
//!    the fourth state is impossible *here*, at the only place that can emit
//!    it.** [`git_bar`] derives both bools from one `Option<&WorktreeStatus>`,
//!    so `has_upstream: true` is unreachable without `collected: true`. Read as
//!    a union, exactly as `model.ts` does:
//!
//!    | wire | browser |
//!    | --- | --- |
//!    | `collected: false` | `unknown` → renders `git: ?` |
//!    | `collected: true, has_upstream: false` | `no_upstream` → renders `no-upstream` |
//!    | `collected: true, has_upstream: true` | `known` |
//!
//!    `unknown` is never inferred from a *missing* field, from zeroed counts,
//!    or from an empty branch: it is `collected == false`, sent deliberately,
//!    and `is_git_unknown` below is the single predicate that says so.
//!
//! Why not widen: the encoding is decided by two peers, and the browser's
//! decoder lives in `webui/`, which protocol v1 is already shipped against. A
//! wire change that the SPA is not changed to match in the same commit does not
//! make the model narrower — it makes the two halves disagree, which is a worse
//! failure than a bool pair with a documented reading. If a later turn does
//! widen `GitBar` to a tagged union, this function is the only host-side place
//! that changes, and `git_bar_never_claims_an_upstream_it_has_not_looked_for`
//! is the test that would then be rewritten rather than deleted.
//!
//! What is **not** acceptable, and is what this module exists to prevent, is
//! the browser guessing either fact. Both arrive as data.

use crate::agents::status::DisplayStatus;
use crate::contracts::domain::{AgentDef, PtySize, TabId};
use crate::git::status::WorktreeStatus;
use crate::web::protocol::{
    Delta, DialogOutcome, Geometry, GitBar, ProjectId, ProjectView, SessionPhase, SessionStatus,
    SessionView, StatusBucket, TerminalId, TerminalRole, TerminalView,
};
use crate::web::server::HostState;

use super::TerminalStreams;

// ===========================================================================
// Git (R2)
// ===========================================================================

/// The git facts for one session, as the desktop holds them.
///
/// `status` is `None` exactly when this worktree's status has not been
/// collected yet — a freshly created tab, or a project whose background refresh
/// has not landed. That `None` is the `unknown` arm of the union, and it is the
/// *only* thing that produces it.
#[derive(Clone, Copy, Debug)]
pub struct GitFacts<'a> {
    /// The cached worktree status, or `None` when nothing has been collected.
    pub status: Option<&'a WorktreeStatus>,
    /// The tab's recorded branch name, shown while the status is uncollected so
    /// the row is not blank. Never treated as evidence about the upstream.
    pub fallback_branch: &'a str,
}

/// Build the wire [`GitBar`] from the desktop's cached status. See the module
/// doc for the R2 mapping this implements.
pub fn git_bar(facts: GitFacts<'_>) -> GitBar {
    match facts.status {
        Some(status) => GitBar {
            branch: Some(status.branch.clone()),
            added: status.changes.added,
            modified: status.changes.modified,
            removed: status.changes.deleted,
            ahead: status.ahead,
            behind: status.behind,
            drift: status.base_drift,
            // The `no_upstream` / `known` split. Only ever read by the browser
            // once `collected` is true.
            has_upstream: status.upstream.is_some(),
            files_changed: status.changes.added + status.changes.modified + status.changes.deleted,
            collected: true,
        },
        None => GitBar {
            // The branch name is a persisted tab field, not a git observation,
            // so it is safe to show. Everything that *would* be a git
            // observation stays zero, and `collected: false` says why.
            branch: (!facts.fallback_branch.is_empty()).then(|| facts.fallback_branch.to_string()),
            added: 0,
            modified: 0,
            removed: 0,
            ahead: 0,
            behind: 0,
            drift: 0,
            // Not "there is no upstream" — `collected: false` is what the
            // browser reads, and it reads it first. Set to `false` rather than
            // left to chance so the impossible fourth state
            // (`collected: false, has_upstream: true`) is unrepresentable in
            // anything this host emits.
            has_upstream: false,
            files_changed: 0,
            collected: false,
        },
    }
}

/// The single predicate for "the host has not looked at this worktree's git
/// yet", so no caller re-derives it from zeroed counts or a missing branch.
pub fn is_git_unknown(git: &GitBar) -> bool {
    !git.collected
}

/// Whether this agent has an explicit lifecycle integration, i.e. whether its
/// statuses are reported facts rather than absent ones
/// ([`SessionView::lifecycle_reporting`], turn 2 §5.1's "unknown stays
/// unknown").
///
/// Derived from the same function that decides whether to *attach* an
/// integration at launch, so the flag cannot drift from the behaviour it
/// describes: an agent FlightDeck wires hooks into reports lifecycle, and a
/// custom command it cannot safely pass flags to does not.
pub fn lifecycle_reporting(agent: Option<&AgentDef>) -> bool {
    agent
        .map(|def| crate::agents::setup::status_backend(def).is_some())
        .unwrap_or(false)
}

// ===========================================================================
// Terminals
// ===========================================================================

/// One terminal's facts, as the TUI reads them off a
/// [`crate::terminal::session::Session`].
#[derive(Clone, Debug)]
pub struct TerminalFacts {
    /// The stable id (see [`super::primary_terminal_id`]).
    pub terminal_id: TerminalId,
    /// What it hosts.
    pub role: TerminalRole,
    /// Tab title.
    pub title: String,
    /// The host-owned grid (D4).
    pub geometry: Geometry,
    /// Whether the process is still running.
    pub alive: bool,
    /// Exit code, when it exited normally.
    pub exit_code: Option<i32>,
}

impl TerminalFacts {
    /// The wire view, taking the byte-stream numbers from the registry.
    pub fn view(&self, session_id: &TabId, streams: &TerminalStreams) -> TerminalView {
        TerminalView {
            terminal_id: self.terminal_id.clone(),
            session_id: session_id.clone(),
            role: self.role,
            title: self.title.clone(),
            geometry: self.geometry,
            byte_len: streams.byte_len(&self.terminal_id),
            replay_from: streams.replay_from(&self.terminal_id),
            alive: self.alive,
            exit_code: self.exit_code,
        }
    }
}

/// A grid from the desktop's own [`PtySize`], so the browser letterboxes the
/// number the PTY actually has.
pub fn geometry_of(size: PtySize) -> Geometry {
    Geometry::from(size)
}

// ===========================================================================
// Sessions and projects
// ===========================================================================

/// One session's facts, as the TUI reads them off a `RuntimeTab`.
pub struct SessionFacts<'a> {
    /// Owning project.
    pub project_id: &'a ProjectId,
    /// The Agent Tab id — the one identity a session has in this product.
    pub tab_id: &'a str,
    /// Session name (== worktree == branch leaf).
    pub name: &'a str,
    /// Configured agent key.
    pub agent: &'a str,
    /// The agent's definition, for the display name and the lifecycle fact.
    /// `None` for a tab whose configured agent is no longer in the config.
    pub agent_def: Option<&'a AgentDef>,
    /// `Creating` while the worktree is still being materialised.
    pub phase: SessionPhase,
    /// The desktop's combined status.
    pub display: DisplayStatus,
    /// Seconds in the current (or last) turn.
    pub running_time_secs: u64,
    /// Git, per R2.
    pub git: GitFacts<'a>,
    /// The magenta `[recovered]` chip.
    pub recovered: bool,
    /// The cyan `[existing]` chip.
    pub attached_existing_branch: bool,
    /// The session's terminals, in tab order.
    pub terminals: Vec<TerminalFacts>,
}

/// Build one [`SessionView`].
pub fn session_view(facts: &SessionFacts<'_>, streams: &TerminalStreams) -> SessionView {
    let session_id = TabId(facts.tab_id.to_string());
    SessionView {
        session_id: session_id.clone(),
        project_id: facts.project_id.clone(),
        name: facts.name.to_string(),
        agent: facts.agent.to_string(),
        agent_display_name: facts
            .agent_def
            .map(|def| def.display_name.clone())
            // A tab whose agent has been removed from the config still has a
            // name to show, and the honest one is the key it was launched with
            // — not a blank, which the browser would have to guess about.
            .unwrap_or_else(|| facts.agent.to_string()),
        phase: facts.phase,
        status: SessionStatus::from_display(facts.display, facts.running_time_secs),
        git: git_bar(facts.git),
        terminals: facts
            .terminals
            .iter()
            .map(|t| t.view(&session_id, streams))
            .collect(),
        lifecycle_reporting: lifecycle_reporting(facts.agent_def),
        recovered: facts.recovered,
        attached_existing_branch: facts.attached_existing_branch,
    }
}

/// Build one [`ProjectView`], including the precedence-ordered project dot.
pub fn project_view(
    project_id: &ProjectId,
    name: &str,
    root: &str,
    base_branch: &str,
    sessions: Vec<SessionView>,
) -> ProjectView {
    let dot = StatusBucket::rollup(sessions.iter().map(|s| s.status.bucket));
    ProjectView {
        project_id: project_id.clone(),
        name: name.to_string(),
        root: root.to_string(),
        base_branch: base_branch.to_string(),
        dot,
        sessions,
    }
}

// ===========================================================================
// The delta the TUI sends alongside a publish
// ===========================================================================

/// Diff two published [`HostState`]s into the [`Delta`] frames that honestly
/// describe the change.
///
/// This exists because `WebServerHandle::publish_state` is deliberately **not**
/// a broadcast: it changes what the *next* attach sees and nothing else, on the
/// stated grounds that only the host knows which delta truthfully describes a
/// change it just made, and a server-side diff would invent one. The TUI
/// therefore does both, and this is the "both" — one function, one place to
/// look, unit-tested against the cases that matter.
///
/// Three things it deliberately does **not** emit:
///
/// * **No delta for `byte_len` / `replay_from`.** Those move on every tick a
///   terminal prints, and a `TerminalUpsert` per tick would be a state-change
///   frame riding alongside the byte frame that already carries the same fact.
///   Only the fields the *row* renders — title, role, geometry, liveness —
///   produce a terminal delta.
/// * **No top-level `Geometry` delta.** [`HostState::geometry`] is the selected
///   terminal's grid, which is also on that terminal's [`TerminalView`]; emitting
///   both would tell the browser the same thing twice, and the per-terminal one
///   is the one that names which terminal it is about.
/// * **No delta at all for a change it cannot describe.** When a project's own
///   identity fields move, the whole [`Delta::ProjectUpsert`] is sent (it carries
///   the sessions) instead of a project delta *and* a pile of session deltas
///   describing rows the upsert already replaced.
///
/// `previous` is the last state published. On the very first publish, pass
/// [`HostState::default`]'s equivalent — an empty state — and every project
/// arrives as an upsert, which is correct: nobody has been told about any of it.
pub fn deltas(previous: &HostState, next: &HostState) -> Vec<Delta> {
    let mut out = Vec::new();

    for project in &next.projects {
        let Some(before) = previous
            .projects
            .iter()
            .find(|p| p.project_id == project.project_id)
        else {
            out.push(Delta::ProjectUpsert(project.clone()));
            continue;
        };
        if before.name != project.name
            || before.root != project.root
            || before.base_branch != project.base_branch
        {
            // The upsert carries the sessions, so diffing them as well would
            // describe the same change twice.
            out.push(Delta::ProjectUpsert(project.clone()));
            continue;
        }
        if before.dot != project.dot {
            out.push(Delta::ProjectDot {
                project_id: project.project_id.clone(),
                dot: project.dot,
            });
        }
        for session in &project.sessions {
            match before
                .sessions
                .iter()
                .find(|s| s.session_id == session.session_id)
            {
                None => out.push(Delta::SessionUpsert(session.clone())),
                Some(was) => session_deltas(was, session, &mut out),
            }
        }
        for was in &before.sessions {
            if !project
                .sessions
                .iter()
                .any(|s| s.session_id == was.session_id)
            {
                out.push(Delta::SessionRemoved {
                    session_id: was.session_id.clone(),
                });
            }
        }
    }
    for before in &previous.projects {
        if !next
            .projects
            .iter()
            .any(|p| p.project_id == before.project_id)
        {
            out.push(Delta::ProjectRemoved {
                project_id: before.project_id.clone(),
            });
        }
    }

    if previous.selection != next.selection {
        out.push(Delta::Selection(next.selection.clone()));
    }

    // Only genuinely new feed entries, matched by id — the backfill list is
    // resent wholesale on every publish and must not replay as N deltas.
    for event in &next.activity {
        if !previous
            .activity
            .iter()
            .any(|e| e.event_id == event.event_id)
        {
            out.push(Delta::Activity(event.clone()));
        }
    }

    // Dialogs are M2 (D8/D13); this is the fallback that stops a browser
    // keeping a dead modal on screen if a dialog disappears from the published
    // state without anyone announcing it. `Superseded` is the only honest
    // outcome a *diff* can report — it did not witness a decision — so the code
    // path that actually confirms or cancels a dialog must send its own
    // `Delta::DialogClosed` with the real outcome rather than leaning on this.
    match (&previous.dialog, &next.dialog) {
        (None, Some(open)) => out.push(Delta::DialogOpened(open.clone())),
        (Some(was), None) => out.push(Delta::DialogClosed {
            dialog_id: was.dialog_id.clone(),
            outcome: DialogOutcome::Superseded,
        }),
        (Some(was), Some(open)) if was.dialog_id != open.dialog_id => {
            out.push(Delta::DialogClosed {
                dialog_id: was.dialog_id.clone(),
                outcome: DialogOutcome::Superseded,
            });
            out.push(Delta::DialogOpened(open.clone()));
        }
        _ => {}
    }

    out
}

/// The deltas for one session that exists on both sides of the diff.
fn session_deltas(was: &SessionView, now: &SessionView, out: &mut Vec<Delta>) {
    // Fields with no delta of their own. A whole-row upsert is the honest
    // answer; the cheap per-field deltas below exist only for the two things
    // that change often enough to be worth them.
    if was.name != now.name
        || was.agent != now.agent
        || was.agent_display_name != now.agent_display_name
        || was.phase != now.phase
        || was.lifecycle_reporting != now.lifecycle_reporting
        || was.recovered != now.recovered
        || was.attached_existing_branch != now.attached_existing_branch
    {
        out.push(Delta::SessionUpsert(now.clone()));
        return;
    }
    if was.status != now.status {
        out.push(Delta::Status {
            session_id: now.session_id.clone(),
            status: now.status.clone(),
        });
    }
    if was.git != now.git {
        out.push(Delta::Git {
            session_id: now.session_id.clone(),
            git: now.git.clone(),
        });
    }

    for terminal in &now.terminals {
        let Some(before) = was
            .terminals
            .iter()
            .find(|t| t.terminal_id == terminal.terminal_id)
        else {
            out.push(Delta::TerminalUpsert(terminal.clone()));
            continue;
        };
        if before.alive && !terminal.alive {
            out.push(Delta::TerminalClosed {
                terminal_id: terminal.terminal_id.clone(),
                exit_code: terminal.exit_code,
            });
            continue;
        }
        if before.geometry != terminal.geometry {
            out.push(Delta::Geometry {
                terminal_id: terminal.terminal_id.clone(),
                geometry: terminal.geometry,
            });
        }
        // `byte_len` and `replay_from` are deliberately not compared — see the
        // `deltas` doc.
        if before.title != terminal.title
            || before.role != terminal.role
            || before.alive != terminal.alive
        {
            out.push(Delta::TerminalUpsert(terminal.clone()));
        }
    }
    for before in &was.terminals {
        if !now
            .terminals
            .iter()
            .any(|t| t.terminal_id == before.terminal_id)
        {
            out.push(Delta::TerminalClosed {
                terminal_id: before.terminal_id.clone(),
                exit_code: before.exit_code,
            });
        }
    }
}
