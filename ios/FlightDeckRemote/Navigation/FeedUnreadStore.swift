//
//  FeedUnreadStore.swift
//  FlightDeckRemote
//
//  The unified Feed's unread source (remote-control-fa8) — the multi-machine
//  successor to the (removed) Activity tab's `ActivityStore`. It tracks, per
//  feed item (key = `pairingId` + `projectId`, i.e. `FeedItem.id`):
//   - the newest `AgentEvent` time SEEN for that item, aggregated across EVERY
//     paired machine's `TransportStore.agentEvents` (not just the primary — the
//     Activity tab was single-machine; this is not); and
//   - a "last seen" watermark advanced when the user opens that row.
//  A row is UNREAD when its newest-seen event time exceeds its watermark.
//
//  Both maps are persisted to disk (`FeedUnreadFileStore`, reusing the durable
//  JSON-in-Application-Support shape the Activity feed used) so unread state
//  survives app restarts even though a `TransportStore`'s in-memory
//  `agentEvents` do not: a row whose event arrived in a previous session still
//  shows unread on next launch (its cached snapshot re-renders the row, and the
//  persisted event time still outranks the watermark).
//
//  Ingestion is armed reactively over the live coordinator (see
//  `armIngestion(coordinator:)`) — the same self-perpetuating
//  `withObservationTracking` shape `FeedStore.armOnlineObservation` and
//  `TransportCoordinator.armMachineNameObservation` use — so newly-synced
//  events on ANY machine fold in without per-machine `.onChange` wiring, and
//  the initial fold is deferred to a main-actor tick to avoid mutating this
//  @Observable store during SwiftUI view construction (a reentrancy hang, per
//  the same hazard documented on `FeedStore.init`).
//

import Foundation
import Observation

/// The durable payload a `FeedUnreadPersisting` implementation stores: the
/// newest-seen event time per item and the per-item last-seen watermark.
struct FeedUnreadPersistedState: Codable, Equatable {
    /// itemKey (`FeedItem.id`) → newest `AgentEvent.occurredAtMs` ever seen.
    var latestEventMs: [String: Int64]
    /// itemKey (`FeedItem.id`) → last-seen watermark.
    var watermarks: [String: Int64]

    init(latestEventMs: [String: Int64] = [:], watermarks: [String: Int64] = [:]) {
        self.latestEventMs = latestEventMs
        self.watermarks = watermarks
    }
}

/// Where a `FeedUnreadStore` durably keeps its maps. A small seam (mirrors the
/// Activity feed's `ActivityEventPersisting`) so tests can inject an in-memory
/// double instead of touching real disk.
protocol FeedUnreadPersisting {
    func load() -> FeedUnreadPersistedState
    func save(_ state: FeedUnreadPersistedState)
}

@MainActor
@Observable
final class FeedUnreadStore {

    /// itemKey → newest event time seen. `private(set)` — mutated only through
    /// `ingest`/`markRead`.
    private(set) var latestEventMs: [String: Int64] = [:]
    /// itemKey → last-seen watermark.
    private(set) var watermarks: [String: Int64] = [:]

    private let persistence: FeedUnreadPersisting

    init(persistence: FeedUnreadPersisting = FeedUnreadFileStore()) {
        self.persistence = persistence
        let loaded = persistence.load()
        latestEventMs = loaded.latestEventMs
        watermarks = loaded.watermarks
    }

    /// The per-item key for a `(pairingId, projectId)` pair — identical to
    /// `FeedItem.id`, so callers can pass `item.id` directly.
    static func itemKey(pairingId: String, projectId: Wire.ProjectId) -> String {
        pairingId + "\u{1f}" + projectId.rawValue
    }

    // MARK: - Ingestion (all machines, aggregate)

    /// Fold one machine's `agentEvents` in, tagging each by `pairingId` so the
    /// item key matches `FeedItem.id`. Idempotent — records only the NEWEST
    /// time per item, so re-ingesting the same stream (or an event seen before)
    /// changes nothing. Persists only when something actually advanced.
    func ingest(pairingId: String, events: [Wire.AgentEvent]) {
        var changed = false
        for event in events {
            let key = Self.itemKey(pairingId: pairingId, projectId: event.deepLink.projectId)
            if event.occurredAtMs > (latestEventMs[key] ?? .min) {
                latestEventMs[key] = event.occurredAtMs
                changed = true
            }
        }
        if changed { persist() }
    }

    /// Ingest every live handle's events in one pass (initial fold + re-fold on
    /// any change, driven by `armIngestion`).
    func ingestAll(coordinator: TransportCoordinator) {
        for handle in coordinator.handles {
            ingest(pairingId: handle.pairingId, events: handle.store.agentEvents)
        }
    }

    /// Arm a one-shot observation over every handle's `agentEvents` (and the
    /// handle set itself, so machines added/removed by `reconcile` are picked
    /// up). On any change it re-folds and re-arms. Reading observable state to
    /// register tracking is safe; the WRITE (`ingestAll`) happens on a deferred
    /// main-actor tick so this never mutates the store during SwiftUI view
    /// construction (reentrancy hang — see the file/`FeedStore.init` notes).
    func armIngestion(coordinator: TransportCoordinator) {
        withObservationTracking {
            for handle in coordinator.handles {
                _ = handle.store.agentEvents
            }
        } onChange: { [weak self, weak coordinator] in
            Task { @MainActor [weak self, weak coordinator] in
                guard let self, let coordinator else { return }
                self.ingestAll(coordinator: coordinator)
                self.armIngestion(coordinator: coordinator)
            }
        }
    }

    // MARK: - Unread queries + read tracking

    /// Whether the item keyed `itemKey` (== `FeedItem.id`) is unread: its
    /// newest-seen event time exceeds its watermark. An item with no event
    /// seen is never unread.
    func isUnread(itemKey: String) -> Bool {
        guard let latest = latestEventMs[itemKey] else { return false }
        return latest > (watermarks[itemKey] ?? 0)
    }

    /// Count of unread rows among `items` — the Feed tab's unread badge.
    func unreadCount(items: [FeedItem]) -> Int {
        items.reduce(0) { $0 + (isUnread(itemKey: $1.id) ? 1 : 0) }
    }

    /// Mark one item read (advance its watermark to its newest-seen event
    /// time), persisting. No-op if already caught up.
    func markRead(itemKey: String) {
        let latest = latestEventMs[itemKey] ?? 0
        guard latest > (watermarks[itemKey] ?? 0) else { return }
        watermarks[itemKey] = latest
        persist()
    }

    private func persist() {
        persistence.save(FeedUnreadPersistedState(latestEventMs: latestEventMs, watermarks: watermarks))
    }
}

extension FeedUnreadStore {
    /// The store `MainTabView` uses. Any `-uitest*` launch gets an in-memory
    /// (hermetic, always-clean) backing — never the real on-disk file — so
    /// unread watermarks can't leak across simulator runs or test scenarios
    /// (mirrors `-uitest-reset-pairing`'s hermeticity goal and
    /// `TransportStoreFactory`'s uitest disk skip). Under the
    /// `-uitest-fixture-activity` seam it is additionally seeded with the
    /// fixture events so the Feed shows unread rows + the badge deterministically.
    static func makeDefault(arguments: [String] = ProcessInfo.processInfo.arguments) -> FeedUnreadStore {
        #if DEBUG
        let isUITestLaunch = arguments.contains { $0.hasPrefix("-uitest") }
        if isUITestLaunch {
            let store = FeedUnreadStore(persistence: InMemoryFeedUnreadPersisting())
            if arguments.contains("-uitest-fixture-activity") {
                store.ingest(pairingId: FeedStore.Fixture.pairingId, events: ActivityFixtures.events())
            }
            return store
        }
        #endif
        return FeedUnreadStore()
    }
}

#if DEBUG
/// In-memory `FeedUnreadPersisting` used for `-uitest*` launches, so fixture /
/// toggle-paired runs never read or write the real on-disk unread state.
final class InMemoryFeedUnreadPersisting: FeedUnreadPersisting {
    private var state = FeedUnreadPersistedState()
    func load() -> FeedUnreadPersistedState { state }
    func save(_ state: FeedUnreadPersistedState) { self.state = state }
}
#endif
