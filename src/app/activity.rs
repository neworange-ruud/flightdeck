//! Per-session activity timeline and the pure rules a "Mission control" view
//! is built from. Front-end neutral: no terminal, git, or clock access here —
//! callers pass wall-clock Unix seconds obtained through the
//! [`crate::contracts::Clock`] seam ([`Clock::now_unix_secs`]).
//!
//! Three moments are tracked per session ([`TabActivity`]): last PTY output,
//! last status change, last git change. They are persisted in `state.json`
//! (inside `TabState`) so "updated in the last 24h" survives a restart.
//!
//! The view derives from three pure functions:
//! - [`is_active`]: live tiles (working, waiting for input, needs attention);
//! - [`is_recent`]: "earlier today" cards and the 7-day / all filters;
//! - [`compare_sessions`] / [`sort_sessions`]: needs-you first, then working,
//!   then most recently updated.
//!
//! [`Clock::now_unix_secs`]: crate::contracts::Clock::now_unix_secs

use std::cmp::Ordering;

use crate::contracts::{InterpretedStatus, TabActivity};
use crate::git::status::{WorktreeChanges, WorktreeStatus};

/// Default "recent" window: 24 hours.
pub const RECENT_WINDOW_24H: u64 = 24 * 60 * 60;
/// The "last 7 days" window.
pub const RECENT_WINDOW_7D: u64 = 7 * RECENT_WINDOW_24H;
/// The "all" window: everything that has ever shown activity.
pub const RECENT_WINDOW_ALL: u64 = u64::MAX;

impl TabActivity {
    /// Record PTY output at `now_secs`. Just stores the timestamp — no
    /// allocation — because the caller does this on every PTY read.
    pub fn note_output(&mut self, now_secs: u64) {
        self.last_output_at = Some(now_secs);
    }

    /// Record that the interpreted status changed at `now_secs`.
    pub fn note_status_change(&mut self, now_secs: u64) {
        self.last_status_change_at = Some(now_secs);
    }

    /// Record that the worktree's git state changed at `now_secs`.
    pub fn note_git_change(&mut self, now_secs: u64) {
        self.last_git_change_at = Some(now_secs);
    }

    /// The most recent of the three moments, or `None` if nothing was ever seen.
    pub fn last_activity(&self) -> Option<u64> {
        [
            self.last_output_at,
            self.last_status_change_at,
            self.last_git_change_at,
        ]
        .into_iter()
        .flatten()
        .max()
    }
}

/// Whether a session is live right now and belongs in the live tiles: the agent
/// is working, waiting for the user, or needs attention.
pub fn is_active(status: InterpretedStatus) -> bool {
    matches!(
        status,
        InterpretedStatus::Working
            | InterpretedStatus::WaitingForInput
            | InterpretedStatus::NeedsAttention
    )
}

/// Whether the session needs the user (ranks above merely working ones).
pub fn needs_you(status: InterpretedStatus) -> bool {
    matches!(
        status,
        InterpretedStatus::WaitingForInput | InterpretedStatus::NeedsAttention
    )
}

/// Whether any activity falls inside `window_secs` before `now_secs`. The
/// boundary is inclusive: activity exactly `window_secs` ago is still recent.
/// A timestamp in the future (clock skew, restored state) counts as recent. A
/// session with no recorded activity is never recent. Use
/// [`RECENT_WINDOW_ALL`] for "all time".
pub fn is_recent(activity: &TabActivity, now_secs: u64, window_secs: u64) -> bool {
    activity
        .last_activity()
        .is_some_and(|last| now_secs.saturating_sub(last) <= window_secs)
}

/// The facts [`compare_sessions`] orders by, borrowed from whatever a front end
/// holds (a runtime tab, a snapshot row, ...).
#[derive(Debug, Clone, Copy)]
pub struct SessionActivity<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub status: InterpretedStatus,
    pub activity: TabActivity,
}

/// Rank: 0 = needs you, 1 = working, 2 = everything else.
fn rank(status: InterpretedStatus) -> u8 {
    if needs_you(status) {
        0
    } else if status == InterpretedStatus::Working {
        1
    } else {
        2
    }
}

/// Mission-control order: needs you first, then working, then most recently
/// updated. Ties break stably by name, then id, so equal-recency sessions never
/// shuffle between renders.
pub fn compare_sessions(a: &SessionActivity<'_>, b: &SessionActivity<'_>) -> Ordering {
    rank(a.status)
        .cmp(&rank(b.status))
        // Newest first; a session with no activity sorts after any with some
        // (`None < Some`, so reversing puts `Some(newest)` first).
        .then_with(|| b.activity.last_activity().cmp(&a.activity.last_activity()))
        .then_with(|| a.name.cmp(b.name))
        .then_with(|| a.id.cmp(b.id))
}

/// Sort `sessions` in place by [`compare_sessions`].
pub fn sort_sessions(sessions: &mut [SessionActivity<'_>]) {
    sessions.sort_by(compare_sessions);
}

/// The parts of a [`WorktreeStatus`] whose change counts as "git activity":
/// the per-category change counts and the ahead count (a new commit). Behind /
/// drift are excluded — they move because of *other* branches, not this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitFingerprint {
    changes: WorktreeChanges,
    ahead: u32,
}

impl GitFingerprint {
    /// Fingerprint a freshly collected status.
    pub fn of(status: &WorktreeStatus) -> Self {
        Self {
            changes: status.changes,
            ahead: status.ahead,
        }
    }
}

/// Runtime-only edge memory for one session: what has been seen since the last
/// sync, so [`TabActivity`] only gains a timestamp on a real edge. Not persisted.
#[derive(Debug, Clone, Default)]
pub struct ActivityProbe {
    output_pending: bool,
    status_seen: Option<InterpretedStatus>,
    git_seen: Option<GitFingerprint>,
}

impl ActivityProbe {
    /// Flag PTY output. A single bool store: safe on every PTY read.
    pub fn note_output(&mut self) {
        self.output_pending = true;
    }

    /// Consume the pending flags against the current `status`; returns
    /// `(output_since_last_call, status_changed)`. The first status seen is a
    /// baseline, not a change.
    pub fn take_edges(&mut self, status: InterpretedStatus) -> (bool, bool) {
        let output = std::mem::take(&mut self.output_pending);
        let changed = self.status_seen.is_some_and(|prev| prev != status);
        self.status_seen = Some(status);
        (output, changed)
    }

    /// Compare a git fingerprint with the previous one; true on a change. The
    /// first observation is a baseline, not a change.
    pub fn observe_git(&mut self, fingerprint: GitFingerprint) -> bool {
        let changed = self.git_seen.is_some_and(|prev| prev != fingerprint);
        self.git_seen = Some(fingerprint);
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{Clock, TabState};
    use crate::testing::FakeClock;

    const NOW: u64 = 2_000_000_000;

    fn clock_at(secs: u64) -> FakeClock {
        let c = FakeClock::default();
        c.set_unix_secs(secs);
        c
    }

    fn at(secs: Option<u64>) -> TabActivity {
        TabActivity {
            last_output_at: secs,
            ..Default::default()
        }
    }

    #[test]
    fn active_statuses() {
        use InterpretedStatus::*;
        for s in [Working, WaitingForInput, NeedsAttention] {
            assert!(is_active(s), "{s:?}");
        }
        for s in [
            Starting,
            Running,
            Idle,
            Completed,
            Failed,
            Stopped,
            SessionLost,
            Recovered,
            Unknown,
        ] {
            assert!(!is_active(s), "{s:?}");
        }
    }

    #[test]
    fn window_edges_against_fake_clock() {
        let clock = clock_at(NOW);
        let now = clock.now_unix_secs();
        let w = RECENT_WINDOW_24H;
        // Exactly on the boundary: inclusive.
        assert!(is_recent(&at(Some(now - w)), now, w));
        // One second inside / outside.
        assert!(is_recent(&at(Some(now - w + 1)), now, w));
        assert!(!is_recent(&at(Some(now - w - 1)), now, w));
        // Advancing the clock one second pushes the boundary case out.
        clock.set_unix_secs(NOW + 1);
        assert!(!is_recent(&at(Some(now - w)), clock.now_unix_secs(), w));
    }

    #[test]
    fn recent_uses_latest_of_all_three_moments() {
        let a = TabActivity {
            last_output_at: Some(NOW - 10 * RECENT_WINDOW_24H),
            last_status_change_at: None,
            last_git_change_at: Some(NOW - 60),
        };
        assert!(is_recent(&a, NOW, RECENT_WINDOW_24H));
        assert_eq!(a.last_activity(), Some(NOW - 60));
    }

    #[test]
    fn never_active_is_never_recent_even_for_all() {
        assert!(!is_recent(&TabActivity::default(), NOW, RECENT_WINDOW_ALL));
        assert!(is_recent(&at(Some(0)), NOW, RECENT_WINDOW_ALL));
    }

    #[test]
    fn seven_day_window_and_future_timestamps() {
        assert!(is_recent(
            &at(Some(NOW - 6 * 86_400)),
            NOW,
            RECENT_WINDOW_7D
        ));
        assert!(!is_recent(
            &at(Some(NOW - 8 * 86_400)),
            NOW,
            RECENT_WINDOW_7D
        ));
        // Future timestamp (clock skew) does not underflow and counts as recent.
        assert!(is_recent(&at(Some(NOW + 500)), NOW, RECENT_WINDOW_24H));
    }

    #[test]
    fn note_methods_record_independently() {
        let clock = clock_at(NOW);
        let mut a = TabActivity::default();
        a.note_output(clock.now_unix_secs());
        clock.set_unix_secs(NOW + 5);
        a.note_status_change(clock.now_unix_secs());
        clock.set_unix_secs(NOW + 9);
        a.note_git_change(clock.now_unix_secs());
        assert_eq!(a.last_output_at, Some(NOW));
        assert_eq!(a.last_status_change_at, Some(NOW + 5));
        assert_eq!(a.last_git_change_at, Some(NOW + 9));
    }

    fn sess<'a>(
        id: &'a str,
        name: &'a str,
        status: InterpretedStatus,
        t: Option<u64>,
    ) -> SessionActivity<'a> {
        SessionActivity {
            id,
            name,
            status,
            activity: at(t),
        }
    }

    fn order(mut v: Vec<SessionActivity<'_>>) -> Vec<&str> {
        sort_sessions(&mut v);
        v.iter().map(|s| s.id).collect()
    }

    #[test]
    fn ordering_needs_you_then_working_then_recency() {
        use InterpretedStatus::*;
        // Older needs-you still beats a fresher working session, which beats
        // a fresher idle one; among needs-you, the fresher comes first.
        let got = order(vec![
            sess("idle-new", "a", Idle, Some(900)),
            sess("work-new", "b", Working, Some(800)),
            sess("need-old", "c", NeedsAttention, Some(1)),
            sess("wait-mid", "d", WaitingForInput, Some(50)),
            sess("idle-old", "e", Completed, Some(10)),
            sess("idle-none", "f", Idle, None),
        ]);
        assert_eq!(
            got,
            [
                "wait-mid",
                "need-old",
                "work-new",
                "idle-new",
                "idle-old",
                "idle-none"
            ]
        );
    }

    #[test]
    fn ordering_within_working_is_by_recency() {
        use InterpretedStatus::*;
        let got = order(vec![
            sess("w1", "a", Working, Some(10)),
            sess("w2", "b", Working, Some(20)),
        ]);
        assert_eq!(got, ["w2", "w1"]);
    }

    #[test]
    fn ordering_ties_break_by_name_then_id() {
        use InterpretedStatus::*;
        let got = order(vec![
            sess("3", "beta", Idle, Some(5)),
            sess("2", "alpha", Idle, Some(5)),
            sess("1", "alpha", Idle, Some(5)),
        ]);
        assert_eq!(got, ["1", "2", "3"]);
        // Input order does not matter.
        let got = order(vec![
            sess("1", "alpha", Idle, Some(5)),
            sess("3", "beta", Idle, Some(5)),
            sess("2", "alpha", Idle, Some(5)),
        ]);
        assert_eq!(got, ["1", "2", "3"]);
    }

    #[test]
    fn probe_stamps_only_on_edges() {
        use InterpretedStatus::*;
        let clock = clock_at(NOW);
        let mut probe = ActivityProbe::default();
        let mut act = TabActivity::default();
        let sync = |probe: &mut ActivityProbe, act: &mut TabActivity, st| {
            let (out, changed) = probe.take_edges(st);
            let now = clock.now_unix_secs();
            if out {
                act.note_output(now);
            }
            if changed {
                act.note_status_change(now);
            }
        };
        // First sighting: baseline only.
        sync(&mut probe, &mut act, Idle);
        assert_eq!(act, TabActivity::default());
        // Output stamps output but not status.
        clock.set_unix_secs(NOW + 10);
        probe.note_output();
        sync(&mut probe, &mut act, Idle);
        assert_eq!(act.last_output_at, Some(NOW + 10));
        assert_eq!(act.last_status_change_at, None);
        // No new output: nothing moves.
        clock.set_unix_secs(NOW + 20);
        sync(&mut probe, &mut act, Idle);
        assert_eq!(act.last_output_at, Some(NOW + 10));
        // Status edge.
        clock.set_unix_secs(NOW + 30);
        sync(&mut probe, &mut act, Working);
        assert_eq!(act.last_status_change_at, Some(NOW + 30));
    }

    #[test]
    fn probe_git_edges() {
        let mut probe = ActivityProbe::default();
        let a = GitFingerprint {
            changes: WorktreeChanges {
                added: 1,
                ..Default::default()
            },
            ahead: 0,
        };
        let b = GitFingerprint { ahead: 1, ..a };
        assert!(!probe.observe_git(a), "baseline");
        assert!(!probe.observe_git(a), "unchanged");
        assert!(probe.observe_git(b), "new commit");
    }

    fn tab_json(extra: &str) -> String {
        format!(
            r#"{{"id":"t1","name":"n","slug":"n","agent":"claude","branch":"b",
            "worktree_path_relative":"w","base_branch":"main","base_commit_sha":"abc",
            "created_at":"2026-01-01T00:00:00Z"{extra}}}"#
        )
    }

    #[test]
    fn activity_survives_a_restart() {
        let mut tab: TabState = serde_json::from_str(&tab_json("")).unwrap();
        tab.activity.note_output(NOW);
        tab.activity.note_git_change(NOW + 3);
        let json = serde_json::to_string(&tab).unwrap();
        let back: TabState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tab);
        assert_eq!(back.activity.last_output_at, Some(NOW));
        assert_eq!(back.activity.last_status_change_at, None);
        assert_eq!(back.activity.last_git_change_at, Some(NOW + 3));
        // Still "recent" after the reload, an hour later.
        assert!(is_recent(&back.activity, NOW + 3_600, RECENT_WINDOW_24H));
    }

    #[test]
    fn old_state_without_activity_still_loads() {
        let tab: TabState = serde_json::from_str(&tab_json("")).unwrap();
        assert_eq!(tab.activity, TabActivity::default());
        // A partially-populated activity object also loads.
        let tab: TabState =
            serde_json::from_str(&tab_json(r#","activity":{"last_output_at":7}"#)).unwrap();
        assert_eq!(tab.activity.last_output_at, Some(7));
        assert_eq!(tab.activity.last_git_change_at, None);
    }
}
