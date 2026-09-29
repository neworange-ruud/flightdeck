//! Mission control's view model (designs A2 and A3): every live session
//! across **all** open projects as a tile, recently active but quiet ones as
//! compact cards, and a count of the rest.
//!
//! What shows up, in the design's words ("Mission control rules"):
//!
//! - **Tiles**: every session that is working, waiting for input or needs
//!   attention ([`is_active`]), whatever the scope's recency window says.
//! - **Cards**: sessions with output, a status change or a git change inside
//!   the scope's window ([`is_recent`]) that are not live.
//! - **Quiet**: everything else in scope, counted, not shown.
//! - **Order**: needs you, then working, then most recently updated — the V1
//!   ordering of [`compare_sessions`], tie-broken by project so two projects'
//!   identically named tabs never swap between frames.
//!
//! The scope's project filter narrows all three. The needs-you count does
//! not: it is the view switch's badge, an alert about the whole workspace
//! that must not go quiet because the grid is filtered to another project.
//!
//! Grid geometry and keyboard movement ([`grid_columns`], [`move_selection`])
//! are here too, so the GPUI layer only draws what these functions decide.
//!
//! Pure, like the rest of [`crate::view`]: the clock arrives as parameters.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

use crate::app::activity::{compare_sessions, is_active, is_recent, needs_you, SessionActivity};
use crate::app::commands::Command;
use crate::app::state::AppState;
use crate::contracts::{InterpretedStatus, TabActivity};
use crate::git::status::WorktreeStatus;
use crate::persistence::workspace::MissionScope;
use crate::view::agent::{agent_row_view, AgentBadge, AgentRowView, UpstreamState};
use crate::view::git_strip::git_actions;

/// One open project, as Mission control reads it.
#[derive(Clone, Copy)]
pub struct MissionSource<'a> {
    /// Display name (the folder name).
    pub name: &'a str,
    /// Repository root: what the scope's project filter names.
    pub root: &'a Path,
    /// Its application state.
    pub state: &'a AppState,
    /// Its git-status cache, keyed by tab id.
    pub git: &'a HashMap<String, WorktreeStatus>,
}

/// Which session, workspace-wide: the project's position in the workspace
/// and the tab's stable id (tab ids are only unique within one project).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    /// Index of the project in the workspace.
    pub project: usize,
    /// The Agent Tab's id.
    pub tab_id: String,
}

/// A live session's tile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionTile {
    /// Which session.
    pub key: SessionKey,
    /// The project's display name.
    pub project_name: String,
    /// The tab's position in its project (what `SwitchAgentTab(Index)` takes).
    pub tab_index: usize,
    /// The Alt-N that reaches this tile in Mission control: its 1-based
    /// position in tile order, for the first nine tiles.
    pub alt_index: Option<u8>,
    /// The interpreted status that made it live.
    pub status: InterpretedStatus,
    /// Waiting for input or needs attention (ranks first, drawn loudest).
    pub needs_you: bool,
    /// Everything the Projects view's row shows about it: name, agent,
    /// branch, badge, status text and age, diff.
    pub row: AgentRowView,
}

/// Which of a session's three recorded moments was the latest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastEventKind {
    /// The agent's terminal printed something.
    Output,
    /// Its interpreted status changed.
    StatusChange,
    /// Its worktree's changes or ahead count moved.
    GitChange,
}

/// A session's most recent activity, and how long ago it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastEvent {
    /// What happened.
    pub kind: LastEventKind,
    /// Seconds before `now` (zero for a timestamp from the future).
    pub secs_ago: u64,
}

impl LastEvent {
    /// The latest of `activity`'s moments, or `None` when nothing was ever
    /// recorded. On a tie git wins over status over output: the more
    /// meaningful event is the one to name.
    pub fn of(activity: &TabActivity, now_secs: u64) -> Option<LastEvent> {
        [
            (activity.last_output_at, LastEventKind::Output),
            (activity.last_status_change_at, LastEventKind::StatusChange),
            (activity.last_git_change_at, LastEventKind::GitChange),
        ]
        .into_iter()
        .filter_map(|(at, kind)| at.map(|at| (at, kind)))
        .max_by_key(|(at, _)| *at)
        .map(|(at, kind)| LastEvent {
            kind,
            secs_ago: now_secs.saturating_sub(at),
        })
    }
}

/// The one git action a card offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardAction {
    /// Finish (local merge-back).
    Finish,
    /// Pull the base branch in the base folder.
    PullBase,
    /// Push the session's branch.
    Push,
}

impl CardAction {
    /// The command its chord dispatches (Ctrl-f, Ctrl-u, Ctrl-p), payload
    /// and all, so the card cannot mean something the chord does not.
    pub fn command(self) -> Command {
        match self {
            CardAction::Finish => Command::FinishLocalMerge { confirm: false },
            CardAction::PullBase => Command::PullBase,
            CardAction::Push => Command::PushBranch { confirm: None },
        }
    }

    /// The button's label.
    pub fn label(self) -> &'static str {
        match self {
            CardAction::Finish => "Finish",
            CardAction::PullBase => "Pull base",
            CardAction::Push => "Push",
        }
    }
}

/// A recent-but-quiet session's compact card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionCard {
    /// Which session.
    pub key: SessionKey,
    /// The project's display name.
    pub project_name: String,
    /// The tab's position in its project.
    pub tab_index: usize,
    /// The Projects view's row for it.
    pub row: AgentRowView,
    /// Its latest recorded activity.
    pub last_event: Option<LastEvent>,
    /// One line: its state, then what git knows (`Idle · 2 files changed,
    /// not pushed`).
    pub detail: String,
    /// The one context action, or `None` when none applies.
    pub action: Option<CardAction>,
}

/// Everything Mission control draws.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MissionView {
    /// Live sessions, in V1 order.
    pub tiles: Vec<MissionTile>,
    /// Recent, not live sessions, in V1 order.
    pub cards: Vec<MissionCard>,
    /// In-scope sessions that are neither: hidden, counted.
    pub quiet_count: usize,
    /// Sessions needing the user across every open project (not filtered).
    pub needs_you_count: usize,
    /// Tiles that are working rather than waiting on the user.
    pub working_count: usize,
    /// The project filter, resolved to a workspace index; `None` for every
    /// project (including a filter naming a project that is no longer open).
    pub project_filter: Option<usize>,
}

impl MissionView {
    /// The tile for `key`, with its position in tile order.
    pub fn tile(&self, key: &SessionKey) -> Option<(usize, &MissionTile)> {
        self.tiles.iter().enumerate().find(|(_, t)| &t.key == key)
    }
}

/// One in-scope session while the view is being sorted.
struct Candidate<'a> {
    project: usize,
    tab_index: usize,
    status: InterpretedStatus,
    activity: SessionActivity<'a>,
}

/// Build Mission control for `projects` (in workspace order) under `scope`.
///
/// `now_ms` is the clock in milliseconds (display status), `now_secs` in unix
/// seconds (activity, ages) — the same pair the other view models take.
pub fn mission_view(
    projects: &[MissionSource<'_>],
    scope: &MissionScope,
    now_ms: u64,
    now_secs: u64,
) -> MissionView {
    let project_filter = scope
        .project
        .as_deref()
        .and_then(|root| projects.iter().position(|p| p.root == Path::new(root)));
    let window = scope.window.window_secs();

    let mut needs_you_count = 0;
    let mut candidates: Vec<Candidate<'_>> = Vec::new();
    for (pi, source) in projects.iter().enumerate() {
        for (ti, tab) in source.state.tabs.iter().enumerate() {
            let status = tab.display_status(now_ms).interpreted;
            if needs_you(status) {
                needs_you_count += 1;
            }
            if project_filter.is_some_and(|only| only != pi) {
                continue;
            }
            candidates.push(Candidate {
                project: pi,
                tab_index: ti,
                status,
                activity: SessionActivity {
                    id: &tab.meta.id,
                    name: &tab.meta.name,
                    status,
                    activity: tab.meta.activity,
                },
            });
        }
    }
    candidates.sort_by(|a, b| {
        compare_sessions(&a.activity, &b.activity)
            .then_with(|| a.project.cmp(&b.project))
            .then(Ordering::Equal)
    });

    let mut view = MissionView {
        needs_you_count,
        project_filter,
        ..MissionView::default()
    };
    for c in candidates {
        let source = &projects[c.project];
        let Some(row) = agent_row_view(source.state, c.tab_index, source.git, now_ms, now_secs)
        else {
            continue;
        };
        let key = SessionKey {
            project: c.project,
            tab_id: row.id.clone(),
        };
        if is_active(c.status) {
            let position = view.tiles.len();
            let needs = needs_you(c.status);
            if !needs {
                view.working_count += 1;
            }
            view.tiles.push(MissionTile {
                key,
                project_name: source.name.to_string(),
                tab_index: c.tab_index,
                alt_index: (position < 9).then(|| position as u8 + 1),
                status: c.status,
                needs_you: needs,
                row,
            });
        } else if is_recent(&c.activity.activity, now_secs, window) {
            let tab = &source.state.tabs[c.tab_index];
            let git = source.git.get(&tab.meta.id);
            view.cards.push(MissionCard {
                key,
                project_name: source.name.to_string(),
                tab_index: c.tab_index,
                last_event: LastEvent::of(&c.activity.activity, now_secs),
                detail: card_detail(&row, c.status),
                action: card_action(&row, git_actions(tab), git.is_some()),
                row,
            });
        } else {
            view.quiet_count += 1;
        }
    }
    view
}

/// A card's one line: what the session is doing, then what git says.
fn card_detail(row: &AgentRowView, status: InterpretedStatus) -> String {
    let state = match row.manual_status {
        Some(manual) => capitalise(manual.as_str()),
        None => match row.badge {
            AgentBadge::Done => "Done".to_string(),
            AgentBadge::Error => "Error".to_string(),
            AgentBadge::Idle => "Idle".to_string(),
            // Starting / Running without a lifecycle signal: not live, but
            // not idle either.
            AgentBadge::Working | AgentBadge::WaitingAttention => match status {
                InterpretedStatus::Starting => "Starting".to_string(),
                _ => "Running".to_string(),
            },
        },
    };
    let mut parts = vec![state];
    if let Some(changes) = row.changes {
        let files = match changes.files {
            0 => "clean".to_string(),
            1 => "1 file changed".to_string(),
            n => format!("{n} files changed"),
        };
        // Only say "not pushed" about work that exists: a clean branch with
        // no upstream has nothing to push yet.
        let unpushed = match &row.upstream {
            UpstreamState::None if !changes.is_clean() => Some("not pushed".to_string()),
            UpstreamState::Tracking { ahead, .. } if *ahead > 0 => Some(format!("{ahead} to push")),
            _ => None,
        };
        parts.push(match unpushed {
            Some(u) => format!("{files}, {u}"),
            None => files,
        });
    }
    if row.base_drift > 0 {
        parts.push(format!("base +{}", row.base_drift));
    }
    parts.join(" · ")
}

fn capitalise(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The card's one context action: the next git step a quiet session most
/// plausibly wants, among those its git strip would enable.
///
/// 1. A finished agent's work goes home: **Finish**.
/// 2. The target moved on: **Pull base**.
/// 3. Work the remote has not seen (uncommitted changes, or commits ahead of
///    the upstream): **Push**.
/// 4. Otherwise **Finish**, when git status is known — a clean, pushed,
///    up-to-date session is done with. Unknown git state offers nothing.
fn card_action(
    row: &AgentRowView,
    actions: crate::view::GitActions,
    git_known: bool,
) -> Option<CardAction> {
    if actions.finish && row.badge == AgentBadge::Done {
        return Some(CardAction::Finish);
    }
    if !git_known {
        return None;
    }
    if actions.pull_base && row.base_drift > 0 {
        return Some(CardAction::PullBase);
    }
    let dirty = row.changes.is_some_and(|c| !c.is_clean());
    let ahead = matches!(row.upstream, UpstreamState::Tracking { ahead, .. } if ahead > 0);
    if actions.push && (dirty || ahead) {
        return Some(CardAction::Push);
    }
    actions.finish.then_some(CardAction::Finish)
}

/// Columns of the tile grid for `count` tiles: one fills the area, two sit
/// side by side, three or four are 2×2, five or six 3×2, and more keep three
/// columns and scroll.
pub fn grid_columns(count: usize) -> usize {
    match count {
        0 | 1 => 1,
        2..=4 => 2,
        _ => 3,
    }
}

/// Rows of the grid that fit the area at once; more rows scroll. One row for
/// up to two tiles, two for anything larger.
pub fn grid_visible_rows(count: usize) -> usize {
    if count <= 2 {
        1
    } else {
        2
    }
}

/// An arrow key in the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridMove {
    Up,
    Down,
    Left,
    Right,
}

/// The tile an arrow moves the selection to, in reading order over a grid of
/// `count` tiles and `columns` columns. Left and Right step through the
/// order (wrapping onto the neighbouring row); Up and Down move a whole row,
/// landing on the last tile when the row below is short. Never leaves the
/// grid: at an edge the selection stays put.
pub fn move_selection(current: usize, count: usize, columns: usize, mv: GridMove) -> usize {
    if count == 0 {
        return 0;
    }
    let current = current.min(count - 1);
    let columns = columns.max(1);
    match mv {
        GridMove::Left => current.saturating_sub(1),
        GridMove::Right => (current + 1).min(count - 1),
        GridMove::Up => current.checked_sub(columns).unwrap_or(current),
        GridMove::Down => {
            let row = current / columns;
            let last_row = (count - 1) / columns;
            if row == last_row {
                current
            } else {
                (current + columns).min(count - 1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::PtySize;
    use crate::git::status::{LineStats, WorktreeChanges};
    use crate::host::testing::state_with_tabs;
    use crate::persistence::workspace::RecentScope;
    use crate::testing::FakePty;
    use std::path::PathBuf;

    const NOW: u64 = 2_000_000_000;
    const HOUR: u64 = 3_600;

    /// A project whose tabs are `(name, status, last activity seconds ago)`;
    /// `status: None` leaves the agent unspawned (Unknown, never live).
    struct P {
        name: &'static str,
        root: PathBuf,
        state: AppState,
        git: HashMap<String, WorktreeStatus>,
        _pty: FakePty,
    }

    fn project(name: &'static str, tabs: &[(&str, Option<InterpretedStatus>, Option<u64>)]) -> P {
        let names: Vec<&str> = tabs.iter().map(|t| t.0).collect();
        let mut state = state_with_tabs(name, &names);
        let pty = FakePty::new();
        for (i, (_, status, ago)) in tabs.iter().enumerate() {
            if let Some(status) = status {
                pty.queue_session();
                state.tabs[i]
                    .session
                    .spawn_primary(&pty, "agent", &[], Path::new("/wt"), PtySize::default())
                    .unwrap();
                state.tabs[i].interpreted = Some(*status);
            }
            state.tabs[i].meta.activity.last_output_at = ago.map(|a| NOW - a);
        }
        P {
            name,
            root: PathBuf::from(format!("/{name}")),
            state,
            git: HashMap::new(),
            _pty: pty,
        }
    }

    fn sources(ps: &[P]) -> Vec<MissionSource<'_>> {
        ps.iter()
            .map(|p| MissionSource {
                name: p.name,
                root: &p.root,
                state: &p.state,
                git: &p.git,
            })
            .collect()
    }

    fn build(ps: &[P], scope: &MissionScope) -> MissionView {
        mission_view(&sources(ps), scope, 0, NOW)
    }

    fn tile_names(v: &MissionView) -> Vec<&str> {
        v.tiles.iter().map(|t| t.row.name.as_str()).collect()
    }

    fn card_names(v: &MissionView) -> Vec<&str> {
        v.cards.iter().map(|c| c.row.name.as_str()).collect()
    }

    fn ws(changes: u32, upstream: Option<&str>, ahead: u32, drift: u32) -> WorktreeStatus {
        WorktreeStatus {
            branch: "b".to_string(),
            base_branch: "main".to_string(),
            dirty: changes > 0,
            changes: WorktreeChanges {
                modified: changes,
                ..Default::default()
            },
            lines: LineStats {
                added: 3 * changes,
                removed: changes,
            },
            ahead,
            behind: 0,
            upstream: upstream.map(str::to_string),
            base_drift: drift,
            worktree_path: PathBuf::from("/wt"),
        }
    }

    use InterpretedStatus::*;

    #[test]
    fn tiles_span_every_project_in_v1_order() {
        let ps = [
            project(
                "alpha",
                &[
                    ("a-work-old", Some(Working), Some(600)),
                    ("a-idle", Some(Idle), Some(60)),
                    ("a-wait", Some(WaitingForInput), Some(900)),
                ],
            ),
            project(
                "beta",
                &[
                    ("b-work-new", Some(Working), Some(5)),
                    ("b-attn", Some(NeedsAttention), Some(30)),
                ],
            ),
        ];
        let v = build(&ps, &MissionScope::default());
        // Needs you (fresher first), then working (fresher first).
        assert_eq!(
            tile_names(&v),
            ["b-attn", "a-wait", "b-work-new", "a-work-old"]
        );
        assert_eq!(
            v.tiles.iter().map(|t| t.alt_index).collect::<Vec<_>>(),
            [Some(1), Some(2), Some(3), Some(4)]
        );
        assert_eq!(
            v.tiles[0].key,
            SessionKey {
                project: 1,
                tab_id: "t1".to_string()
            }
        );
        assert_eq!(v.tiles[0].tab_index, 1);
        assert_eq!(v.tiles[0].project_name, "beta");
        assert!(v.tiles[0].needs_you && v.tiles[1].needs_you && !v.tiles[2].needs_you);
        assert_eq!(v.working_count, 2);
        assert_eq!(v.needs_you_count, 2);
        // The idle one, active a minute ago, is a card; nothing is quiet.
        assert_eq!(card_names(&v), ["a-idle"]);
        assert_eq!(v.quiet_count, 0);
    }

    #[test]
    fn only_the_first_nine_tiles_have_an_alt_digit() {
        let tabs: Vec<(String, Option<InterpretedStatus>, Option<u64>)> = (0..11)
            .map(|i| (format!("w{i:02}"), Some(Working), Some(100 + i as u64)))
            .collect();
        let tabs: Vec<(&str, Option<InterpretedStatus>, Option<u64>)> =
            tabs.iter().map(|(n, s, a)| (n.as_str(), *s, *a)).collect();
        let v = build(&[project("alpha", &tabs)], &MissionScope::default());
        let alts: Vec<Option<u8>> = v.tiles.iter().map(|t| t.alt_index).collect();
        let expected: Vec<Option<u8>> = (1..=9u8).map(Some).chain([None, None]).collect();
        assert_eq!(alts, expected);
    }

    #[test]
    fn identical_tabs_in_two_projects_order_by_project() {
        let ps = [
            project("alpha", &[("same", Some(Working), Some(10))]),
            project("beta", &[("same", Some(Working), Some(10))]),
        ];
        let v = build(&ps, &MissionScope::default());
        let projects: Vec<usize> = v.tiles.iter().map(|t| t.key.project).collect();
        assert_eq!(projects, [0, 1]);
    }

    #[test]
    fn scope_windows_move_sessions_between_cards_and_quiet() {
        let ps = [project(
            "alpha",
            &[
                ("hour", Some(Idle), Some(HOUR)),
                ("two-days", Some(Completed), Some(48 * HOUR)),
                ("month", None, Some(30 * 24 * HOUR)),
                ("never", None, None),
                ("live-but-ancient", Some(Working), Some(90 * 24 * HOUR)),
            ],
        )];
        let with = |window| MissionScope {
            window,
            project: None,
        };
        let day = build(&ps, &with(RecentScope::Day));
        assert_eq!(
            tile_names(&day),
            ["live-but-ancient"],
            "live ignores the window"
        );
        assert_eq!(card_names(&day), ["hour"]);
        assert_eq!(day.quiet_count, 3);

        let week = build(&ps, &with(RecentScope::Week));
        assert_eq!(card_names(&week), ["hour", "two-days"]);
        assert_eq!(week.quiet_count, 2);

        let all = build(&ps, &with(RecentScope::All));
        assert_eq!(card_names(&all), ["hour", "two-days", "month"]);
        assert_eq!(
            all.quiet_count, 1,
            "no activity at all is quiet even for All"
        );
    }

    #[test]
    fn the_project_filter_narrows_everything_but_the_badge() {
        let ps = [
            project(
                "alpha",
                &[
                    ("a-wait", Some(WaitingForInput), Some(10)),
                    ("a-old", None, Some(40 * 24 * HOUR)),
                ],
            ),
            project(
                "beta",
                &[
                    ("b-work", Some(Working), Some(10)),
                    ("b-recent", Some(Idle), Some(HOUR)),
                    ("b-quiet", None, None),
                ],
            ),
        ];
        let beta = MissionScope {
            window: RecentScope::Day,
            project: Some("/beta".to_string()),
        };
        let v = build(&ps, &beta);
        assert_eq!(v.project_filter, Some(1));
        assert_eq!(tile_names(&v), ["b-work"]);
        assert_eq!(card_names(&v), ["b-recent"]);
        assert_eq!(v.quiet_count, 1, "alpha's quiet tab is filtered, not quiet");
        assert_eq!(v.needs_you_count, 1, "alpha still needs you");
        // The one tile is reached by Alt-1 inside the filtered grid.
        assert_eq!(v.tiles[0].alt_index, Some(1));

        // A filter naming a project that is no longer open shows everything.
        let gone = MissionScope {
            window: RecentScope::Day,
            project: Some("/gamma".to_string()),
        };
        let v = build(&ps, &gone);
        assert_eq!(v.project_filter, None);
        assert_eq!(tile_names(&v), ["a-wait", "b-work"]);
    }

    #[test]
    fn needs_you_counts_waiting_and_attention_only() {
        let ps = [project(
            "alpha",
            &[
                ("w", Some(WaitingForInput), Some(1)),
                ("n", Some(NeedsAttention), Some(1)),
                ("k", Some(Working), Some(1)),
                ("f", Some(Failed), Some(1)),
                ("i", Some(Idle), Some(1)),
            ],
        )];
        let v = build(&ps, &MissionScope::default());
        assert_eq!(v.needs_you_count, 2);
        assert_eq!(v.working_count, 1);
        assert_eq!(v.tiles.len(), 3);
    }

    #[test]
    fn cards_carry_last_event_detail_and_one_action() {
        let mut p = project(
            "alpha",
            &[
                ("done", None, Some(2 * HOUR)),
                ("behind", Some(Idle), Some(3 * HOUR)),
                ("unpushed", Some(Idle), Some(6 * HOUR)),
                ("unknown", Some(Idle), Some(7 * HOUR)),
            ],
        );
        // A primary that exited 0 is "done".
        let pty = FakePty::new();
        let handle = pty.queue_session();
        p.state.tabs[0]
            .session
            .spawn_primary(&pty, "agent", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        handle.set_state(crate::contracts::ProcessState::Exited(0));
        p.state.tabs[1].meta.activity.last_git_change_at = Some(NOW - 3 * HOUR + 1);
        p.git
            .insert("t0".to_string(), ws(0, Some("origin/done"), 0, 0));
        p.git
            .insert("t1".to_string(), ws(0, Some("origin/behind"), 0, 24));
        p.git.insert("t2".to_string(), ws(2, None, 0, 0));
        let v = build(std::slice::from_ref(&p), &MissionScope::default());
        let by_name = |n: &str| v.cards.iter().find(|c| c.row.name == n).unwrap();

        let done = by_name("done");
        assert_eq!(done.action, Some(CardAction::Finish));
        assert_eq!(done.detail, "Done · clean");
        assert_eq!(
            done.last_event,
            Some(LastEvent {
                kind: LastEventKind::Output,
                secs_ago: 2 * HOUR
            })
        );

        let behind = by_name("behind");
        assert_eq!(behind.action, Some(CardAction::PullBase));
        assert_eq!(behind.detail, "Idle · clean · base +24");
        assert_eq!(
            behind.last_event.map(|e| e.kind),
            Some(LastEventKind::GitChange)
        );

        let unpushed = by_name("unpushed");
        assert_eq!(unpushed.action, Some(CardAction::Push));
        assert_eq!(unpushed.detail, "Idle · 2 files changed, not pushed");

        let unknown = by_name("unknown");
        assert_eq!(unknown.action, None, "no git status, no guess");
        assert_eq!(unknown.detail, "Idle");

        // Card order is recency.
        assert_eq!(card_names(&v), ["done", "behind", "unpushed", "unknown"]);
    }

    #[test]
    fn card_actions_respect_the_git_strip() {
        // A tab on the base branch cannot Finish; a clean pushed one then
        // offers nothing.
        let mut p = project("alpha", &[("on-base", Some(Idle), Some(HOUR))]);
        p.state.tabs[0].meta.runs_on_base = true;
        p.git
            .insert("t0".to_string(), ws(0, Some("origin/main"), 0, 0));
        let v = build(std::slice::from_ref(&p), &MissionScope::default());
        assert_eq!(v.cards[0].action, None);
        // Commits ahead of its upstream: Push.
        p.git
            .insert("t0".to_string(), ws(0, Some("origin/main"), 3, 0));
        let v = build(std::slice::from_ref(&p), &MissionScope::default());
        assert_eq!(v.cards[0].action, Some(CardAction::Push));
        assert_eq!(v.cards[0].detail, "Idle · clean, 3 to push");
    }

    #[test]
    fn card_actions_are_their_chords_commands() {
        assert_eq!(
            CardAction::Finish.command(),
            Command::FinishLocalMerge { confirm: false }
        );
        assert_eq!(CardAction::PullBase.command(), Command::PullBase);
        assert_eq!(
            CardAction::Push.command(),
            Command::PushBranch { confirm: None }
        );
    }

    #[test]
    fn last_event_prefers_the_latest_moment() {
        let a = TabActivity {
            last_output_at: Some(NOW - 50),
            last_status_change_at: Some(NOW - 10),
            last_git_change_at: None,
        };
        assert_eq!(
            LastEvent::of(&a, NOW),
            Some(LastEvent {
                kind: LastEventKind::StatusChange,
                secs_ago: 10
            })
        );
        assert_eq!(LastEvent::of(&TabActivity::default(), NOW), None);
        // A stamp from the future is "now".
        let future = TabActivity {
            last_output_at: Some(NOW + 9),
            ..Default::default()
        };
        assert_eq!(LastEvent::of(&future, NOW).map(|e| e.secs_ago), Some(0));
    }

    #[test]
    fn grid_columns_follow_the_count() {
        let cols: Vec<usize> = (0..=9).map(grid_columns).collect();
        assert_eq!(cols, [1, 1, 2, 2, 2, 3, 3, 3, 3, 3]);
        let rows: Vec<usize> = (0..=7).map(grid_visible_rows).collect();
        assert_eq!(rows, [1, 1, 1, 2, 2, 2, 2, 2]);
    }

    #[test]
    fn arrows_move_through_the_grid_and_stop_at_its_edges() {
        use GridMove::*;
        // 5 tiles, 3 columns:  0 1 2
        //                      3 4
        let m = |from, mv| move_selection(from, 5, 3, mv);
        assert_eq!(m(0, Right), 1);
        assert_eq!(m(2, Right), 3, "reading order wraps onto the next row");
        assert_eq!(m(4, Right), 4, "the last tile stays");
        assert_eq!(m(0, Left), 0);
        assert_eq!(m(3, Left), 2);
        assert_eq!(m(1, Down), 4);
        assert_eq!(m(2, Down), 4, "a short row below lands on its last tile");
        assert_eq!(m(4, Down), 4, "the bottom row stays");
        assert_eq!(m(4, Up), 1);
        assert_eq!(m(1, Up), 1, "the top row stays");
        // Out of range (the grid shrank) clamps first.
        assert_eq!(move_selection(9, 5, 3, Left), 3);
        assert_eq!(move_selection(0, 0, 3, Down), 0);
    }
}
