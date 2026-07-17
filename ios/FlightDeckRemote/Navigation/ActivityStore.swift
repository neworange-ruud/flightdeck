//
//  ActivityStore.swift
//  FlightDeckRemote
//
//  The Activity tab's real data source (PRD §5.7): a durable, deduped feed of
//  `Wire.AgentEvent`s — persisted to disk so the feed survives app restarts,
//  capped at 200 — plus the unread-badge count `CustomTabBar` reads. This
//  replaces the earlier stub (`unreadCount` hardcoded to `1`).
//
//  Live delivery flows in from `TransportStore.agentEvents` — `MainTabView`
//  bridges the two with `.onChange(of: transportStore.agentEvents)` (Transport
//  stays transport-agnostic of the Activity feature and vice versa) — while
//  `ActivityFeedView`/`ActivityFeedModel` read `events` straight off this
//  store to render cells.
//
//  Unread semantics: an event newly seen while the Activity tab is *not*
//  selected bumps `unreadCount`; one seen while it *is* selected instead
//  advances the "last seen" watermark immediately (so leaving and coming
//  back doesn't re-surface it as unread). `markViewed()` — called by
//  `MainTabView` when the tab becomes selected, and by `ActivityFeedView` on
//  appear — clears the count and advances the watermark to the newest known
//  event, persisting both.
//

import Foundation
import Observation

/// The durable payload an `ActivityEventPersisting` implementation stores:
/// the feed itself plus the "last seen" watermark driving the unread count.
struct ActivityPersistedState: Codable, Equatable {
    var events: [Wire.AgentEvent]
    var lastSeenAtMs: Int64

    init(events: [Wire.AgentEvent] = [], lastSeenAtMs: Int64 = 0) {
        self.events = events
        self.lastSeenAtMs = lastSeenAtMs
    }
}

/// Where an `ActivityStore` durably keeps its events + watermark. A small
/// seam (mirrors `AppLockSettingsProviding`'s shape) so tests can inject an
/// in-memory double instead of touching real disk.
protocol ActivityEventPersisting {
    func load() -> ActivityPersistedState
    func save(_ state: ActivityPersistedState)
}

@MainActor
@Observable
final class ActivityStore {
    /// Hard cap on how many events are retained (PRD: "cap 200 events").
    static let cap = 200

    /// The feed, newest first — what `ActivityFeedView` renders directly.
    private(set) var events: [Wire.AgentEvent] = []
    /// Unread badge count (`CustomTabBar` reads this via `MainTabView`).
    private(set) var unreadCount: Int = 0

    private let persistence: ActivityEventPersisting
    private var lastSeenAtMs: Int64 = 0
    private var seenEventIds: Set<Wire.EventId> = []

    init(persistence: ActivityEventPersisting = ActivityEventFileStore()) {
        self.persistence = persistence
        let loaded = persistence.load()

        // Newest first, deduped defensively — a corrupt/hand-edited cache
        // file must never crash or double-count.
        var seen = Set<Wire.EventId>()
        var ordered: [Wire.AgentEvent] = []
        for event in loaded.events.sorted(by: { $0.occurredAtMs > $1.occurredAtMs }) where seen.insert(event.eventId).inserted {
            ordered.append(event)
        }

        self.events = Array(ordered.prefix(Self.cap))
        self.seenEventIds = seen
        self.lastSeenAtMs = loaded.lastSeenAtMs
        self.unreadCount = self.events.filter { $0.occurredAtMs > loaded.lastSeenAtMs }.count
    }

    /// Merges freshly delivered live events (`TransportStore.agentEvents`)
    /// into the durable feed, deduped by `event_id`. New events bump
    /// `unreadCount` unless the Activity tab is currently selected, in which
    /// case they instead advance the watermark immediately (already "seen").
    func ingest(_ liveEvents: [Wire.AgentEvent], tabSelected: Bool) {
        var newOnes: [Wire.AgentEvent] = []
        for event in liveEvents where seenEventIds.insert(event.eventId).inserted {
            newOnes.append(event)
        }
        guard !newOnes.isEmpty else { return }

        events.append(contentsOf: newOnes)
        events.sort { $0.occurredAtMs > $1.occurredAtMs }
        if events.count > Self.cap {
            events.removeLast(events.count - Self.cap)
        }

        if tabSelected {
            lastSeenAtMs = max(lastSeenAtMs, newOnes.map(\.occurredAtMs).max() ?? lastSeenAtMs)
        } else {
            unreadCount += newOnes.count
        }
        persist()
    }

    /// Clears the unread badge and advances the watermark to the newest known
    /// event (PRD §5.7: "unread dot clears on view"). Called once the
    /// Activity tab is actually viewed, not merely when new events arrive.
    func markViewed() {
        guard unreadCount != 0 || lastSeenAtMs < (events.map(\.occurredAtMs).max() ?? lastSeenAtMs) else { return }
        unreadCount = 0
        lastSeenAtMs = max(lastSeenAtMs, events.map(\.occurredAtMs).max() ?? lastSeenAtMs)
        persist()
    }

    private func persist() {
        persistence.save(ActivityPersistedState(events: events, lastSeenAtMs: lastSeenAtMs))
    }
}

extension ActivityStore {
    /// Builds the store `MainTabView` uses: real disk persistence, unless
    /// launched under the DEBUG `-uitest-fixture-activity` seam, in which
    /// case it seeds a canned, in-memory-only feed instead — never touching
    /// real disk, so UI tests are hermetic regardless of what a previous
    /// simulator run persisted (mirrors `-uitest-fixture-snapshot`'s
    /// `TransportStoreFactory` counterpart).
    static func makeDefault(arguments: [String] = ProcessInfo.processInfo.arguments) -> ActivityStore {
        #if DEBUG
        if arguments.contains("-uitest-fixture-activity") {
            let store = ActivityStore(persistence: InMemoryActivityEventPersisting())
            store.seedFixtureEvents()
            return store
        }
        #endif
        return ActivityStore()
    }

    #if DEBUG
    /// Seeds a realistic mixed feed (needs-input / finished / error, plus one
    /// event pointing at a session absent from `Wire.StateSnapshot.uiTestFixture`
    /// to exercise the "session no longer active" dead-link path) and leaves
    /// them all unread, so `-uitest-fixture-activity` UI tests can exercise
    /// cell rendering, the unread badge, and tap-to-navigate deterministically.
    fileprivate func seedFixtureEvents() {
        let fixtureEvents = ActivityFixtures.events().sorted { $0.occurredAtMs > $1.occurredAtMs }
        seenEventIds = Set(fixtureEvents.map(\.eventId))
        events = fixtureEvents
        unreadCount = events.count
    }
    #endif
}

#if DEBUG
/// In-memory `ActivityEventPersisting` used only by the
/// `-uitest-fixture-activity` seam above, so fixture runs never read/write
/// the real on-disk feed.
final class InMemoryActivityEventPersisting: ActivityEventPersisting {
    private var state = ActivityPersistedState()
    func load() -> ActivityPersistedState { state }
    func save(_ state: ActivityPersistedState) { self.state = state }
}
#endif
