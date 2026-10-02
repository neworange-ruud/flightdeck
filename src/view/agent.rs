//! The Agent Tab list view model (one row per agent).

use std::collections::HashMap;

use crate::app::state::{AppState, RuntimeTab, TabPhase};
use crate::contracts::{InterpretedStatus, ManualStatus};
use crate::git::status::{LineStats, WorktreeChanges, WorktreeStatus};
use crate::terminal::session::TerminalKind;

/// The glanceable state of an agent, independent of how a front-end colours it.
///
/// Derived from the agent's interpreted status only. A manual override is
/// reported separately in [`AgentRowView::manual_status`], because the TUI lets
/// it recolour the row without changing what the agent is actually doing (a
/// working agent stays "working" even if the user marked it "blocked").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentBadge {
    /// Starting, running or mid-turn.
    Working,
    /// Waiting for the user's input, or needs attention.
    WaitingAttention,
    /// Waiting for a prompt, stopped, recovered or not yet known.
    Idle,
    /// The agent process finished cleanly.
    Done,
    /// The agent failed or its session was lost.
    Error,
}

/// Map an interpreted status to its badge.
pub fn agent_badge(status: InterpretedStatus) -> AgentBadge {
    use InterpretedStatus::*;
    match status {
        Starting | Running | Working => AgentBadge::Working,
        WaitingForInput | NeedsAttention => AgentBadge::WaitingAttention,
        Failed | SessionLost => AgentBadge::Error,
        Completed => AgentBadge::Done,
        Idle | Stopped | Recovered | Unknown => AgentBadge::Idle,
    }
}

/// The word the TUI shows in the `[...]` next to the agent name for a status.
/// It collapses more states than [`AgentBadge`]: a cleanly finished agent still
/// reads "idle" there.
pub fn agent_status_text(status: InterpretedStatus) -> &'static str {
    match agent_badge(status) {
        AgentBadge::Working => "in progress",
        AgentBadge::WaitingAttention => "waiting",
        AgentBadge::Error => "error",
        AgentBadge::Idle | AgentBadge::Done => "idle",
    }
}

/// Whether a branch tracks an upstream, and how far it has diverged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamState {
    /// No git status has been collected for this agent yet.
    Unknown,
    /// The branch has no upstream (it was never pushed).
    None,
    /// The branch tracks `upstream`, `ahead` commits ahead and `behind` behind.
    Tracking {
        /// The upstream ref, e.g. `origin/flightdeck/feature`.
        upstream: String,
        /// Local commits not on the upstream.
        ahead: u32,
        /// Upstream commits not in the local branch.
        behind: u32,
    },
}

impl UpstreamState {
    /// The upstream state for an optional collected git status.
    pub fn of(git: Option<&WorktreeStatus>) -> Self {
        match git {
            None => UpstreamState::Unknown,
            Some(ws) => match &ws.upstream {
                None => UpstreamState::None,
                Some(upstream) => UpstreamState::Tracking {
                    upstream: upstream.clone(),
                    ahead: ws.ahead,
                    behind: ws.behind,
                },
            },
        }
    }
}

/// Uncommitted-change counts for a worktree: files by category, plus lines
/// added and removed in tracked files versus `HEAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeSummary {
    /// New files (untracked or staged additions).
    pub added: u32,
    /// Modified, renamed or type-changed files.
    pub modified: u32,
    /// Deleted files.
    pub deleted: u32,
    /// Total changed files; zero means the worktree is clean.
    pub files: u32,
    /// Lines added versus `HEAD` (tracked files, staged and unstaged).
    pub lines_added: u32,
    /// Lines removed versus `HEAD` (tracked files, staged and unstaged).
    pub lines_removed: u32,
}

impl ChangeSummary {
    /// Summarise a set of worktree changes and their line counts.
    pub fn of(changes: WorktreeChanges, lines: LineStats) -> Self {
        ChangeSummary {
            added: changes.added,
            modified: changes.modified,
            deleted: changes.deleted,
            files: changes.total(),
            lines_added: lines.added,
            lines_removed: lines.removed,
        }
    }

    /// Whether the worktree has no uncommitted changes.
    pub fn is_clean(&self) -> bool {
        self.files == 0
    }
}

/// Which terminal of an agent a [`TerminalView`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalRef {
    /// The agent's primary terminal.
    Primary,
    /// The child terminal at this index in the agent's session.
    Child(usize),
}

/// What a terminal hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalRole {
    /// The primary agent or an additional agent process.
    Agent,
    /// A plain shell.
    Shell,
}

/// One terminal nested under an agent: the agent itself plus its extra agents
/// and shells, in tab-bar order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalView {
    /// Which terminal this is.
    pub target: TerminalRef,
    /// Whether it hosts an agent or a shell.
    pub role: TerminalRole,
    /// The tab label: `agent`, `agent 2`, `shell 1`, … Additional agents count
    /// up from 2, shells from 1, each in creation order.
    pub label: String,
    /// The terminal's title (the command it was spawned with). Empty for the
    /// primary of an agent whose session is not spawned yet.
    pub title: String,
}

/// The terminals of `tab` in tab-bar order: the primary first, then every child.
pub fn terminal_views(tab: &RuntimeTab) -> Vec<TerminalView> {
    let mut out = vec![TerminalView {
        target: TerminalRef::Primary,
        role: TerminalRole::Agent,
        label: "agent".to_string(),
        title: tab
            .session
            .primary()
            .map(|t| t.title.clone())
            .unwrap_or_default(),
    }];
    let mut agent_n = 2;
    let mut shell_n = 1;
    for i in 0..tab.session.child_count() {
        let Some(term) = tab.session.child(i) else {
            continue;
        };
        let (role, label) = if term.kind == TerminalKind::Agent {
            let l = format!("agent {agent_n}");
            agent_n += 1;
            (TerminalRole::Agent, l)
        } else {
            let l = format!("shell {shell_n}");
            shell_n += 1;
            (TerminalRole::Shell, l)
        };
        out.push(TerminalView {
            target: TerminalRef::Child(i),
            role,
            label,
            title: term.title.clone(),
        });
    }
    out
}

/// One agent's row in the Agent Tab list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRowView {
    /// Stable tab id.
    pub id: String,
    /// Position in the project's tab list (0-based).
    pub index: usize,
    /// The Alt-N shortcut that selects this row: `Some(1..=9)` for the first
    /// nine agents, `None` beyond that (Alt-0 and Alt-10+ do not exist).
    pub alt_index: Option<u8>,
    /// The tab's display name.
    pub name: String,
    /// The agent's display name (e.g. "Claude Code"), or its registry key when
    /// the registry does not know it.
    pub agent_name: String,
    /// The branch: the freshly collected one when git status is known, else the
    /// one stored with the tab.
    pub branch: String,
    /// Glanceable state (from the interpreted status, ignoring manual override).
    pub badge: AgentBadge,
    /// The user's manual status override, if set. It takes visual priority in
    /// the TUI but never replaces [`AgentRowView::badge`].
    pub manual_status: Option<ManualStatus>,
    /// The status word the TUI shows: the manual override's label when set,
    /// otherwise `in progress` / `waiting` / `error` / `idle`. The elapsed time
    /// is separate, in [`AgentRowView::status_since_secs`].
    pub status_text: String,
    /// Seconds the agent has been in its current status, or `None` when no
    /// status change has been recorded yet. Format with [`format_elapsed`].
    pub status_since_secs: Option<u64>,
    /// Whether the worktree is still being created on a background worker.
    pub creating: bool,
    /// Whether this row is the selected Agent Tab.
    pub selected: bool,
    /// Whether the agent produced output while it was not on screen and the
    /// user has not viewed it since (see [`RuntimeTab::is_unread`]).
    pub unread: bool,
    /// Uncommitted changes, or `None` when no git status is collected yet.
    pub changes: Option<ChangeSummary>,
    /// Upstream tracking state, including ahead/behind when tracking.
    pub upstream: UpstreamState,
    /// Commits the target branch has advanced since this tab last synced.
    pub base_drift: u32,
    /// The tab was recovered from a previous session.
    pub recovered: bool,
    /// The tab attached to a branch that already existed.
    pub attached_existing_branch: bool,
    /// The agent terminal and its nested shells / extra agents.
    pub terminals: Vec<TerminalView>,
}

/// Compact elapsed-time text for a status age: `now` (under 5 seconds), then
/// `42s`, `2m`, `3h`, `2d`, each rounded down to its largest whole unit.
pub fn format_elapsed(secs: u64) -> String {
    match secs {
        0..=4 => "now".to_string(),
        5..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// Build the row for the tab at `index`.
///
/// `git` maps tab ids to their latest collected status. `now_ms` is the clock
/// in milliseconds and `now_secs` in unix seconds; the view reads no clock
/// itself. Returns `None` when `index` is out of range.
pub fn agent_row_view(
    state: &AppState,
    index: usize,
    git: &HashMap<String, WorktreeStatus>,
    now_ms: u64,
    now_secs: u64,
) -> Option<AgentRowView> {
    let tab = state.tabs.get(index)?;
    let ds = tab.display_status(now_ms);
    let ws = git.get(&tab.meta.id);
    let status_text = match ds.manual {
        Some(manual) => manual.as_str().to_string(),
        None => agent_status_text(ds.interpreted).to_string(),
    };
    Some(AgentRowView {
        id: tab.meta.id.clone(),
        index,
        alt_index: (index < 9).then(|| index as u8 + 1),
        name: tab.meta.name.clone(),
        agent_name: state
            .registry
            .get(&tab.meta.agent)
            .map(|a| a.display_name.clone())
            .unwrap_or_else(|| tab.meta.agent.clone()),
        branch: ws
            .map(|w| w.branch.clone())
            .unwrap_or_else(|| tab.meta.branch.clone()),
        badge: agent_badge(ds.interpreted),
        manual_status: ds.manual,
        status_text,
        status_since_secs: tab
            .meta
            .activity
            .last_status_change_at
            .map(|at| now_secs.saturating_sub(at)),
        creating: tab.phase == TabPhase::Creating,
        selected: state.selected_tab == Some(index),
        unread: tab.is_unread(),
        changes: ws.map(|w| ChangeSummary::of(w.changes, w.lines)),
        upstream: UpstreamState::of(ws),
        base_drift: ws.map(|w| w.base_drift).unwrap_or(0),
        recovered: tab.meta.recovered,
        attached_existing_branch: tab.meta.attached_existing_branch,
        terminals: terminal_views(tab),
    })
}

/// Build every row of the Agent Tab list, in tab order.
pub fn agent_row_views(
    state: &AppState,
    git: &HashMap<String, WorktreeStatus>,
    now_ms: u64,
    now_secs: u64,
) -> Vec<AgentRowView> {
    (0..state.tabs.len())
        .filter_map(|i| agent_row_view(state, i, git, now_ms, now_secs))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::contracts::Config;
    use crate::contracts::PtySize;
    use crate::persistence::project_state::default_state;
    use crate::testing::FakePty;
    use std::path::{Path, PathBuf};

    fn state_with_tabs(n: usize) -> AppState {
        let mut ps = default_state("main");
        for i in 0..n {
            ps.tabs.push(crate::contracts::TabState {
                id: format!("t{i}"),
                name: format!("tab{i}"),
                slug: format!("tab{i}"),
                agent: "opencode".to_string(),
                branch: format!("flightdeck/tab{i}"),
                worktree_path_relative: format!(".flightdeck/worktrees/tab{i}"),
                base_branch: "main".to_string(),
                base_commit_sha: "sha".to_string(),
                created_at: "t".to_string(),
                attached_existing_branch: false,
                recovered: false,
                last_known_status: "unknown".to_string(),
                manual_status: None,
                containerized: false,
                container_image: None,
                runs_on_base: false,
                resume_args: Vec::new(),
                activity: Default::default(),
            });
        }
        AppState::new(Config::default(), ps, "/repo", "/repo/state.json")
    }

    fn ws(upstream: Option<&str>, ahead: u32, behind: u32) -> WorktreeStatus {
        WorktreeStatus {
            branch: "flightdeck/live".to_string(),
            base_branch: "main".to_string(),
            dirty: false,
            changes: WorktreeChanges::default(),
            lines: LineStats::default(),
            ahead,
            behind,
            upstream: upstream.map(str::to_string),
            base_drift: 0,
            worktree_path: PathBuf::from("/wt"),
        }
    }

    fn rows(state: &AppState) -> Vec<AgentRowView> {
        agent_row_views(state, &HashMap::new(), 0, 0)
    }

    #[test]
    fn badges_cover_every_interpreted_status() {
        use InterpretedStatus::*;
        let cases = [
            (Starting, AgentBadge::Working),
            (Running, AgentBadge::Working),
            (Working, AgentBadge::Working),
            (WaitingForInput, AgentBadge::WaitingAttention),
            (NeedsAttention, AgentBadge::WaitingAttention),
            (Idle, AgentBadge::Idle),
            (Stopped, AgentBadge::Idle),
            (Recovered, AgentBadge::Idle),
            (Unknown, AgentBadge::Idle),
            (Completed, AgentBadge::Done),
            (Failed, AgentBadge::Error),
            (SessionLost, AgentBadge::Error),
        ];
        for (status, badge) in cases {
            assert_eq!(agent_badge(status), badge, "{status:?}");
        }
    }

    #[test]
    fn status_text_matches_what_the_tui_shows() {
        use InterpretedStatus::*;
        assert_eq!(agent_status_text(Working), "in progress");
        assert_eq!(agent_status_text(WaitingForInput), "waiting");
        assert_eq!(agent_status_text(Failed), "error");
        assert_eq!(agent_status_text(Idle), "idle");
        // A cleanly finished agent is a "done" badge but still reads "idle".
        assert_eq!(agent_status_text(Completed), "idle");
    }

    #[test]
    fn alt_index_covers_only_the_first_nine_agents() {
        let state = state_with_tabs(11);
        let alts: Vec<Option<u8>> = rows(&state).iter().map(|r| r.alt_index).collect();
        let expected: Vec<Option<u8>> = (1..=9u8).map(Some).chain([None, None]).collect();
        assert_eq!(alts, expected);
    }

    #[test]
    fn selected_flag_follows_the_selected_tab() {
        let mut state = state_with_tabs(3);
        state.selected_tab = Some(1);
        let selected: Vec<bool> = rows(&state).iter().map(|r| r.selected).collect();
        assert_eq!(selected, vec![false, true, false]);
        state.selected_tab = None;
        assert!(rows(&state).iter().all(|r| !r.selected));
    }

    #[test]
    fn unread_flag_comes_from_app_state() {
        let mut state = state_with_tabs(2);
        state.tabs[1].note_output();
        state.sync_activity(10, 10_000, true);
        let rows = rows(&state);
        assert!(!rows[0].unread);
        assert!(rows[1].unread);
    }

    #[test]
    fn status_since_is_measured_from_the_last_status_change() {
        let mut state = state_with_tabs(2);
        state.tabs[0].meta.activity.last_status_change_at = Some(1_000);
        let rows = agent_row_views(&state, &HashMap::new(), 0, 1_090);
        assert_eq!(rows[0].status_since_secs, Some(90));
        assert_eq!(rows[1].status_since_secs, None);
        // A clock that reads earlier than the stamp never underflows.
        let rows = agent_row_views(&state, &HashMap::new(), 0, 500);
        assert_eq!(rows[0].status_since_secs, Some(0));
    }

    #[test]
    fn format_elapsed_covers_every_unit_boundary() {
        let cases = [
            (0, "now"),
            (4, "now"),
            (5, "5s"),
            (42, "42s"),
            (59, "59s"),
            (60, "1m"),
            (119, "1m"),
            (3_599, "59m"),
            (3_600, "1h"),
            (10_800, "3h"),
            (86_399, "23h"),
            (86_400, "1d"),
            (172_800, "2d"),
            (u64::MAX, "213503982334601d"),
        ];
        for (secs, text) in cases {
            assert_eq!(format_elapsed(secs), text, "{secs}s");
        }
    }

    #[test]
    fn change_summary_carries_line_counts() {
        let summary = ChangeSummary::of(
            WorktreeChanges {
                added: 1,
                modified: 1,
                deleted: 0,
            },
            LineStats {
                added: 12,
                removed: 3,
            },
        );
        assert_eq!((summary.lines_added, summary.lines_removed), (12, 3));
        assert_eq!(summary.files, 2);
    }

    #[test]
    fn upstream_variants() {
        assert_eq!(UpstreamState::of(None), UpstreamState::Unknown);
        assert_eq!(
            UpstreamState::of(Some(&ws(None, 0, 0))),
            UpstreamState::None
        );
        assert_eq!(
            UpstreamState::of(Some(&ws(Some("origin/x"), 2, 1))),
            UpstreamState::Tracking {
                upstream: "origin/x".to_string(),
                ahead: 2,
                behind: 1
            }
        );
        // Ahead/behind are meaningless without an upstream and are dropped.
        assert_eq!(
            UpstreamState::of(Some(&ws(None, 5, 5))),
            UpstreamState::None
        );
    }

    #[test]
    fn git_status_feeds_branch_changes_and_upstream() {
        let state = state_with_tabs(1);
        let mut status = ws(Some("origin/flightdeck/live"), 1, 0);
        status.changes = WorktreeChanges {
            added: 1,
            modified: 2,
            deleted: 3,
        };
        status.base_drift = 4;
        status.lines = LineStats {
            added: 10,
            removed: 4,
        };
        let git = HashMap::from([("t0".to_string(), status)]);
        let row = agent_row_view(&state, 0, &git, 0, 0).unwrap();
        assert_eq!(row.branch, "flightdeck/live");
        assert_eq!(
            row.changes,
            Some(ChangeSummary {
                added: 1,
                modified: 2,
                deleted: 3,
                files: 6,
                lines_added: 10,
                lines_removed: 4,
            })
        );
        assert!(!row.changes.unwrap().is_clean());
        assert_eq!(row.base_drift, 4);
        assert!(matches!(row.upstream, UpstreamState::Tracking { .. }));
    }

    #[test]
    fn without_git_status_the_stored_branch_is_used() {
        let state = state_with_tabs(1);
        let row = agent_row_view(&state, 0, &HashMap::new(), 0, 0).unwrap();
        assert_eq!(row.branch, "flightdeck/tab0");
        assert_eq!(row.changes, None);
        assert_eq!(row.upstream, UpstreamState::Unknown);
        assert_eq!(row.base_drift, 0);
    }

    #[test]
    fn out_of_range_index_yields_none() {
        let state = state_with_tabs(1);
        assert!(agent_row_view(&state, 1, &HashMap::new(), 0, 0).is_none());
    }

    #[test]
    fn manual_override_replaces_the_text_but_not_the_badge() {
        let pty = FakePty::new();
        pty.queue_session();
        let mut state = state_with_tabs(1);
        state.tabs[0]
            .session
            .spawn_primary(&pty, "agent", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        state.tabs[0].interpreted = Some(InterpretedStatus::Working);
        assert_eq!(rows(&state)[0].status_text, "in progress");

        state.tabs[0].meta.manual_status = Some("blocked".to_string());
        let row = &rows(&state)[0];
        assert_eq!(row.badge, AgentBadge::Working);
        assert_eq!(row.manual_status, Some(ManualStatus::Blocked));
        assert_eq!(row.status_text, "blocked");
    }

    #[test]
    fn running_agent_with_a_lifecycle_signal_maps_to_its_badge() {
        let pty = FakePty::new();
        pty.queue_session();
        let mut state = state_with_tabs(1);
        state.tabs[0]
            .session
            .spawn_primary(&pty, "agent", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        state.tabs[0].interpreted = Some(InterpretedStatus::WaitingForInput);
        let row = &rows(&state)[0];
        assert_eq!(row.badge, AgentBadge::WaitingAttention);
        assert_eq!(row.status_text, "waiting");
    }

    #[test]
    fn creating_tab_is_flagged() {
        let mut state = state_with_tabs(1);
        state.tabs[0].phase = TabPhase::Creating;
        assert!(rows(&state)[0].creating);
    }

    #[test]
    fn terminals_list_agent_then_shells_and_extra_agents_with_titles() {
        let pty = FakePty::new();
        let mut state = state_with_tabs(1);
        let session = &mut state.tabs[0].session;
        pty.queue_session();
        session
            .spawn_primary(&pty, "claude", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        pty.queue_session();
        session
            .spawn_child(&pty, "zsh", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        pty.queue_session();
        session
            .spawn_agent_child(&pty, "codex", &[], Path::new("/wt"), PtySize::default())
            .unwrap();
        pty.queue_session();
        session
            .spawn_child(&pty, "bash", &[], Path::new("/wt"), PtySize::default())
            .unwrap();

        let summary: Vec<(TerminalRef, TerminalRole, String, String)> = rows(&state)[0]
            .terminals
            .iter()
            .map(|t| (t.target, t.role, t.label.clone(), t.title.clone()))
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    TerminalRef::Primary,
                    TerminalRole::Agent,
                    "agent".to_string(),
                    "claude".to_string()
                ),
                (
                    TerminalRef::Child(0),
                    TerminalRole::Shell,
                    "shell 1".to_string(),
                    "zsh".to_string()
                ),
                (
                    TerminalRef::Child(1),
                    TerminalRole::Agent,
                    "agent 2".to_string(),
                    "codex".to_string()
                ),
                (
                    TerminalRef::Child(2),
                    TerminalRole::Shell,
                    "shell 2".to_string(),
                    "bash".to_string()
                ),
            ]
        );
    }

    #[test]
    fn unspawned_agent_still_lists_its_primary_terminal() {
        let state = state_with_tabs(1);
        let terms = &rows(&state)[0].terminals;
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].label, "agent");
        assert_eq!(terms[0].title, "");
    }
}
