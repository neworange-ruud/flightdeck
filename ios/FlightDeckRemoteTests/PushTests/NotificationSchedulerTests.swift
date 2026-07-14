//
//  NotificationSchedulerTests.swift
//  FlightDeckRemoteTests
//
//  Local-notification scheduling: dedup by `event_id` (PRD §5.8 — a
//  queued-then-replayed event must not double-fire) and settings/mute gating.
//

import Testing
@testable import FlightDeckRemote

@MainActor
struct NotificationSchedulerTests {

    @Test func schedulesOneRequestPerEvent() {
        let recorder = RecordingNotificationScheduler()
        let scheduler = NotificationScheduler(scheduler: recorder)
        scheduler.ingest([PushFixtures.needsInput, PushFixtures.finished], settings: NotificationSettings())
        #expect(recorder.identifiers == ["evt_needs", "evt_done"])
    }

    @Test func deduplicatesRepeatedEventIds() {
        let recorder = RecordingNotificationScheduler()
        let scheduler = NotificationScheduler(scheduler: recorder)
        // A replay re-delivers the same event; it must fire exactly once.
        scheduler.ingest([PushFixtures.needsInput], settings: NotificationSettings())
        scheduler.ingest([PushFixtures.needsInput], settings: NotificationSettings())
        #expect(recorder.identifiers == ["evt_needs"])
    }

    @Test func suppressedEventsAreNotScheduledButStillMarkedSeen() {
        let recorder = RecordingNotificationScheduler()
        let scheduler = NotificationScheduler(scheduler: recorder)
        // Toggled off → nothing scheduled.
        scheduler.ingest([PushFixtures.needsInput], settings: NotificationSettings(agentNeedsInput: false))
        #expect(recorder.requests.isEmpty)
        // Even if the toggle later flips on, the already-seen event must not
        // retro-fire (it counted as seen).
        scheduler.ingest([PushFixtures.needsInput], settings: NotificationSettings(agentNeedsInput: true))
        #expect(recorder.requests.isEmpty)
    }

    @Test func mutedProjectIsNotScheduled() {
        let recorder = RecordingNotificationScheduler()
        let scheduler = NotificationScheduler(scheduler: recorder)
        let settings = NotificationSettings(mutedProjectIds: ["proj_1"])
        scheduler.ingest([PushFixtures.needsInput], settings: settings)
        #expect(recorder.requests.isEmpty)
    }
}
