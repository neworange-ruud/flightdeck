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
