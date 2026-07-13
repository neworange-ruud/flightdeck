use super::*;
use crate::contracts::{InterpretedStatus as IS, ManualStatus, ProcessState};

fn ds(interpreted: IS, manual: Option<ManualStatus>) -> DisplayStatus {
    DisplayStatus {
        process: ProcessState::Running,
        interpreted,
        manual,
    }
}

// --- agent_type_of ---------------------------------------------------------

#[test]
fn agent_type_maps_known_backends() {
    assert_eq!(agent_type_of("claude"), AgentType::ClaudeCode);
    assert_eq!(agent_type_of("Claude Code"), AgentType::ClaudeCode);
    assert_eq!(agent_type_of("codex"), AgentType::Codex);
    assert_eq!(agent_type_of("my-codex-cli"), AgentType::Codex);
    assert_eq!(agent_type_of("opencode"), AgentType::Opencode);
    // Unknown keys default to Claude Code.
    assert_eq!(agent_type_of("something-else"), AgentType::ClaudeCode);
}

// --- agent_status: every interpreted status --------------------------------

#[test]
fn agent_status_maps_working_family() {
    for s in [IS::Starting, IS::Running, IS::Working] {
        assert_eq!(agent_status(ds(s, None)), AgentStatus::Working, "{s:?}");
    }
}

#[test]
fn agent_status_maps_needs_input_family() {
    for s in [IS::WaitingForInput, IS::NeedsAttention] {
        assert_eq!(agent_status(ds(s, None)), AgentStatus::NeedsInput, "{s:?}");
    }
}

#[test]
fn agent_status_maps_idle_family() {
    for s in [
        IS::Idle,
        IS::Completed,
        IS::Failed,
        IS::Stopped,
        IS::SessionLost,
        IS::Recovered,
        IS::Unknown,
    ] {
        assert_eq!(agent_status(ds(s, None)), AgentStatus::Idle, "{s:?}");
    }
}

#[test]
fn manual_override_wins_over_every_interpreted() {
    for s in [IS::Working, IS::WaitingForInput, IS::Idle, IS::Failed] {
        let got = agent_status(ds(s, Some(ManualStatus::Blocked)));
        assert_eq!(
            got,
            AgentStatus::Manual {
                label: "blocked".to_string()
            },
            "{s:?}"
        );
    }
}

#[test]
fn manual_labels_match_tui_wording() {
    let cases = [
        (ManualStatus::InProgress, "in progress"),
        (ManualStatus::Waiting, "waiting"),
        (ManualStatus::Blocked, "blocked"),
        (ManualStatus::Done, "done"),
    ];
    for (m, label) in cases {
        assert_eq!(
            agent_status(ds(IS::Idle, Some(m))),
            AgentStatus::Manual {
                label: label.to_string()
            }
        );
    }
}

// --- git indicators --------------------------------------------------------

#[test]
fn git_indicators_from_cache() {
    use crate::git::status::{WorktreeChanges, WorktreeStatus};
    let ws = WorktreeStatus {
        branch: "fix-login".to_string(),
        base_branch: "main".to_string(),
        dirty: true,
        changes: WorktreeChanges {
            added: 2,
            modified: 3,
            deleted: 1,
        },
        ahead: 4,
        behind: 5,
        upstream: Some("origin/fix-login".to_string()),
        base_drift: 6,
        worktree_path: std::path::PathBuf::from("/tmp/wt"),
    };
    let g = git_indicators(Some(&ws), "ignored");
    assert_eq!(g.branch.as_deref(), Some("fix-login"));
    assert_eq!((g.added, g.modified, g.removed), (2, 3, 1));
    assert_eq!((g.ahead, g.behind, g.drift), (4, 5, 6));
    assert!(g.has_upstream);
    assert!(!g.is_clean());
}

#[test]
fn git_indicators_fallback_without_cache() {
    let g = git_indicators(None, "my-branch");
    assert_eq!(g.branch.as_deref(), Some("my-branch"));
    assert_eq!((g.added, g.modified, g.removed), (0, 0, 0));
    assert!(!g.has_upstream);
    assert!(g.is_clean());
    // Empty fallback branch yields no branch.
    assert_eq!(git_indicators(None, "").branch, None);
}

// --- rollup precedence + summary -------------------------------------------

fn sess(status: AgentStatus) -> SessionState {
    SessionState {
        session_id: SessionId::new("s"),
        project_id: ProjectId::new("p"),
        name: "n".to_string(),
        agent_type: AgentType::ClaudeCode,
        status,
        git: git_indicators(None, "b"),
        running_time_secs: 0,
        pending_question: None,
    }
}

#[test]
fn rollup_precedence_needs_input_over_working() {
    let r = rollup(&[
        sess(AgentStatus::Working),
        sess(AgentStatus::NeedsInput),
        sess(AgentStatus::Idle),
    ]);
    assert_eq!(r.dot, RollupDot::NeedsInput);
    assert_eq!((r.working, r.idle, r.needs_input, r.manual), (1, 1, 1, 0));
    assert_eq!(r.agent_count, 3);
    assert_eq!(r.summary, "1 needs input · 1 working · 1 idle · 3 agents");
}

#[test]
fn rollup_working_over_manual() {
    let r = rollup(&[
        sess(AgentStatus::Working),
        sess(AgentStatus::Manual {
            label: "blocked".to_string(),
        }),
    ]);
    assert_eq!(r.dot, RollupDot::Working);
}

#[test]
fn rollup_manual_over_idle() {
    let r = rollup(&[
        sess(AgentStatus::Idle),
        sess(AgentStatus::Manual {
            label: "x".to_string(),
        }),
    ]);
    assert_eq!(r.dot, RollupDot::Manual);
}

#[test]
fn rollup_all_idle_is_idle_dot() {
    let r = rollup(&[sess(AgentStatus::Idle), sess(AgentStatus::Idle)]);
    assert_eq!(r.dot, RollupDot::Idle);
    assert_eq!(r.summary, "2 idle · 2 agents");
}

#[test]
fn rollup_empty_project() {
    let r = rollup(&[]);
    assert_eq!(r.dot, RollupDot::Idle);
    assert_eq!(r.agent_count, 0);
    assert_eq!(r.summary, "no agents");
}

#[test]
fn rollup_singular_agent_unit() {
    let r = rollup(&[sess(AgentStatus::Working)]);
    assert_eq!(r.summary, "1 working · 1 agent");
}

// --- turn timer ------------------------------------------------------------

#[test]
fn turn_timer_counts_while_working_then_freezes() {
    let mut t = TurnTimer::default();
    assert_eq!(t.observe(&AgentStatus::Working, 1_000), 0);
    assert_eq!(t.observe(&AgentStatus::Working, 6_000), 5); // 5s elapsed
                                                            // Leaving working freezes the elapsed time.
    assert_eq!(t.observe(&AgentStatus::Idle, 20_000), 5);
    assert_eq!(t.observe(&AgentStatus::Idle, 99_000), 5);
    // Working again starts a fresh span.
    assert_eq!(t.observe(&AgentStatus::Working, 100_000), 0);
    assert_eq!(t.observe(&AgentStatus::Working, 103_000), 3);
}

// --- delta vs snapshot -----------------------------------------------------

fn snap(sessions: Vec<SessionState>) -> StateSnapshot {
    let project = ProjectState {
        project_id: ProjectId::new("p"),
        name: "p".to_string(),
        rollup: rollup(&sessions),
        sessions,
    };
    StateSnapshot {
        server_time_ms: 0,
        projects: vec![project],
    }
}

fn named_sess(id: &str, status: AgentStatus) -> SessionState {
    SessionState {
        session_id: SessionId::new(id),
        project_id: ProjectId::new("p"),
        name: id.to_string(),
        agent_type: AgentType::ClaudeCode,
        status,
        git: git_indicators(None, "b"),
        running_time_secs: 0,
        pending_question: None,
    }
}

#[test]
fn first_diff_reports_no_status_deltas_but_flags_new_sessions() {
    let mut fs = FeedState::default();
    let s = snap(vec![named_sess("a", AgentStatus::Idle)]);
    let d = fs.diff(&s);
    // A brand-new session is a structural change (bridge upgrades to snapshot).
    assert!(d.set_changed);
    assert!(d.status.is_empty());
}

#[test]
fn status_change_produces_delta_only() {
    let mut fs = FeedState::default();
    fs.record_snapshot(&snap(vec![named_sess("a", AgentStatus::Idle)]));
    // Same set, one status changed.
    let d = fs.diff(&snap(vec![named_sess("a", AgentStatus::Working)]));
    assert!(!d.set_changed);
    assert_eq!(d.status.len(), 1);
    assert_eq!(d.status[0].status, AgentStatus::Working);
    // Roll-up also changed (idle -> working dot).
    assert_eq!(d.rollups.len(), 1);
}

#[test]
fn no_change_produces_nothing() {
    let mut fs = FeedState::default();
    fs.record_snapshot(&snap(vec![named_sess("a", AgentStatus::Idle)]));
    let d = fs.diff(&snap(vec![named_sess("a", AgentStatus::Idle)]));
    assert!(!d.set_changed);
    assert!(d.status.is_empty());
    assert!(d.rollups.is_empty());
}

#[test]
fn removed_session_flags_set_changed() {
    let mut fs = FeedState::default();
    fs.record_snapshot(&snap(vec![
        named_sess("a", AgentStatus::Idle),
        named_sess("b", AgentStatus::Idle),
    ]));
    let d = fs.diff(&snap(vec![named_sess("a", AgentStatus::Idle)]));
    assert!(d.set_changed);
}
