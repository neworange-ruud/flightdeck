//
//  ActivityStoreTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the Activity tab's real data source (PRD §5.7): persisted event
//  round-trip + cap, the unread watermark (bumped only while the tab isn't
//  selected, advanced immediately while it is), and `markViewed` clearing +
//  persisting the watermark.
//

import Testing
@testable import FlightDeckRemote

/// In-memory `ActivityEventPersisting` double so tests never touch real disk.
private final class FakeActivityEventPersisting: ActivityEventPersisting {
    var state: ActivityPersistedState
    private(set) var saveCount = 0

    init(state: ActivityPersistedState = ActivityPersistedState()) {
        self.state = state
    }

    func load() -> ActivityPersistedState { state }
    func save(_ state: ActivityPersistedState) {
        self.state = state
        saveCount += 1
    }
}

@MainActor
@Suite struct ActivityStoreTests {

    private func event(_ id: String, atMs: Int64, kind: Wire.EventKind = .finished(summary: "done", filesChanged: 1, readyToPush: false)) -> Wire.AgentEvent {
        Wire.AgentEvent(
            eventId: Wire.EventId(id),
            kind: kind,
            deepLink: Wire.DeepLink(projectId: Wire.ProjectId("p1"), sessionId: Wire.SessionId("s1"), itemId: nil),
            occurredAtMs: atMs,
            title: "title-\(id)")
    }

    @Test func defaultsToNoUnreadEventsWithoutAnyPersistedData() {
        let store = ActivityStore(persistence: FakeActivityEventPersisting())
        #expect(store.unreadCount == 0)
        #expect(store.events.isEmpty)
    }

    @Test func loadsPersistedEventsAndComputesUnreadAgainstTheWatermark() {
        let persisted = ActivityPersistedState(
            events: [event("e1", atMs: 100), event("e2", atMs: 200)],
            lastSeenAtMs: 100)
        let store = ActivityStore(persistence: FakeActivityEventPersisting(state: persisted))

        // e2 (200) is newer than the watermark (100); e1 (100) is not.
        #expect(store.unreadCount == 1)
        #expect(store.events.map(\.eventId.rawValue) == ["e2", "e1"]) // newest first
    }

    @Test func ingestWhileTabNotSelectedBumpsUnreadCount() {
        let persistence = FakeActivityEventPersisting()
        let store = ActivityStore(persistence: persistence)

        store.ingest([event("e1", atMs: 100)], tabSelected: false)
        #expect(store.unreadCount == 1)

        store.ingest([event("e2", atMs: 200)], tabSelected: false)
        #expect(store.unreadCount == 2)
        #expect(store.events.map(\.eventId.rawValue) == ["e2", "e1"])
        #expect(persistence.saveCount == 2)
    }

    @Test func ingestWhileTabSelectedAdvancesWatermarkInsteadOfUnread() {
        let store = ActivityStore(persistence: FakeActivityEventPersisting())

        store.ingest([event("e1", atMs: 100)], tabSelected: true)
        #expect(store.unreadCount == 0)
        #expect(store.events.count == 1)

        // A later relaunch (fresh store over the same persisted state)
        // should not resurrect e1 as unread, since the watermark advanced.
        let reloaded = ActivityStore(persistence: FakeActivityEventPersisting(
            state: ActivityPersistedState(events: store.events, lastSeenAtMs: 100)))
        #expect(reloaded.unreadCount == 0)
    }

    @Test func ingestDedupesByEventId() {
        let store = ActivityStore(persistence: FakeActivityEventPersisting())
        let e1 = event("e1", atMs: 100)

        store.ingest([e1], tabSelected: false)
        store.ingest([e1], tabSelected: false) // same event id, arrives again
        #expect(store.events.count == 1)
        #expect(store.unreadCount == 1)
    }

    @Test func eventsCapAtTwoHundredKeepingTheNewest() {
        let store = ActivityStore(persistence: FakeActivityEventPersisting())
        let events = (0..<250).map { event("e\($0)", atMs: Int64($0)) }

        store.ingest(events, tabSelected: false)

        #expect(store.events.count == ActivityStore.cap)
        // Newest-first: the top of the feed is the highest-timestamp event.
        #expect(store.events.first?.eventId.rawValue == "e249")
        #expect(store.events.last?.eventId.rawValue == "e50")
    }

    @Test func markViewedClearsUnreadAndAdvancesWatermark() {
        let persistence = FakeActivityEventPersisting()
        let store = ActivityStore(persistence: persistence)
        store.ingest([event("e1", atMs: 100), event("e2", atMs: 200)], tabSelected: false)
        #expect(store.unreadCount == 2)

        store.markViewed()
        #expect(store.unreadCount == 0)

        // A fresh store over the persisted watermark shouldn't re-surface
        // those same events as unread.
        let reloaded = ActivityStore(persistence: persistence)
        #expect(reloaded.unreadCount == 0)
    }

    @Test func persistedRoundTripSurvivesAFreshStoreInstance() {
        let persistence = FakeActivityEventPersisting()
        let store = ActivityStore(persistence: persistence)
        store.ingest([event("e1", atMs: 100)], tabSelected: false)

        let reloaded = ActivityStore(persistence: persistence)
        #expect(reloaded.events.map(\.eventId.rawValue) == ["e1"])
        #expect(reloaded.unreadCount == 1)
    }
}
