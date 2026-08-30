//! Tests for [`super::ActivityStore`]: retention on both bounds, global
//! ordering, unread-tier precedence, read-marking, and the "unknown stays
//! unknown" honesty requirement (§5.1). Failure paths get equal billing with
//! success paths here (SPECS §26): eviction edges, an aging store nobody
//! writes to, and a clock that steps backward are all exercised explicitly.

use super::*;
use crate::testing::FakeClock;

fn transition(
    project: &str,
    session: &str,
    from: InterpretedStatus,
    to: InterpretedStatus,
    reason: &str,
) -> Transition {
    Transition {
        project_id: ProjectId::new(project),
        project_name: project.to_string(),
        session_id: TabId(session.to_string()),
        session_name: session.to_string(),
        from,
        to,
        manual: None,
        reason: reason.to_string(),
    }
}

// ===========================================================================
// Empty store
// ===========================================================================

#[test]
fn empty_store_has_no_events_and_no_unread() {
    let store = ActivityStore::new();
    assert_eq!(store.len(), 0);
    assert!(store.is_empty());
    assert_eq!(store.events().count(), 0);
    assert_eq!(store.unread_summary(), None);
}

// ===========================================================================
// Recording basics: reason strings, unique ids, unread by default
// ===========================================================================

#[test]
fn record_preserves_the_design_example_reason_strings() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();

    store.record(
        &clock,
        transition(
            "api-gateway",
            "migrate-schema-v4",
            InterpretedStatus::Working,
            InterpretedStatus::WaitingForInput,
            "asked a question",
        ),
    );
    store.record(
        &clock,
        transition(
            "flightdeck",
            "flaky-e2e-runner",
            InterpretedStatus::Working,
            InterpretedStatus::Failed,
            "agent exited (code 1)",
        ),
    );
    store.record(
        &clock,
        transition(
            "flightdeck",
            "add-tests-api",
            InterpretedStatus::Working,
            InterpretedStatus::Completed,
            "finished, 18 files touched",
        ),
    );

    let reasons: Vec<&str> = store.events().map(|e| e.reason.as_str()).collect();
    assert_eq!(
        reasons,
        vec![
            "asked a question",
            "agent exited (code 1)",
            "finished, 18 files touched",
        ]
    );

    // Every event starts unread.
    assert!(store.events().all(|e| !e.read));
}

#[test]
fn record_returns_unique_monotonic_ids() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    let id1 = store.record(
        &clock,
        transition(
            "p",
            "s1",
            InterpretedStatus::Idle,
            InterpretedStatus::Working,
            "resumed",
        ),
    );
    let id2 = store.record(
        &clock,
        transition(
            "p",
            "s2",
            InterpretedStatus::Idle,
            InterpretedStatus::Working,
            "resumed",
        ),
    );
    assert_ne!(id1, id2);
    let ids: Vec<&EventId> = store.events().map(|e| &e.event_id).collect();
    assert_eq!(ids, vec![&id1, &id2]);
}

// ===========================================================================
// Tier derivation reuses the committed precedence functions
// ===========================================================================

#[test]
fn tier_derivation_matches_documented_precedence_for_every_to_state() {
    let clock = FakeClock::default();
    let cases = [
        (InterpretedStatus::WaitingForInput, ActivityTier::Attention),
        (InterpretedStatus::NeedsAttention, ActivityTier::Attention),
        (InterpretedStatus::Failed, ActivityTier::Attention),
        (InterpretedStatus::SessionLost, ActivityTier::Attention),
        (InterpretedStatus::Idle, ActivityTier::Finished),
        (InterpretedStatus::Completed, ActivityTier::Finished),
        (InterpretedStatus::Stopped, ActivityTier::Finished),
        (InterpretedStatus::Recovered, ActivityTier::Finished),
        (InterpretedStatus::Working, ActivityTier::Quiet),
        (InterpretedStatus::Running, ActivityTier::Quiet),
        (InterpretedStatus::Starting, ActivityTier::Quiet),
        (InterpretedStatus::Unknown, ActivityTier::Quiet),
    ];
    for (to, expected_tier) in cases {
        let mut store = ActivityStore::new();
        store.record(&clock, transition("p", "s", to, to, "x"));
        let tier = store.events().next().unwrap().tier;
        assert_eq!(tier, expected_tier, "to={to:?} expected {expected_tier:?}");
    }
}

// ===========================================================================
// "Unknown stays unknown" — never an inferred transition
// ===========================================================================

#[test]
fn unknown_lifecycle_agent_records_unknown_to_unknown_verbatim() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    store.record(
        &clock,
        transition(
            "flightdeck",
            "hotfix-csp-header",
            InterpretedStatus::Unknown,
            InterpretedStatus::Unknown,
            "Codex CLI reports no lifecycle",
        ),
    );

    let stored = store.events().next().unwrap();
    // Never smoothed into Idle, Completed, or anything else that would read
    // as a confident guess.
    assert_eq!(stored.from, InterpretedStatus::Unknown);
    assert_eq!(stored.to, InterpretedStatus::Unknown);
    assert_eq!(stored.tier, ActivityTier::Quiet);
    assert_eq!(stored.reason, "Codex CLI reports no lifecycle");
}

#[test]
fn manual_override_transition_is_recorded_alongside_the_lifecycle_pair() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    store.record(
        &clock,
        Transition {
            manual: Some(ManualStatus::InProgress),
            ..transition(
                "web",
                "perf-audit-images",
                InterpretedStatus::Idle,
                InterpretedStatus::Idle,
                "set by hand on the desktop",
            )
        },
    );
    let stored = store.events().next().unwrap();
    assert_eq!(stored.manual, Some(ManualStatus::InProgress));
    assert_eq!(stored.reason, "set by hand on the desktop");
}

// ===========================================================================
// Global ordering across projects (D11)
// ===========================================================================

#[test]
fn ordering_is_global_across_projects_not_partitioned() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    store.record(
        &clock,
        transition(
            "api-gateway",
            "s1",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "first",
        ),
    );
    store.record(
        &clock,
        transition(
            "web",
            "s2",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "second",
        ),
    );
    store.record(
        &clock,
        transition(
            "api-gateway",
            "s3",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "third",
        ),
    );

    let reasons: Vec<&str> = store.events().map(|e| e.reason.as_str()).collect();
    assert_eq!(reasons, vec!["first", "second", "third"]);
}

// ===========================================================================
// Arrival order vs timestamp order (documented API decision)
// ===========================================================================

#[test]
fn arrival_order_is_preserved_even_when_the_clock_steps_backward() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();

    clock.set_millis(10_000);
    store.record(
        &clock,
        transition(
            "p",
            "s1",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "later-clock-first-call",
        ),
    );

    // Clock steps backward (e.g. NTP correction, or a test's fake clock).
    clock.set_millis(1_000);
    store.record(
        &clock,
        transition(
            "p",
            "s2",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "earlier-clock-second-call",
        ),
    );

    // The store keeps call order, not timestamp order: it does not re-sort
    // the feed around one glitchy clock reading.
    let reasons: Vec<&str> = store.events().map(|e| e.reason.as_str()).collect();
    assert_eq!(
        reasons,
        vec!["later-clock-first-call", "earlier-clock-second-call"]
    );
    let at_ms: Vec<i64> = store.events().map(|e| e.at_ms).collect();
    assert_eq!(at_ms, vec![10_000, 1_000]);
}

// ===========================================================================
// Eviction: count bound
// ===========================================================================

#[test]
fn eviction_at_exactly_201_events_drops_only_the_oldest() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    for i in 0..MAX_EVENTS {
        store.record(
            &clock,
            transition(
                "p",
                "s",
                InterpretedStatus::Working,
                InterpretedStatus::Idle,
                &format!("event-{i}"),
            ),
        );
    }
    assert_eq!(store.len(), MAX_EVENTS);

    // The 201st push must evict exactly the oldest one.
    store.record(
        &clock,
        transition(
            "p",
            "s",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "event-200",
        ),
    );
    assert_eq!(store.len(), MAX_EVENTS);
    let reasons: Vec<&str> = store.events().map(|e| e.reason.as_str()).collect();
    assert_eq!(reasons.first(), Some(&"event-1"));
    assert_eq!(reasons.last(), Some(&"event-200"));
    assert!(!reasons.contains(&"event-0"));
}

#[test]
fn many_events_all_recent_are_capped_by_count_alone() {
    let clock = FakeClock::default();
    clock.set_millis(1_000_000);
    let mut store = ActivityStore::new();
    for i in 0..250 {
        store.record(
            &clock,
            transition(
                "p",
                "s",
                InterpretedStatus::Working,
                InterpretedStatus::Idle,
                &format!("event-{i}"),
            ),
        );
    }
    // All 250 are well within 24h of each other (clock never advanced), so
    // only the count bound is in play.
    assert_eq!(store.len(), MAX_EVENTS);
}

// ===========================================================================
// Eviction: age bound, with an injected clock (no sleeping)
// ===========================================================================

#[test]
fn few_events_all_old_are_evicted_by_the_24h_bound_alone() {
    let clock = FakeClock::default();
    clock.set_millis(0);
    let mut store = ActivityStore::new();
    for i in 0..5 {
        store.record(
            &clock,
            transition(
                "p",
                "s",
                InterpretedStatus::Working,
                InterpretedStatus::Idle,
                &format!("event-{i}"),
            ),
        );
    }
    assert_eq!(store.len(), 5);

    // Advance the clock past 24h with no new writes, then evict explicitly —
    // this is the "read-time aging" the module doc describes: nothing ages
    // out spontaneously, but a caller that calls `evict` before reading sees
    // the honest, aged-down feed.
    clock.set_millis(MAX_AGE_MS as u64 + 1);
    store.evict(&clock);
    assert_eq!(store.len(), 0, "all events should have aged out");
    assert_eq!(store.unread_summary(), None);
}

#[test]
fn evict_without_any_new_record_call_still_ages_the_store_down() {
    // Distinguishes "evict only runs inside record" from "evict is a
    // standalone, callable-anytime operation" (module doc: callers reading
    // the feed after a quiet stretch should call `evict` themselves).
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    store.record(
        &clock,
        transition(
            "p",
            "s",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "only-event",
        ),
    );
    assert_eq!(store.len(), 1);

    clock.advance_millis(MAX_AGE_MS as u64 + 1);
    assert_eq!(store.len(), 1, "no new record call yet, so nothing pruned");

    store.evict(&clock);
    assert_eq!(store.len(), 0);
}

#[test]
fn both_bounds_interact_old_excess_events_evicted_by_age_recent_ones_kept() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();

    // Five very old events.
    clock.set_millis(0);
    for i in 0..5 {
        store.record(
            &clock,
            transition(
                "p",
                "s",
                InterpretedStatus::Working,
                InterpretedStatus::Idle,
                &format!("old-{i}"),
            ),
        );
    }

    // Jump forward past the age bound, then record a few recent ones.
    clock.set_millis(MAX_AGE_MS as u64 + 1);
    for i in 0..3 {
        store.record(
            &clock,
            transition(
                "p",
                "s",
                InterpretedStatus::Working,
                InterpretedStatus::Idle,
                &format!("new-{i}"),
            ),
        );
    }

    // The last `record` call's internal `evict` should have dropped the five
    // old ones (older than the 24h cutoff from the new "now") and kept the
    // three recent ones.
    let reasons: Vec<&str> = store.events().map(|e| e.reason.as_str()).collect();
    assert_eq!(reasons, vec!["new-0", "new-1", "new-2"]);
}

// ===========================================================================
// Read/unread state
// ===========================================================================

#[test]
fn mark_read_flags_one_event_and_reports_whether_it_existed() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    let id = store.record(
        &clock,
        transition(
            "p",
            "s",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "x",
        ),
    );

    assert!(store.mark_read(&id));
    assert!(store.events().next().unwrap().read);

    // A second call is a harmless no-op that still reports success (found).
    assert!(store.mark_read(&id));

    // An id that was never issued reports false.
    assert!(!store.mark_read(&EventId::new("evt-nonexistent")));
}

#[test]
fn mark_all_read_flags_every_retained_event() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    for i in 0..4 {
        store.record(
            &clock,
            transition(
                "p",
                "s",
                InterpretedStatus::Working,
                InterpretedStatus::Idle,
                &format!("e{i}"),
            ),
        );
    }
    store.mark_all_read();
    assert!(store.events().all(|e| e.read));
    assert_eq!(store.unread_summary(), None);
}

// ===========================================================================
// Most-urgent-unread: every tier combination, all-read, and empty
// ===========================================================================

#[test]
fn most_urgent_unread_is_none_for_empty_store() {
    let store = ActivityStore::new();
    assert_eq!(store.unread_summary(), None);
}

#[test]
fn most_urgent_unread_is_none_when_everything_is_read() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    store.record(
        &clock,
        transition(
            "p",
            "s",
            InterpretedStatus::Working,
            InterpretedStatus::WaitingForInput,
            "asked a question",
        ),
    );
    store.mark_all_read();
    assert_eq!(store.unread_summary(), None);
}

#[test]
fn most_urgent_unread_is_quiet_when_only_quiet_events_are_unread() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    store.record(
        &clock,
        transition(
            "p",
            "s",
            InterpretedStatus::Idle,
            InterpretedStatus::Working,
            "resumed",
        ),
    );
    let summary = store.unread_summary().unwrap();
    assert_eq!(summary.tier, ActivityTier::Quiet);
    assert_eq!(summary.count_at_tier, 1);
    assert_eq!(summary.total_unread, 1);
}

#[test]
fn most_urgent_unread_is_finished_when_finished_and_quiet_are_both_unread() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    store.record(
        &clock,
        transition(
            "p",
            "s1",
            InterpretedStatus::Idle,
            InterpretedStatus::Working,
            "quiet",
        ),
    );
    store.record(
        &clock,
        transition(
            "p",
            "s2",
            InterpretedStatus::Working,
            InterpretedStatus::Completed,
            "finished, 18 files touched",
        ),
    );
    // Finished must beat quiet even though quiet was recorded first — this
    // is the case that would come out backwards if `unread_summary` reused
    // `StatusBucket::rank` instead of its own `tier_rank` (see module doc).
    let summary = store.unread_summary().unwrap();
    assert_eq!(summary.tier, ActivityTier::Finished);
    assert_eq!(summary.count_at_tier, 1);
    assert_eq!(summary.total_unread, 2);
}

#[test]
fn most_urgent_unread_is_attention_when_all_three_tiers_are_unread() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    store.record(
        &clock,
        transition(
            "p",
            "s1",
            InterpretedStatus::Idle,
            InterpretedStatus::Working,
            "quiet",
        ),
    );
    store.record(
        &clock,
        transition(
            "p",
            "s2",
            InterpretedStatus::Working,
            InterpretedStatus::Completed,
            "finished",
        ),
    );
    store.record(
        &clock,
        transition(
            "p",
            "s3",
            InterpretedStatus::Working,
            InterpretedStatus::WaitingForInput,
            "asked a question",
        ),
    );
    store.record(
        &clock,
        transition(
            "p",
            "s4",
            InterpretedStatus::Working,
            InterpretedStatus::Failed,
            "agent exited (code 1)",
        ),
    );

    let summary = store.unread_summary().unwrap();
    assert_eq!(summary.tier, ActivityTier::Attention);
    // Waiting and Error both map to Attention (protocol.rs), so both count.
    assert_eq!(summary.count_at_tier, 2);
    assert_eq!(summary.total_unread, 4);
}

#[test]
fn most_urgent_unread_ignores_already_read_events_at_a_more_urgent_tier() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    let attention_id = store.record(
        &clock,
        transition(
            "p",
            "s1",
            InterpretedStatus::Working,
            InterpretedStatus::WaitingForInput,
            "asked a question",
        ),
    );
    store.record(
        &clock,
        transition(
            "p",
            "s2",
            InterpretedStatus::Working,
            InterpretedStatus::Completed,
            "finished",
        ),
    );

    // Read the attention-tier event; the finished one should now be most
    // urgent among what remains unread.
    assert!(store.mark_read(&attention_id));
    let summary = store.unread_summary().unwrap();
    assert_eq!(summary.tier, ActivityTier::Finished);
    assert_eq!(summary.count_at_tier, 1);
    assert_eq!(summary.total_unread, 1);
}

// ===========================================================================
// `observe`: what the host may honestly say about a status change (§5.1)
// ===========================================================================

fn display(
    process: ProcessState,
    interpreted: InterpretedStatus,
    manual: Option<ManualStatus>,
) -> DisplayStatus {
    DisplayStatus {
        process,
        interpreted,
        manual,
    }
}

/// A well-behaved agent that reports a lifecycle, working then waiting. The
/// design's row reads `in progress → waiting · asked a question`, and *asked a
/// question* is the half the host cannot know: the status hook writes the word
/// `waiting`, never the reason for it.
#[test]
fn a_waiting_agent_gets_no_reason_because_why_it_waits_never_reaches_the_host() {
    let observed = observe(
        display(ProcessState::Running, InterpretedStatus::Working, None),
        display(
            ProcessState::Running,
            InterpretedStatus::WaitingForInput,
            None,
        ),
        "Claude Code",
        true,
    );
    assert_eq!(observed.from, InterpretedStatus::Working);
    assert_eq!(observed.to, InterpretedStatus::WaitingForInput);
    assert_eq!(
        observed.reason, "",
        "empty, never the design's `asked a question` — that is a guess about \
         the agent's intent, and §5.1 forbids padding the reason with one"
    );
}

/// The other row the status pair cannot explain: `finished, 18 files touched`.
/// [`observe`] still refuses to invent one — the count is not in the statuses
/// and never will be. It arrives separately, from the finish-edge git refresh
/// exercised further down (see [`wants_file_count`] and [`PendingFinishes`]),
/// which is exactly why `observe` leaving this empty is the correct handoff
/// rather than a gap.
#[test]
fn a_finished_agent_gets_no_reason_rather_than_an_invented_file_count() {
    let observed = observe(
        display(ProcessState::Running, InterpretedStatus::Working, None),
        display(ProcessState::Running, InterpretedStatus::Idle, None),
        "OpenCode",
        true,
    );
    assert_eq!(observed.to, InterpretedStatus::Idle);
    assert_eq!(observed.reason, "");
}

#[test]
fn a_non_zero_exit_reports_the_code_the_process_actually_returned() {
    let observed = observe(
        display(ProcessState::Running, InterpretedStatus::Working, None),
        display(ProcessState::Exited(1), InterpretedStatus::Failed, None),
        "Claude Code",
        true,
    );
    assert_eq!(observed.to, InterpretedStatus::Failed);
    assert_eq!(observed.reason, "agent exited (code 1)");
}

#[test]
fn a_clean_exit_reports_its_code_too_rather_than_going_silent() {
    let observed = observe(
        display(ProcessState::Running, InterpretedStatus::Working, None),
        display(ProcessState::Exited(0), InterpretedStatus::Completed, None),
        "Claude Code",
        true,
    );
    assert_eq!(observed.reason, "agent exited (code 0)");
}

/// `to: Failed` cannot distinguish "ran and returned 1" from "never started",
/// so the reason is where that fact has to live.
#[test]
fn an_agent_that_never_started_says_so_rather_than_reporting_an_exit_code() {
    let observed = observe(
        display(ProcessState::Starting, InterpretedStatus::Starting, None),
        display(ProcessState::Failed, InterpretedStatus::Failed, None),
        "Claude Code",
        true,
    );
    assert_eq!(observed.reason, "agent failed to start");
}

#[test]
fn a_lost_session_gets_no_reason_because_to_already_says_it() {
    let observed = observe(
        display(ProcessState::Running, InterpretedStatus::Working, None),
        display(ProcessState::Lost, InterpretedStatus::SessionLost, None),
        "Claude Code",
        true,
    );
    assert_eq!(observed.to, InterpretedStatus::SessionLost);
    assert_eq!(observed.reason, "");
}

#[test]
fn a_manual_override_is_reported_as_set_by_hand_on_the_desktop() {
    let observed = observe(
        display(ProcessState::Running, InterpretedStatus::Idle, None),
        display(
            ProcessState::Running,
            InterpretedStatus::Idle,
            Some(ManualStatus::InProgress),
        ),
        "Claude Code",
        true,
    );
    assert_eq!(observed.reason, "set by hand on the desktop");
    assert_eq!(observed.manual, Some(ManualStatus::InProgress));
    assert_eq!(
        (observed.from, observed.to),
        (InterpretedStatus::Idle, InterpretedStatus::Idle),
        "the agent itself did not move, and saying it did would be a fabrication"
    );
}

/// §5.1's hard rule, and the honesty this whole module exists to protect: an
/// agent with no lifecycle hooks is `unknown → unknown`, **even though the
/// process state would happily imply a transition**. `Exited(0)` → "completed"
/// is exactly the inference the spec forbids.
#[test]
fn an_agent_with_no_lifecycle_hooks_stays_unknown_at_both_ends() {
    let observed = observe(
        display(ProcessState::Running, InterpretedStatus::Unknown, None),
        display(ProcessState::Exited(0), InterpretedStatus::Completed, None),
        "Codex CLI",
        false,
    );
    assert_eq!(observed.from, InterpretedStatus::Unknown);
    assert_eq!(
        observed.to,
        InterpretedStatus::Unknown,
        "the process really did exit 0, but this agent never told us what that \
         meant for its turn, so the feed must not translate it into `completed`"
    );
    assert_eq!(observed.reason, "Codex CLI reports no lifecycle");
}

/// The reason wording matches the browser's own `lifecycleNote`
/// (`${agent_display_name} reports no lifecycle`), so a feed row and a sidebar
/// row cannot disagree about the same agent.
#[test]
fn the_no_lifecycle_reason_names_the_agent_the_way_the_sidebar_does() {
    let observed = observe(
        display(ProcessState::NotStarted, InterpretedStatus::Unknown, None),
        display(ProcessState::Running, InterpretedStatus::Running, None),
        "my-custom-wrapper",
        false,
    );
    assert_eq!(observed.reason, "my-custom-wrapper reports no lifecycle");
}

/// Both honesty rules compose: a hand-set status on a no-lifecycle agent gets
/// the reason the user's own action earns, and still refuses to name a
/// lifecycle transition nobody reported.
#[test]
fn a_manual_override_on_a_no_lifecycle_agent_keeps_unknown_at_both_ends() {
    let observed = observe(
        display(ProcessState::Running, InterpretedStatus::Unknown, None),
        display(
            ProcessState::Running,
            InterpretedStatus::Unknown,
            Some(ManualStatus::Done),
        ),
        "Codex CLI",
        false,
    );
    assert_eq!(observed.reason, "set by hand on the desktop");
    assert_eq!(observed.from, InterpretedStatus::Unknown);
    assert_eq!(observed.to, InterpretedStatus::Unknown);
    assert_eq!(observed.manual, Some(ManualStatus::Done));
}

/// The tier a row lands in is derived from `to` by the store, so the honesty
/// above has to survive the round trip into an event: an unknown-lifecycle row
/// must be `quiet`, not `finished`, however cleanly the process exited.
#[test]
fn an_unknown_lifecycle_row_is_recorded_quiet_not_finished() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    let observed = observe(
        display(ProcessState::Running, InterpretedStatus::Unknown, None),
        display(ProcessState::Exited(0), InterpretedStatus::Completed, None),
        "Codex CLI",
        false,
    );
    store.record(
        &clock,
        Transition {
            project_id: ProjectId::new("flightdeck"),
            project_name: "flightdeck".to_string(),
            session_id: TabId("hotfix-csp-header".to_string()),
            session_name: "hotfix-csp-header".to_string(),
            from: observed.from,
            to: observed.to,
            manual: observed.manual,
            reason: observed.reason,
        },
    );
    let event = store.events().next().expect("one event");
    assert_eq!(event.tier, ActivityTier::Quiet);
    assert_eq!(event.reason, "Codex CLI reports no lifecycle");
}

// ===========================================================================
// `apply_mark_read`: the browser's read-marking command (D11)
// ===========================================================================

fn mark_read_command(seq: u64, args: Option<serde_json::Value>) -> Command {
    Command {
        seq,
        name: crate::web::protocol::command::MARK_ACTIVITY_READ.to_string(),
        args,
    }
}

/// Two unread events, ids returned in record order.
fn store_with_two_unread() -> (FakeClock, ActivityStore, EventId, EventId) {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    let first = store.record(
        &clock,
        transition(
            "p",
            "s1",
            InterpretedStatus::Working,
            InterpretedStatus::WaitingForInput,
            "",
        ),
    );
    let second = store.record(
        &clock,
        transition(
            "p",
            "s2",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "",
        ),
    );
    (clock, store, first, second)
}

#[test]
fn marking_named_events_read_applies_and_leaves_the_rest_unread() {
    let (_clock, mut store, first, _second) = store_with_two_unread();
    let ack = apply_mark_read(
        &mut store,
        &mark_read_command(
            7,
            Some(serde_json::json!({ "event_ids": [first.as_str()] })),
        ),
    );
    assert_eq!(ack.seq, 7);
    assert_eq!(ack.outcome, AckOutcome::Applied);
    assert_eq!(ack.detail, None);
    let summary = store.unread_summary().expect("one event is still unread");
    assert_eq!(summary.total_unread, 1);
    assert_eq!(summary.tier, ActivityTier::Finished);
}

/// "Mark all read" has no ids to name, so an absent argument object *is* the
/// request rather than a malformed one.
#[test]
fn a_command_with_no_args_marks_everything_read() {
    let (_clock, mut store, _first, _second) = store_with_two_unread();
    let ack = apply_mark_read(&mut store, &mark_read_command(1, None));
    assert_eq!(ack.outcome, AckOutcome::Applied);
    assert_eq!(store.unread_summary(), None);
}

#[test]
fn an_args_object_without_event_ids_marks_everything_read() {
    let (_clock, mut store, _first, _second) = store_with_two_unread();
    let ack = apply_mark_read(
        &mut store,
        &mark_read_command(1, Some(serde_json::json!({}))),
    );
    assert_eq!(ack.outcome, AckOutcome::Applied);
    assert_eq!(store.unread_summary(), None);
}

/// An id the store no longer retains is a no-op, not an error — §5.1's bounds
/// evicted it while the tab still held it. But nothing was done, so claiming
/// `Applied` would be a small lie.
#[test]
fn an_id_the_store_no_longer_retains_is_ignored_not_applied() {
    let (_clock, mut store, _first, _second) = store_with_two_unread();
    let ack = apply_mark_read(
        &mut store,
        &mark_read_command(3, Some(serde_json::json!({ "event_ids": ["evt-evicted"] }))),
    );
    assert_eq!(ack.outcome, AckOutcome::Ignored);
    assert_eq!(ack.detail.as_deref(), Some("no retained event matched"));
    assert_eq!(
        store
            .unread_summary()
            .expect("nothing was marked")
            .total_unread,
        2
    );
}

#[test]
fn a_partly_evicted_list_applies_and_says_how_many_missed() {
    let (_clock, mut store, first, _second) = store_with_two_unread();
    let ack = apply_mark_read(
        &mut store,
        &mark_read_command(
            4,
            Some(serde_json::json!({ "event_ids": [first.as_str(), "evt-evicted"] })),
        ),
    );
    assert_eq!(ack.outcome, AckOutcome::Applied);
    assert_eq!(
        ack.detail.as_deref(),
        Some("1 of 2 events are no longer retained"),
        "the browser hears about the miss instead of being silently misled"
    );
}

#[test]
fn an_empty_list_asks_for_nothing_and_is_ignored() {
    let (_clock, mut store, _first, _second) = store_with_two_unread();
    let ack = apply_mark_read(
        &mut store,
        &mark_read_command(5, Some(serde_json::json!({ "event_ids": [] }))),
    );
    assert_eq!(ack.outcome, AckOutcome::Ignored);
    assert_eq!(ack.detail.as_deref(), Some("no event ids given"));
    assert_eq!(
        store
            .unread_summary()
            .expect("nothing was marked")
            .total_unread,
        2
    );
}

#[test]
fn event_ids_that_is_not_a_list_is_rejected_with_a_reason() {
    let (_clock, mut store, _first, _second) = store_with_two_unread();
    let ack = apply_mark_read(
        &mut store,
        &mark_read_command(6, Some(serde_json::json!({ "event_ids": "evt-1" }))),
    );
    assert_eq!(ack.outcome, AckOutcome::Rejected);
    let detail = ack.detail.expect("a rejection states why");
    assert!(detail.contains("event_ids"), "got: {detail}");
    assert_eq!(
        store
            .unread_summary()
            .expect("nothing was marked")
            .total_unread,
        2
    );
}

#[test]
fn a_non_string_entry_in_event_ids_is_rejected_rather_than_skipped() {
    let (_clock, mut store, first, _second) = store_with_two_unread();
    let ack = apply_mark_read(
        &mut store,
        &mark_read_command(
            8,
            Some(serde_json::json!({ "event_ids": [first.as_str(), 42] })),
        ),
    );
    assert_eq!(
        ack.outcome,
        AckOutcome::Rejected,
        "a malformed frame is the browser's bug and deserves to hear about it"
    );
    assert_eq!(
        store
            .unread_summary()
            .expect("nothing was marked")
            .total_unread,
        2,
        "a rejected frame must not half-apply"
    );
}

/// Read state is host state, not per-tab state (D11): whoever marks it read
/// changes what the *next* snapshot backfills for everyone.
#[test]
fn marking_read_survives_into_the_events_a_later_reader_sees() {
    let (_clock, mut store, first, second) = store_with_two_unread();
    let ack = apply_mark_read(
        &mut store,
        &mark_read_command(
            9,
            Some(serde_json::json!({ "event_ids": [first.as_str(), second.as_str()] })),
        ),
    );
    assert_eq!(ack.outcome, AckOutcome::Applied);
    assert!(
        store.events().all(|event| event.read),
        "a second tab attaching now must not be shown a wall of unread"
    );
}

// ===========================================================================
// `finished, N files touched`: the one-shot git refresh at the finish edge
// ===========================================================================

use crate::testing::FakeGit;
use std::path::PathBuf;

/// The worktree every git assertion below asks about.
fn worktree() -> PathBuf {
    PathBuf::from("/repo/worktrees/add-tests-api")
}

/// A finished transition with the empty reason `observe` hands over.
fn finished() -> Observed {
    observe(
        display(ProcessState::Running, InterpretedStatus::Working, None),
        display(ProcessState::Running, InterpretedStatus::Idle, None),
        "Claude Code",
        true,
    )
}

/// The row the store writes for a parked transition, once.
fn only_event(store: &ActivityStore) -> &ActivityEvent {
    let mut events = store.events();
    let event = events.next().expect("exactly one event");
    assert!(events.next().is_none(), "exactly one event");
    event
}

/// The acceptance criterion in one line: the number on the row is the number
/// git reported for *that* worktree at *that* moment — 18 changed paths, the
/// design's own example, spread across all three porcelain categories so the
/// count is a real parse rather than a line tally that happens to agree.
#[test]
fn the_file_count_is_whatever_git_reports_for_that_worktree() {
    let git = FakeGit::new();
    let mut lines: Vec<String> = (0..10).map(|i| format!(" M src/mod-{i}.rs")).collect();
    lines.extend((0..5).map(|i| format!("?? src/new-{i}.rs")));
    lines.extend((0..3).map(|i| format!(" D src/gone-{i}.rs")));
    git.set_porcelain_at(worktree(), lines);

    assert_eq!(file_count(&git, &worktree()), Some(18));
}

/// A different worktree is a different question. The finish edge asks about the
/// tab that finished, so the count must be keyed by path and not by repo.
#[test]
fn the_file_count_is_per_worktree_not_per_repo() {
    let git = FakeGit::new();
    git.set_porcelain_at("/repo/worktrees/a", [" M one.rs", " M two.rs"]);
    git.set_porcelain_at("/repo/worktrees/b", [" M only.rs"]);

    assert_eq!(file_count(&git, Path::new("/repo/worktrees/a")), Some(2));
    assert_eq!(file_count(&git, Path::new("/repo/worktrees/b")), Some(1));
}

/// A clean worktree is a real answer, not a missing one. `Some(0)` and `None`
/// are different facts and the row says different things about them.
#[test]
fn a_clean_worktree_counts_zero_rather_than_refusing_to_answer() {
    let git = FakeGit::new();
    git.set_porcelain_at(worktree(), Vec::<String>::new());

    assert_eq!(file_count(&git, &worktree()), Some(0));
}

/// **The honest-empty source.** Git that cannot answer — a locked index, a
/// worktree that has been removed, no `git` on `PATH` — yields no number at
/// all, never a zero standing in for one.
#[test]
fn git_that_cannot_answer_yields_no_count_rather_than_zero() {
    let git = FakeGit::new();
    git.set_porcelain_at(worktree(), [" M src/lib.rs"]);
    git.set_porcelain_error("fatal: unable to read index");

    assert_eq!(file_count(&git, &worktree()), None);
}

#[test]
fn a_finished_edge_with_an_empty_reason_wants_a_count() {
    assert!(wants_file_count(&finished()));
}

/// Every reason `observe` already found is a better explanation than a file
/// count, so none of them is overwritten by one.
#[test]
fn an_edge_that_already_has_a_reason_does_not_want_a_count() {
    let manual = observe(
        display(ProcessState::Running, InterpretedStatus::Idle, None),
        display(
            ProcessState::Running,
            InterpretedStatus::Idle,
            Some(ManualStatus::Done),
        ),
        "Claude Code",
        true,
    );
    assert_eq!(manual.reason, "set by hand on the desktop");
    assert!(!wants_file_count(&manual));

    let exited = observe(
        display(ProcessState::Running, InterpretedStatus::Working, None),
        display(ProcessState::Exited(0), InterpretedStatus::Completed, None),
        "Claude Code",
        true,
    );
    assert_eq!(exited.reason, "agent exited (code 0)");
    assert!(!wants_file_count(&exited));

    let no_lifecycle = observe(
        display(ProcessState::Running, InterpretedStatus::Unknown, None),
        display(ProcessState::Exited(0), InterpretedStatus::Completed, None),
        "Codex CLI",
        false,
    );
    assert_eq!(no_lifecycle.reason, "Codex CLI reports no lifecycle");
    assert!(!wants_file_count(&no_lifecycle));
}

/// Only the `finished` tier earns the clause. An agent that stopped to ask a
/// question has an empty reason too, and `finished, 3 files touched` on that
/// row would be a flat lie about what just happened.
#[test]
fn only_a_finished_tier_edge_wants_a_count() {
    let waiting = observe(
        display(ProcessState::Running, InterpretedStatus::Working, None),
        display(
            ProcessState::Running,
            InterpretedStatus::WaitingForInput,
            None,
        ),
        "Claude Code",
        true,
    );
    assert_eq!(waiting.reason, "");
    assert!(!wants_file_count(&waiting));

    let lost = observe(
        display(ProcessState::Running, InterpretedStatus::Working, None),
        display(ProcessState::Lost, InterpretedStatus::SessionLost, None),
        "Claude Code",
        true,
    );
    assert_eq!(lost.reason, "");
    assert!(!wants_file_count(&lost));

    let started = observe(
        display(ProcessState::Running, InterpretedStatus::Idle, None),
        display(ProcessState::Running, InterpretedStatus::Working, None),
        "Claude Code",
        true,
    );
    assert_eq!(started.reason, "");
    assert!(!wants_file_count(&started));
}

/// End to end through the seam: park the row the finish edge produced, ask the
/// fake git, resolve with what it said, and read the design's sentence off the
/// recorded event.
#[test]
fn a_resolved_row_carries_the_count_git_reported() {
    let clock = FakeClock::default();
    let git = FakeGit::new();
    git.set_porcelain_at(
        worktree(),
        (0..18)
            .map(|i| format!(" M src/file-{i}.rs"))
            .collect::<Vec<_>>(),
    );

    let mut store = ActivityStore::new();
    let mut pending = PendingFinishes::new();
    let id = pending.park(
        1_700,
        transition(
            "flightdeck",
            "add-tests-api",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "",
        ),
    );
    assert_eq!(store.len(), 0, "nothing is published while git is asked");
    assert_eq!(pending.len(), 1);

    let files = file_count(&git, &worktree());
    assert!(pending.resolve(&mut store, &clock, id, files).is_some());

    let event = only_event(&store);
    assert_eq!(event.reason, "finished, 18 files touched");
    assert_eq!(event.tier, ActivityTier::Finished);
    assert!(pending.is_empty());
}

/// One file is one file. The design's plural is a plural, not a template.
#[test]
fn a_single_touched_file_is_not_reported_as_1_files() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    let mut pending = PendingFinishes::new();
    let id = pending.park(
        1_700,
        transition(
            "flightdeck",
            "add-tests-api",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "",
        ),
    );
    pending.resolve(&mut store, &clock, id, Some(1));

    assert_eq!(only_event(&store).reason, "finished, 1 file touched");
}

/// A clean finish says so. `Some(0)` is a fact git supplied, and the row is
/// allowed to state it — this is the one place a number may be zero without
/// being a stand-in for "we don't know".
#[test]
fn a_finish_that_touched_nothing_says_zero_rather_than_going_silent() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    let mut pending = PendingFinishes::new();
    let id = pending.park(
        1_700,
        transition(
            "flightdeck",
            "add-tests-api",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "",
        ),
    );
    pending.resolve(&mut store, &clock, id, Some(0));

    assert_eq!(only_event(&store).reason, "finished, 0 files touched");
}

/// **The honest-empty path, preserved.** Git said nothing usable, so the row
/// renders exactly as it did before this mechanism existed: present, correctly
/// tiered, and silent about a number nobody can vouch for.
#[test]
fn a_row_git_could_not_count_is_recorded_without_the_clause() {
    let clock = FakeClock::default();
    let git = FakeGit::new();
    git.set_porcelain_error("fatal: not a git repository");

    let mut store = ActivityStore::new();
    let mut pending = PendingFinishes::new();
    let id = pending.park(
        1_700,
        transition(
            "flightdeck",
            "add-tests-api",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "",
        ),
    );
    let files = file_count(&git, &worktree());
    assert_eq!(files, None);
    pending.resolve(&mut store, &clock, id, files);

    let event = only_event(&store);
    assert_eq!(
        event.reason, "",
        "no number is obtainable, and §5.1 forbids padding the reason with a \
         plausible one"
    );
    assert_eq!(event.tier, ActivityTier::Finished);
    assert_eq!(event.to, InterpretedStatus::Idle);
}

/// A worker that never reports back must cost a clause, not a row. Nothing in
/// the wait is allowed to swallow the event D11 exists to deliver.
#[test]
fn a_row_nobody_ever_answers_for_is_published_after_the_deadline() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    let mut pending = PendingFinishes::new();
    pending.park(
        1_000,
        transition(
            "flightdeck",
            "add-tests-api",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "",
        ),
    );

    // One millisecond short of the deadline: still waiting, still unpublished.
    assert_eq!(
        pending.expire(&mut store, &clock, 1_000 + FINISH_COUNT_DEADLINE_MS - 1),
        0
    );
    assert_eq!(store.len(), 0);
    assert_eq!(pending.len(), 1);

    assert_eq!(
        pending.expire(&mut store, &clock, 1_000 + FINISH_COUNT_DEADLINE_MS),
        1
    );
    let event = only_event(&store);
    assert_eq!(event.reason, "");
    assert!(pending.is_empty());
}

/// The row is stamped with the finish, not with git's reply. A user reading
/// `2 minutes ago` is being told when their agent stopped working, and the
/// wait for a file count must not creep into that number.
#[test]
fn a_parked_row_keeps_the_timestamp_of_the_edge_not_of_the_answer() {
    let clock = FakeClock::default();
    clock.set_millis(9_000);
    let mut store = ActivityStore::new();
    let mut pending = PendingFinishes::new();
    let id = pending.park(
        1_700,
        transition(
            "flightdeck",
            "add-tests-api",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "",
        ),
    );
    pending.resolve(&mut store, &clock, id, Some(4));

    assert_eq!(
        only_event(&store).at_ms,
        1_700,
        "the clock has moved on since the edge; the row must not claim the \
         agent finished when git answered"
    );
}

/// A late answer for a row that already gave up must not record it twice — the
/// feed would then show one finish as two, which is worse than a missing count.
#[test]
fn an_answer_that_arrives_after_the_deadline_does_not_duplicate_the_row() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    let mut pending = PendingFinishes::new();
    let id = pending.park(
        1_000,
        transition(
            "flightdeck",
            "add-tests-api",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "",
        ),
    );
    assert_eq!(
        pending.expire(&mut store, &clock, 1_000 + FINISH_COUNT_DEADLINE_MS),
        1
    );

    assert!(
        pending.resolve(&mut store, &clock, id, Some(7)).is_none(),
        "the id is no longer held, so there is nothing to record"
    );
    assert_eq!(store.len(), 1);
    assert_eq!(only_event(&store).reason, "");
}

/// Two sessions finishing on the same tick each get their own count, and the
/// answers may come back in either order.
#[test]
fn two_finished_sessions_do_not_take_each_other_s_counts() {
    let clock = FakeClock::default();
    let mut store = ActivityStore::new();
    let mut pending = PendingFinishes::new();
    let first = pending.park(
        1_000,
        transition(
            "flightdeck",
            "add-tests-api",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "",
        ),
    );
    let second = pending.park(
        1_000,
        transition(
            "api-gateway",
            "migrate-schema-v4",
            InterpretedStatus::Working,
            InterpretedStatus::Idle,
            "",
        ),
    );

    // Answered out of order, as two worker threads will.
    pending.resolve(&mut store, &clock, second, Some(3));
    pending.resolve(&mut store, &clock, first, Some(18));

    let events: Vec<&ActivityEvent> = store.events().collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].session_name, "migrate-schema-v4");
    assert_eq!(events[0].reason, "finished, 3 files touched");
    assert_eq!(events[1].session_name, "add-tests-api");
    assert_eq!(events[1].reason, "finished, 18 files touched");
}
