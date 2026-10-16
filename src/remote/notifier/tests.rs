use super::*;
use crate::contracts::{InterpretedStatus as IS, NotificationSound};
use std::cell::RefCell;

use flightdeck_remote_protocol::{ProjectId, SessionId};

// --- arming / edge detection -----------------------------------------------

#[test]
fn working_then_idle_fires_finished() {
    let mut arm = EventArming::default();
    assert_eq!(arm.observe(IS::Working), None);
    assert_eq!(arm.observe(IS::Idle), Some(EventClass::Finished));
    // A quiet agent does not re-fire.
    assert_eq!(arm.observe(IS::Idle), None);
}

#[test]
fn working_then_needs_input_fires_needs_input() {
    let mut arm = EventArming::default();
    arm.observe(IS::Running);
    assert_eq!(
        arm.observe(IS::WaitingForInput),
        Some(EventClass::NeedsInput)
    );
}

#[test]
fn working_then_failed_fires_error() {
    let mut arm = EventArming::default();
    arm.observe(IS::Working);
    assert_eq!(arm.observe(IS::Failed), Some(EventClass::Error));
}

#[test]
fn settled_without_prior_active_does_not_fire() {
    let mut arm = EventArming::default();
    // Never armed → no event.
    assert_eq!(arm.observe(IS::Idle), None);
    assert_eq!(arm.observe(IS::Completed), None);
}

#[test]
fn neutral_status_disarms() {
    let mut arm = EventArming::default();
    arm.observe(IS::Working);
    // A neutral status (e.g. stopped) clears the arm without firing.
    assert_eq!(arm.observe(IS::Stopped), None);
    assert_eq!(arm.observe(IS::Idle), None);
}

#[test]
fn completed_counts_as_finished() {
    let mut arm = EventArming::default();
    arm.observe(IS::Working);
    assert_eq!(arm.observe(IS::Completed), Some(EventClass::Finished));
}

// --- event construction ----------------------------------------------------

fn ctx(class_preview: Option<&str>) -> EventContext {
    EventContext {
        event_id: EventId::new("ev:1"),
        deep_link: DeepLink {
            project_id: ProjectId::new("p"),
            session_id: SessionId::new("s"),
            item_id: None,
        },
        occurred_at_ms: 123,
        session_name: "add-tests".to_string(),
        preview: class_preview.map(|s| s.to_string()),
        files_changed: 3,
        ready_to_push: true,
        error_message: None,
    }
}

#[test]
fn build_finished_event() {
    let ev = build_event(EventClass::Finished, ctx(None));
    assert_eq!(ev.title, "add-tests finished its turn");
    match ev.kind {
        EventKind::Finished {
            files_changed,
            ready_to_push,
            ..
        } => {
            assert_eq!(files_changed, 3);
            assert!(ready_to_push);
        }
        other => panic!("expected finished, got {other:?}"),
    }
}

#[test]
fn build_needs_input_event_carries_preview() {
    let ev = build_event(EventClass::NeedsInput, ctx(Some("Proceed?")));
    assert_eq!(ev.title, "add-tests needs input");
    match ev.kind {
        EventKind::NeedsInput { preview } => assert_eq!(preview, "Proceed?"),
        other => panic!("expected needs-input, got {other:?}"),
    }
}

#[test]
fn build_error_event_has_default_message() {
    let ev = build_event(EventClass::Error, ctx(None));
    assert_eq!(ev.title, "add-tests hit an error");
    assert!(matches!(ev.kind, EventKind::Error { .. }));
}

// --- decorator delegates ---------------------------------------------------

#[derive(Default)]
struct RecordingNotifier {
    seen: RefCell<Vec<String>>,
}

impl Notifier for RecordingNotifier {
    fn notify(&self, n: &Notification) {
        self.seen.borrow_mut().push(n.title.clone());
    }
}

#[test]
fn composite_notifier_delegates_to_inner() {
    let inner = RecordingNotifier::default();
    let composite = CompositeNotifier::new(&inner);
    composite.notify(&Notification {
        title: "hello".to_string(),
        body: "body".to_string(),
        sound: NotificationSound::None,
    });
    assert_eq!(inner.seen.borrow().as_slice(), ["hello".to_string()]);
}

// --- push decision (remote-control-fglt) -----------------------------------
//
// `Sim` drives an `EventArming` + `PushDecider` exactly the way the bridge
// does each tick: the arming edge (suppressed inside the startup grace window)
// becomes a `SettleEdge` carrying a fresh feed event id, then the decider sees
// the session's status, the edge, the injected clock and the debounce. Time is
// plain numbers; nothing sleeps.

const DEBOUNCE_MS: u64 = 5_000;

struct Sim {
    arm: EventArming,
    push: PushDecider,
    sid: SessionId,
    grace_until_ms: u64,
    debounce_ms: u64,
    next_id: u32,
    /// Every push fired, with the tick it fired on.
    fired: Vec<(u64, PushFire)>,
}

impl Sim {
    fn new() -> Self {
        Sim {
            arm: EventArming::default(),
            push: PushDecider::new(),
            sid: SessionId::new("s1"),
            grace_until_ms: 0,
            debounce_ms: DEBOUNCE_MS,
            next_id: 0,
            fired: Vec::new(),
        }
    }

    /// One bridge tick for the session at `now_ms` with `status`.
    fn tick(&mut self, now_ms: u64, status: IS) -> Option<PushFire> {
        let grace = self.grace_until_ms;
        let edge = self.arm.observe(status).filter(|_| now_ms >= grace);
        let edge = edge.map(|class| {
            self.next_id += 1;
            SettleEdge {
                class,
                event_id: EventId::new(format!("e{}", self.next_id)),
            }
        });
        let fire = self
            .push
            .observe(&self.sid, status, edge, now_ms, self.debounce_ms);
        if let Some(f) = &fire {
            self.fired.push((now_ms, f.clone()));
        }
        fire
    }

    /// Hold `status` from `from` to `to` (inclusive), ticking every `step` ms.
    fn hold(&mut self, status: IS, from: u64, to: u64, step: u64) {
        let mut t = from;
        while t <= to {
            self.tick(t, status);
            t += step;
        }
    }

    fn classes(&self) -> Vec<PushClass> {
        self.fired.iter().map(|(_, f)| f.class).collect()
    }
}

#[test]
fn working_idle_working_within_the_window_pushes_nothing() {
    let mut sim = Sim::new();
    sim.hold(IS::Working, 0, 1_000, 100);
    // Idle for 3 s (under the 5 s window), then back to work.
    sim.hold(IS::Idle, 1_100, 4_000, 100);
    sim.hold(IS::Working, 4_100, 10_000, 100);
    // Another short pause, again under the window.
    sim.hold(IS::Idle, 10_100, 14_000, 100);
    sim.hold(IS::Working, 14_100, 30_000, 100);
    assert!(
        sim.fired.is_empty(),
        "flickers must not push: {:?}",
        sim.fired
    );
}

#[test]
fn idle_held_past_the_window_pushes_exactly_one_done() {
    let mut sim = Sim::new();
    sim.hold(IS::Working, 0, 1_000, 100);
    sim.hold(IS::Idle, 1_100, 60_000, 100);
    assert_eq!(sim.classes(), vec![PushClass::Done]);
    let (fired_at, fire) = &sim.fired[0];
    // Matures on the first tick at least the window after the settle...
    assert_eq!(*fired_at, 6_100);
    // ...but reports when the agent settled, under the feed event's id.
    assert_eq!(fire.occurred_at_ms, 1_100);
    assert_eq!(fire.event_id, Some(EventId::new("e1")));
}

#[test]
fn done_matures_exactly_at_the_window_boundary() {
    let mut sim = Sim::new();
    sim.tick(0, IS::Working);
    assert_eq!(sim.tick(1_000, IS::Idle), None);
    assert!(sim.push.has_pending_done(&sim.sid));
    assert_eq!(sim.tick(5_999, IS::Idle), None, "1 ms short of the window");
    let fire = sim.tick(6_000, IS::Idle).expect("done at the boundary");
    assert_eq!(fire.class, PushClass::Done);
    assert!(!sim.push.has_pending_done(&sim.sid));
    assert_eq!(sim.tick(60_000, IS::Idle), None, "never twice");
}

#[test]
fn a_flicker_restarts_the_window_from_the_last_settle() {
    let mut sim = Sim::new();
    sim.tick(0, IS::Working);
    sim.tick(1_000, IS::Idle); // window opens
    sim.tick(3_000, IS::Working); // cancelled
    sim.tick(4_000, IS::Idle); // window reopens at 4_000
    assert_eq!(
        sim.tick(6_000, IS::Idle),
        None,
        "the first window was cancelled"
    );
    assert_eq!(sim.tick(8_999, IS::Idle), None);
    let fire = sim
        .tick(9_000, IS::Idle)
        .expect("done 5 s after the last settle");
    assert_eq!(fire.class, PushClass::Done);
    assert_eq!(fire.occurred_at_ms, 4_000);
    assert_eq!(
        fire.event_id,
        Some(EventId::new("e2")),
        "the last settle's id"
    );
    assert_eq!(sim.classes(), vec![PushClass::Done]);
}

#[test]
fn completed_counts_as_done() {
    let mut sim = Sim::new();
    sim.tick(0, IS::Running);
    sim.tick(1_000, IS::Completed);
    assert_eq!(
        sim.tick(6_000, IS::Completed).map(|f| f.class),
        Some(PushClass::Done)
    );
}

#[test]
fn needs_input_pushes_immediately_and_once_per_edge() {
    let mut sim = Sim::new();
    sim.tick(0, IS::Working);
    let fire = sim.tick(1_000, IS::WaitingForInput).expect("immediate");
    assert_eq!(fire.class, PushClass::NeedsInput);
    assert_eq!(
        fire.event_id,
        Some(EventId::new("e1")),
        "the feed event's id"
    );
    assert_eq!(fire.occurred_at_ms, 1_000);
    // Still waiting: no repeat, however long.
    sim.hold(IS::WaitingForInput, 1_100, 60_000, 100);
    assert_eq!(sim.classes(), vec![PushClass::NeedsInput]);
    // Answered, works, asks again: a new edge, a new push.
    sim.hold(IS::Working, 60_100, 61_000, 100);
    assert_eq!(
        sim.tick(61_100, IS::NeedsAttention).map(|f| f.class),
        Some(PushClass::NeedsInput),
        "needs-attention (permission prompts) is needs-input"
    );
    assert_eq!(
        sim.classes(),
        vec![PushClass::NeedsInput, PushClass::NeedsInput]
    );
}

#[test]
fn error_pushes_immediately_and_once_per_edge() {
    let mut sim = Sim::new();
    sim.tick(0, IS::Working);
    let fire = sim.tick(1_000, IS::Failed).expect("immediate");
    assert_eq!(fire.class, PushClass::Error);
    assert_eq!(fire.event_id, Some(EventId::new("e1")));
    sim.hold(IS::Failed, 1_100, 30_000, 100);
    assert_eq!(sim.classes(), vec![PushClass::Error]);
    sim.tick(30_100, IS::Starting);
    assert_eq!(
        sim.tick(30_200, IS::Failed).map(|f| f.class),
        Some(PushClass::Error)
    );
    assert_eq!(sim.classes(), vec![PushClass::Error, PushClass::Error]);
}

#[test]
fn needs_input_then_idle_pushes_no_done() {
    let mut sim = Sim::new();
    sim.tick(0, IS::Working);
    sim.tick(1_000, IS::WaitingForInput);
    // The prompt is dismissed without the agent working again.
    sim.hold(IS::Idle, 2_000, 60_000, 100);
    assert_eq!(sim.classes(), vec![PushClass::NeedsInput]);
}

#[test]
fn idle_turning_into_needs_input_inside_the_window_pushes_it_and_drops_the_done() {
    let mut sim = Sim::new();
    sim.tick(0, IS::Working);
    sim.tick(1_000, IS::Idle); // done pending
    let fire = sim
        .tick(2_000, IS::WaitingForInput)
        .expect("the turn really ended on a question");
    assert_eq!(fire.class, PushClass::NeedsInput);
    assert_eq!(
        fire.event_id, None,
        "no feed event matches: caller mints one"
    );
    assert_eq!(fire.occurred_at_ms, 2_000);
    // Back to idle and held: the done was dropped, nothing more.
    sim.hold(IS::Idle, 3_000, 60_000, 100);
    assert_eq!(sim.classes(), vec![PushClass::NeedsInput]);
}

#[test]
fn idle_turning_into_failed_inside_the_window_pushes_error() {
    let mut sim = Sim::new();
    sim.tick(0, IS::Working);
    sim.tick(1_000, IS::Idle);
    assert_eq!(
        sim.tick(1_500, IS::Failed).map(|f| f.class),
        Some(PushClass::Error)
    );
    sim.hold(IS::Failed, 1_600, 30_000, 100);
    assert_eq!(sim.classes(), vec![PushClass::Error]);
}

#[test]
fn an_idle_agent_asking_after_its_done_pushed_stays_silent() {
    // Outside a window the arming model holds: no work in between, no edge.
    let mut sim = Sim::new();
    sim.tick(0, IS::Working);
    sim.hold(IS::Idle, 1_000, 7_000, 100); // done pushed at 6_000
    sim.hold(IS::WaitingForInput, 7_100, 20_000, 100);
    sim.hold(IS::Failed, 20_100, 30_000, 100);
    assert_eq!(sim.classes(), vec![PushClass::Done]);
}

#[test]
fn no_push_for_any_other_status() {
    let others = [
        IS::Starting,
        IS::Running,
        IS::Working,
        IS::Stopped,
        IS::SessionLost,
        IS::Recovered,
        IS::Unknown,
    ];
    // Cycling through every non-settled status, in any order, never pushes.
    let mut sim = Sim::new();
    let mut t = 0;
    for _ in 0..3 {
        for s in others {
            sim.hold(s, t, t + 10_000, 500);
            t += 10_500;
        }
    }
    assert!(sim.fired.is_empty(), "{:?}", sim.fired);

    // A settle with no work before it (a fresh or restored session) is quiet.
    for settled in [
        IS::Idle,
        IS::Completed,
        IS::WaitingForInput,
        IS::NeedsAttention,
        IS::Failed,
    ] {
        let mut sim = Sim::new();
        sim.hold(settled, 0, 30_000, 500);
        assert!(sim.fired.is_empty(), "{settled:?}: {:?}", sim.fired);
    }
}

#[test]
fn leaving_for_a_neutral_status_cancels_a_pending_done() {
    for neutral in [IS::Stopped, IS::SessionLost, IS::Recovered, IS::Unknown] {
        let mut sim = Sim::new();
        sim.tick(0, IS::Working);
        sim.tick(1_000, IS::Idle);
        sim.tick(2_000, neutral);
        sim.hold(IS::Idle, 3_000, 30_000, 500);
        assert!(sim.fired.is_empty(), "{neutral:?}: {:?}", sim.fired);
    }
}

#[test]
fn zero_debounce_pushes_done_on_the_settle_tick() {
    let mut sim = Sim::new();
    sim.debounce_ms = 0;
    sim.tick(0, IS::Working);
    let fire = sim.tick(1_000, IS::Idle).expect("no window");
    assert_eq!(fire.class, PushClass::Done);
    assert_eq!(fire.event_id, Some(EventId::new("e1")));
    sim.hold(IS::Idle, 1_100, 10_000, 100);
    assert_eq!(sim.classes(), vec![PushClass::Done]);
}

#[test]
fn a_settle_inside_the_startup_grace_window_never_pushes() {
    let mut sim = Sim::new();
    sim.grace_until_ms = 10_000;
    // A resumed agent settling at launch: tracked, never pushed, even once the
    // window it would have opened ends after the grace period.
    sim.tick(1_000, IS::Working);
    sim.hold(IS::Idle, 2_000, 30_000, 500);
    assert!(sim.fired.is_empty(), "{:?}", sim.fired);
    // A needs-input edge inside grace is suppressed too.
    let mut sim = Sim::new();
    sim.grace_until_ms = 10_000;
    sim.tick(1_000, IS::Working);
    sim.hold(IS::WaitingForInput, 2_000, 30_000, 500);
    assert!(sim.fired.is_empty(), "{:?}", sim.fired);
    // A turn that settles after grace pushes normally.
    let mut sim = Sim::new();
    sim.grace_until_ms = 10_000;
    sim.tick(1_000, IS::Working);
    sim.hold(IS::Working, 2_000, 11_000, 500);
    sim.hold(IS::Idle, 11_500, 30_000, 500);
    assert_eq!(sim.classes(), vec![PushClass::Done]);
    assert_eq!(sim.fired[0].0, 16_500);
}

#[test]
fn a_session_removed_while_its_done_is_pending_never_pushes() {
    let mut sim = Sim::new();
    sim.tick(0, IS::Working);
    sim.tick(1_000, IS::Idle);
    assert!(sim.push.has_pending_done(&sim.sid));
    // The tab is closed: the bridge stops observing it and prunes it.
    sim.push.retain_sessions(|_| false);
    assert!(!sim.push.has_pending_done(&sim.sid));
    // Even a recycled id observed idle afterwards inherits nothing.
    assert_eq!(
        sim.push
            .observe(&sim.sid, IS::Idle, None, 60_000, DEBOUNCE_MS),
        None
    );
}

#[test]
fn retain_keeps_live_sessions_pending() {
    let mut push = PushDecider::new();
    let a = SessionId::new("a");
    let b = SessionId::new("b");
    for sid in [&a, &b] {
        let edge = SettleEdge {
            class: EventClass::Finished,
            event_id: EventId::new(format!("ev-{}", sid.as_str())),
        };
        assert_eq!(push.observe(sid, IS::Idle, Some(edge), 0, 5_000), None);
    }
    push.retain_sessions(|sid| sid == &a);
    assert!(push.has_pending_done(&a));
    assert!(!push.has_pending_done(&b));
    let fire = push.observe(&a, IS::Idle, None, 5_000, 5_000).unwrap();
    assert_eq!(fire.event_id, Some(EventId::new("ev-a")));
    assert_eq!(push.observe(&b, IS::Idle, None, 5_000, 5_000), None);
}

#[test]
fn sessions_are_debounced_independently() {
    let mut push = PushDecider::new();
    let a = SessionId::new("a");
    let b = SessionId::new("b");
    let edge = |id: &str| {
        Some(SettleEdge {
            class: EventClass::Finished,
            event_id: EventId::new(id),
        })
    };
    push.observe(&a, IS::Idle, edge("ea"), 0, 5_000);
    push.observe(&b, IS::Idle, edge("eb"), 2_000, 5_000);
    // b going back to work does not touch a's window.
    push.observe(&b, IS::Working, None, 3_000, 5_000);
    assert_eq!(
        push.observe(&a, IS::Idle, None, 5_000, 5_000)
            .map(|f| f.class),
        Some(PushClass::Done)
    );
    assert_eq!(push.observe(&b, IS::Idle, None, 9_000, 5_000), None);
}

// --- push intent + truncation (contract C1) --------------------------------

#[test]
fn push_truncation_cuts_multibyte_text_on_a_char_boundary() {
    // The protocol's truncate_push_text, as the desktop uses it for push fields.
    // 2-byte (é), 3-byte (日), 4-byte (🚀) characters, and a mix that puts the
    // cut point mid-character for every width.
    for unit in ["é", "日", "🚀", "aé日🚀"] {
        let long = unit.repeat(1_000); // ≥ 2000 bytes: over every limit below
        for max in [120, 1000, 121, 122, 123, 999] {
            let out = truncate_push_text(&long, max);
            assert!(out.len() <= max, "{unit:?} max {max}: {} bytes", out.len());
            assert!(out.ends_with('…'), "{unit:?} max {max}");
            // The prefix is an intact prefix of the input: no split character.
            let prefix = out.strip_suffix('…').unwrap();
            assert!(long.starts_with(prefix), "{unit:?} max {max}");
            // And it used the budget: at most one character's worth is lost.
            assert!(
                max - out.len() < 4,
                "{unit:?} max {max}: {} bytes",
                out.len()
            );
        }
    }
}

fn push_ctx(name: &str, preview: Option<&str>, files_changed: u32) -> EventContext {
    EventContext {
        event_id: EventId::new("ev-push"),
        deep_link: DeepLink {
            project_id: ProjectId::new("proj"),
            session_id: SessionId::new("sess"),
            item_id: None,
        },
        occurred_at_ms: 42,
        session_name: name.to_string(),
        preview: preview.map(str::to_string),
        files_changed,
        ready_to_push: false,
        error_message: None,
    }
}

#[test]
fn push_intent_reuses_the_feed_titles_and_previews() {
    let ni = build_push_intent(
        PushClass::NeedsInput,
        push_ctx("fd-api", Some("Allow `rm -rf build`?"), 0),
        Some("FlightDeck".to_string()),
    );
    assert_eq!(ni.class, PushClass::NeedsInput);
    assert_eq!(ni.title, "fd-api needs input");
    assert_eq!(ni.body, "Allow `rm -rf build`?");
    assert_eq!(ni.event_id, EventId::new("ev-push"));
    assert_eq!(ni.deep_link.project_id.as_str(), "proj");
    assert_eq!(ni.deep_link.session_id.as_str(), "sess");
    assert_eq!(ni.project_name.as_deref(), Some("FlightDeck"));
    assert_eq!(ni.occurred_at_ms, 42);

    let err = build_push_intent(PushClass::Error, push_ctx("fd-api", None, 0), None);
    assert_eq!(err.title, "fd-api hit an error");
    assert_eq!(err.body, "the agent hit an error");

    let done = build_push_intent(PushClass::Done, push_ctx("fd-api", None, 3), None);
    assert_eq!(done.title, "fd-api finished its turn");
    assert_eq!(done.body, "3 files changed");
    let one = build_push_intent(PushClass::Done, push_ctx("x", None, 1), None);
    assert_eq!(one.body, "1 file changed");
    let none = build_push_intent(PushClass::Done, push_ctx("x", None, 0), None);
    assert_eq!(none.body, "");
    let mut ready = push_ctx("x", None, 0);
    ready.ready_to_push = true;
    assert_eq!(
        build_push_intent(PushClass::Done, ready, None).body,
        "Ready to push"
    );

    // The needs-input push and the feed event agree on the title.
    let ev = build_event(EventClass::NeedsInput, push_ctx("fd-api", None, 0));
    assert_eq!(ev.title, ni.title);
}

#[test]
fn push_intent_truncates_title_and_body_to_the_contract_limits() {
    let name = "日本語のセッション".repeat(20); // far over 120 bytes
    let preview = "🚀 deploy? ".repeat(200); // far over 1000 bytes
    let intent = build_push_intent(
        PushClass::NeedsInput,
        push_ctx(&name, Some(&preview), 0),
        None,
    );
    assert!(intent.title.len() <= MAX_PUSH_TITLE_BYTES);
    assert!(intent.title.ends_with('…'));
    assert!(intent.body.len() <= MAX_PUSH_BODY_BYTES);
    assert!(intent.body.ends_with('…'));
    assert!(preview.starts_with(intent.body.strip_suffix('…').unwrap()));
}

#[test]
fn new_event_ids_are_unique_v4_uuids() {
    let a = new_event_id();
    let b = new_event_id();
    assert_ne!(a, b);
    let s = a.as_str();
    assert_eq!(s.len(), 36);
    let parts: Vec<&str> = s.split('-').collect();
    assert_eq!(
        parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
        vec![8, 4, 4, 4, 12]
    );
    assert!(s
        .chars()
        .all(|c| c == '-' || (c.is_ascii_hexdigit() && !c.is_ascii_uppercase())));
    assert_eq!(&s[14..15], "4", "version nibble");
    assert!(
        matches!(&s[19..20], "8" | "9" | "a" | "b"),
        "variant nibble"
    );
}
