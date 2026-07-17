//
//  ActivityFixtures.swift
//  FlightDeckRemote
//
//  Canned `Wire.AgentEvent`s for the `-uitest-fixture-activity` DEBUG seam
//  (`ActivityStore.makeDefault`). Deep-links reuse `Wire.StateSnapshot`'s
//  `-uitest-fixture-snapshot` session/project ids (`Features/Monitor/DebugFixtures.swift`)
//  so a UI test launched with both fixtures can tap a cell and land on a real
//  chat screen; one event deliberately points at a session absent from that
//  snapshot to exercise the "session no longer active" dead-link path.
//
//  DEBUG-only: never compiled into Release.
//

#if DEBUG
import Foundation

enum ActivityFixtures {
    private static let base: Int64 = 1_752_400_500_000

    static func events() -> [Wire.AgentEvent] {
        [needsInputEvent, finishedEvent, errorEvent, deadSessionEvent]
    }

    static var needsInputEvent: Wire.AgentEvent {
        Wire.AgentEvent(
            eventId: Wire.EventId("fx-evt-needs-input"),
            kind: .needsInput(preview: "Should I run `terraform apply` to provision the staging bucket, or hold until you review the plan?"),
            deepLink: Wire.DeepLink(
                projectId: Wire.ProjectId(Wire.StateSnapshot.FixtureIds.flightdeck),
                sessionId: Wire.SessionId("sess_fix_login"),
                itemId: nil),
            occurredAtMs: base,
            title: "fix-login needs your input")
    }

    static var finishedEvent: Wire.AgentEvent {
        Wire.AgentEvent(
            eventId: Wire.EventId("fx-evt-finished"),
            kind: .finished(summary: "Added StatusDot, WorkingSpinner, StatusPill components.", filesChanged: 4, readyToPush: true),
            deepLink: Wire.DeepLink(
                projectId: Wire.ProjectId(Wire.StateSnapshot.FixtureIds.remoteControl),
                sessionId: Wire.SessionId("sess_relay_handshake"),
                itemId: nil),
            occurredAtMs: base - 5 * 60_000,
            title: "relay-handshake finished its turn")
    }

    static var errorEvent: Wire.AgentEvent {
        Wire.AgentEvent(
            eventId: Wire.EventId("fx-evt-error"),
            kind: .error(message: "`npm test` failed: 3 snapshot mismatches in Hero.test.tsx."),
            deepLink: Wire.DeepLink(
                projectId: Wire.ProjectId(Wire.StateSnapshot.FixtureIds.marketingSite),
                sessionId: Wire.SessionId("sess_hero_copy"),
                itemId: nil),
            occurredAtMs: base - 20 * 60_000,
            title: "hero-copy hit an error")
    }

    /// Points at a session id absent from `Wire.StateSnapshot.uiTestFixture`
    /// — exercises the "session no longer active" note when tapped.
    static var deadSessionEvent: Wire.AgentEvent {
        Wire.AgentEvent(
            eventId: Wire.EventId("fx-evt-dead-session"),
            kind: .finished(summary: "Cleaned up the old spike branch.", filesChanged: 1, readyToPush: false),
            deepLink: Wire.DeepLink(
                projectId: Wire.ProjectId(Wire.StateSnapshot.FixtureIds.flightdeck),
                sessionId: Wire.SessionId("sess_long_gone"),
                itemId: nil),
            occurredAtMs: base - 60 * 60_000,
            title: "spike-cleanup finished its turn")
    }
}
#endif
