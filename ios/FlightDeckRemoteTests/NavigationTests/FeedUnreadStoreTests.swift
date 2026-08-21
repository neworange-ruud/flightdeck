//
//  FeedUnreadStoreTests.swift
//  FlightDeckRemoteTests
//
//  Covers the unified Feed's unread source (remote-control-fa8): per-item
//  (pairingId+projectId) watermarking fed by MULTIPLE machines' agent events,
//  unread-when-latest-event-outranks-watermark, read-on-open advancing the
//  watermark, the unread badge count over a set of feed items, and the
//  persisted round-trip surviving a fresh store instance (unread persists
//  across launches).
//

import Testing
@testable import FlightDeckRemote

/// In-memory `FeedUnreadPersisting` double so tests never touch real disk.
private final class FakeFeedUnreadPersisting: FeedUnreadPersisting {
    var state: FeedUnreadPersistedState
    private(set) var saveCount = 0

    init(state: FeedUnreadPersistedState = FeedUnreadPersistedState()) {
        self.state = state
    }

    func load() -> FeedUnreadPersistedState { state }
    func save(_ state: FeedUnreadPersistedState) {
        self.state = state
        saveCount += 1
    }
}

@MainActor
@Suite struct FeedUnreadStoreTests {

    private func event(project: String, session: String = "s", atMs: Int64,
                       kind: Wire.EventKind = .finished(summary: "done", filesChanged: 1, readyToPush: false)) -> Wire.AgentEvent {
        Wire.AgentEvent(
            eventId: Wire.EventId("evt_\(project)_\(atMs)"),
            kind: kind,
            deepLink: Wire.DeepLink(projectId: Wire.ProjectId(project), sessionId: Wire.SessionId(session), itemId: nil),
            occurredAtMs: atMs,
            title: "title")
    }

    private func key(_ pairingId: String, _ projectId: String) -> String {
        FeedUnreadStore.itemKey(pairingId: pairingId, projectId: Wire.ProjectId(projectId))
    }

    @Test func aRowWithNoEventIsNeverUnread() {
        let store = FeedUnreadStore(persistence: FakeFeedUnreadPersisting())
        #expect(!store.isUnread(itemKey: key("A", "p1")))
    }

    @Test func ingestedEventNewerThanWatermarkIsUnread() {
        let store = FeedUnreadStore(persistence: FakeFeedUnreadPersisting())
        store.ingest(pairingId: "A", events: [event(project: "p1", atMs: 100)])
        #expect(store.isUnread(itemKey: key("A", "p1")))
    }

    @Test func unreadIsScopedPerMachineAndProject() {
        // Same project id on two machines are distinct items.
        let store = FeedUnreadStore(persistence: FakeFeedUnreadPersisting())
        store.ingest(pairingId: "A", events: [event(project: "p1", atMs: 100)])
        store.ingest(pairingId: "B", events: [event(project: "p2", atMs: 200)])

        #expect(store.isUnread(itemKey: key("A", "p1")))
        #expect(store.isUnread(itemKey: key("B", "p2")))
        #expect(!store.isUnread(itemKey: key("A", "p2")))
        #expect(!store.isUnread(itemKey: key("B", "p1")))
    }

    @Test func ingestTracksOnlyTheNewestTimePerItemAndIsIdempotent() {
        let persistence = FakeFeedUnreadPersisting()
        let store = FeedUnreadStore(persistence: persistence)

        store.ingest(pairingId: "A", events: [event(project: "p1", atMs: 100)])
        // An older event for the same item doesn't lower the recorded time and
        // doesn't re-persist.
        let savesAfterFirst = persistence.saveCount
        store.ingest(pairingId: "A", events: [event(project: "p1", atMs: 50)])
        #expect(persistence.saveCount == savesAfterFirst, "no advance → no re-persist")

        store.ingest(pairingId: "A", events: [event(project: "p1", atMs: 300)])
        #expect(store.latestEventMs[key("A", "p1")] == 300)
    }

    @Test func markReadAdvancesTheWatermarkAndClearsUnread() {
        let store = FeedUnreadStore(persistence: FakeFeedUnreadPersisting())
        let k = key("A", "p1")
        store.ingest(pairingId: "A", events: [event(project: "p1", atMs: 100)])
        #expect(store.isUnread(itemKey: k))

        store.markRead(itemKey: k)
        #expect(!store.isUnread(itemKey: k))
    }

    @Test func aLaterEventAfterReadReopensUnread() {
        let store = FeedUnreadStore(persistence: FakeFeedUnreadPersisting())
        let k = key("A", "p1")
        store.ingest(pairingId: "A", events: [event(project: "p1", atMs: 100)])
        store.markRead(itemKey: k)
        #expect(!store.isUnread(itemKey: k))

        // A brand-new event lands after the row was read → unread again.
        store.ingest(pairingId: "A", events: [event(project: "p1", atMs: 200)])
        #expect(store.isUnread(itemKey: k))
    }

    @Test func unreadCountCountsUnreadFeedItems() {
        let store = FeedUnreadStore(persistence: FakeFeedUnreadPersisting())
        store.ingest(pairingId: "A", events: [
            event(project: "p1", atMs: 100),
            event(project: "p2", atMs: 100),
        ])
        let items = [
            FeedItemFixtures.item(pairingId: "A", projectId: "p1"),
            FeedItemFixtures.item(pairingId: "A", projectId: "p2"),
            FeedItemFixtures.item(pairingId: "A", projectId: "p3"), // no event → read
        ]
        #expect(store.unreadCount(items: items) == 2)

        store.markRead(itemKey: key("A", "p1"))
        #expect(store.unreadCount(items: items) == 1)
    }

    @Test func persistedRoundTripSurvivesAFreshStoreInstance() {
        // Unread persists across launches: the transport's in-memory events are
        // gone on relaunch, but the persisted latest-event map still outranks
        // the watermark, so the row is still unread.
        let persistence = FakeFeedUnreadPersisting()
        let store = FeedUnreadStore(persistence: persistence)
        store.ingest(pairingId: "A", events: [event(project: "p1", atMs: 100)])

        let reloaded = FeedUnreadStore(persistence: persistence)
        #expect(reloaded.isUnread(itemKey: key("A", "p1")))

        // …and read state persists too.
        reloaded.markRead(itemKey: key("A", "p1"))
        let reloadedAgain = FeedUnreadStore(persistence: persistence)
        #expect(!reloadedAgain.isUnread(itemKey: key("A", "p1")))
    }
}
