//! Mission control over a remote host (`remote-control-pgy3`): the very
//! [`MissionView`] a local window builds, fed from the mirror instead of from
//! `AppState`.
//!
//! The rules — what is a tile, a card or quiet, and in what order — are
//! [`crate::view::mission`]'s, reached through its source-agnostic input
//! ([`MissionSession`]). This module only translates what the wire says about
//! a session into that input:
//!
//! - **Rows** are [`super::views::session_row`], the Projects sidebar's own,
//!   with the age measured from the session's last status change.
//! - **Activity** is what the host stated with the row
//!   ([`SessionView::activity`]), brought forward by what this client saw
//!   happen since ([`ActivityWatch`]): a status or git change dated when its
//!   delta arrived. A host that predates the field is dated from those alone,
//!   so its quiet sessions become cards only once they move.
//! - **Git actions** are the remote git strip's: Push and Finish once the
//!   worktree is ready, never Pull Base (refused from a remote surface).
//! - **Prompts** are the host's detection ([`SessionView::prompt`]), offered
//!   inline only where the host takes an answer
//!   ([`crate::web::protocol::command::ANSWER_PROMPT`]); otherwise the tile
//!   offers to open the session, as it does for a prompt it cannot answer.
//!
//! Pure: the clock arrives as a parameter.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::app::activity::{ActivityProbe, GitFingerprint};
use crate::contracts::{TabActivity, TabId};
use crate::git::status::WorktreeChanges;
use crate::persistence::workspace::MissionScope;
use crate::view::{
    mission_view_of, MissionProject, MissionSession, MissionView, PromptAnswer, PromptButton,
    PromptReply, PromptShape, PromptView,
};
use crate::web::protocol::{
    command as names, AnswerPromptArgs, PendingPrompt, PromptAnswerView, PromptShapeView,
    SessionView,
};

use super::mirror::RemoteWorkspace;
use super::views::{session_git_actions, session_row};

/// What one session did since the host last stated its activity, as this
/// client saw it.
#[derive(Debug, Default)]
struct Watched {
    probe: ActivityProbe,
    seen: TabActivity,
}

/// Dates what happens to each session after the host last sent its row: a
/// status or git change is stamped when this client first sees it, by the
/// same edge rules the host records its own activity with
/// ([`ActivityProbe`]: the first sight is a baseline, not a change). A new
/// snapshot restates everything and starts the watch over.
///
/// Output is not watched: the host's terminals replay their recent bytes on
/// every attach, which would read as every session printing at once.
#[derive(Debug, Default)]
pub struct ActivityWatch {
    /// The mirror's snapshot count the stamps were observed since.
    snapshots: u64,
    sessions: HashMap<TabId, Watched>,
}

impl ActivityWatch {
    pub fn new() -> ActivityWatch {
        ActivityWatch::default()
    }

    /// Look at every session once, at `now_secs` (unix seconds): stamp what
    /// changed since the last look.
    pub fn observe(&mut self, ws: &RemoteWorkspace, now_secs: u64) {
        if ws.snapshots() != self.snapshots {
            self.snapshots = ws.snapshots();
            self.sessions.clear();
        }
        let mut open: HashSet<&TabId> = HashSet::new();
        for session in ws.projects.iter().flat_map(|p| p.sessions.iter()) {
            open.insert(&session.session_id);
            let watched = self.sessions.entry(session.session_id.clone()).or_default();
            let (_, status_changed) = watched.probe.take_edges(session.status.interpreted);
            if status_changed {
                watched.seen.note_status_change(now_secs);
            }
            if session.git.collected {
                let fingerprint = GitFingerprint::from_parts(
                    WorktreeChanges {
                        added: session.git.added,
                        modified: session.git.modified,
                        deleted: session.git.removed,
                    },
                    session.git.ahead,
                );
                if watched.probe.observe_git(fingerprint) {
                    watched.seen.note_git_change(now_secs);
                }
            }
        }
        self.sessions.retain(|id, _| open.contains(id));
    }

    /// Start over (a new link: its first snapshot is the baseline).
    pub fn reset(&mut self) {
        *self = ActivityWatch::default();
    }

    /// `session`'s activity: the host's, each moment brought forward to what
    /// this client saw since, if later.
    pub fn activity(&self, session: &SessionView) -> TabActivity {
        let host = session.activity.unwrap_or_default();
        let Some(seen) = self.sessions.get(&session.session_id).map(|w| w.seen) else {
            return host;
        };
        TabActivity {
            last_output_at: host.last_output_at.max(seen.last_output_at),
            last_status_change_at: host.last_status_change_at.max(seen.last_status_change_at),
            last_git_change_at: host.last_git_change_at.max(seen.last_git_change_at),
        }
    }
}

/// Whether the host takes inline prompt answers from this client: it lists
/// [`names::ANSWER_PROMPT`] and does not refuse it.
pub fn answers_prompts(ws: &RemoteWorkspace) -> bool {
    ws.command(names::ANSWER_PROMPT)
        .is_some_and(|c| c.refusal.is_none())
}

/// The inline view of a pending prompt. `answerable` is whether the host
/// takes inline answers at all; when it does not — or none of the answers is
/// one this build knows — the prompt is shown to be answered in the session.
pub fn prompt_view(prompt: &PendingPrompt, answerable: bool) -> PromptView {
    let buttons: Vec<PromptButton> = if answerable {
        prompt
            .buttons
            .iter()
            .filter_map(|b| {
                let answer = match b.answer {
                    PromptAnswerView::Approve => PromptAnswer::Approve,
                    PromptAnswerView::Deny => PromptAnswer::Deny,
                    PromptAnswerView::Option { index } => PromptAnswer::Option(index),
                    PromptAnswerView::Unrecognized => return None,
                };
                Some(PromptButton {
                    label: b.label.clone(),
                    answer,
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let shape = match prompt.shape {
        _ if buttons.is_empty() => PromptShape::Unsupported,
        // One of its answers is unknown here: answering the rest inline
        // would be offering half the question.
        _ if buttons.len() != prompt.buttons.len() => PromptShape::Unsupported,
        PromptShapeView::Binary => PromptShape::Binary,
        PromptShapeView::SingleSelect => PromptShape::SingleSelect,
        PromptShapeView::Unsupported => PromptShape::Unsupported,
    };
    PromptView {
        prompt_id: prompt.prompt_id.clone(),
        buttons: if shape == PromptShape::Unsupported {
            Vec::new()
        } else {
            buttons
        },
        shape,
        text: prompt.text.clone(),
    }
}

/// The `args` of [`names::ANSWER_PROMPT`] for `reply`.
pub fn answer_args(reply: &PromptReply) -> serde_json::Value {
    let answer = match reply.answer {
        PromptAnswer::Approve => PromptAnswerView::Approve,
        PromptAnswer::Deny => PromptAnswerView::Deny,
        PromptAnswer::Option(index) => PromptAnswerView::Option { index },
    };
    serde_json::to_value(AnswerPromptArgs {
        session_id: TabId(reply.key.tab_id.clone()),
        prompt_id: reply.prompt_id.clone(),
        answer,
    })
    .unwrap_or_default()
}

/// Mission control over the host `ws` mirrors, under `scope`, with ages
/// against `now_secs` (unix seconds, this machine's clock). Each waiting
/// tile carries its prompt unless its id is in `answered` (answered from
/// here, and not yet withdrawn by the host).
pub fn mission_view(
    ws: &RemoteWorkspace,
    watch: &ActivityWatch,
    scope: &MissionScope,
    now_secs: u64,
    answered: &[String],
) -> MissionView {
    let selected = ws.selected_session().map(|s| s.session_id.clone());
    let projects: Vec<MissionProject<'_>> = ws
        .projects
        .iter()
        .map(|project| MissionProject {
            name: &project.name,
            root: Path::new(&project.root),
            sessions: project
                .sessions
                .iter()
                .enumerate()
                .map(|(index, session)| {
                    let activity = watch.activity(session);
                    let mut row = session_row(
                        index,
                        session,
                        selected.as_ref() == Some(&session.session_id),
                    );
                    // The wire's turn timer is not kept by every host; the
                    // last status change is, as it is locally.
                    if let Some(at) = activity.last_status_change_at {
                        row.status_since_secs = Some(now_secs.saturating_sub(at));
                    }
                    MissionSession {
                        row,
                        status: session.status.interpreted,
                        activity,
                        git_actions: session_git_actions(session),
                        git_known: session.git.collected,
                    }
                })
                .collect(),
        })
        .collect();
    let mut view = mission_view_of(&projects, scope, now_secs);
    let answerable = answers_prompts(ws);
    for tile in view.tiles.iter_mut().filter(|t| t.needs_you) {
        let prompt = ws
            .projects
            .get(tile.key.project)
            .and_then(|p| p.sessions.get(tile.tab_index))
            .and_then(|s| s.prompt.as_ref())
            .filter(|p| !answered.contains(&p.prompt_id));
        tile.prompt = prompt.map(|p| prompt_view(p, answerable));
    }
    view
}

#[cfg(test)]
mod tests;
