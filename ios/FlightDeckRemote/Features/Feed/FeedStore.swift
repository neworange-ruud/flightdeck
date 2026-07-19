//
//  FeedStore.swift
//  FlightDeckRemote
//
//  The aggregation layer for the unified multi-pairing feed
//  (remote-control-b8d.6). It folds the per-instance `TransportStore`s owned by
//  the `TransportCoordinator` (b8d.5) into ONE flat, interleaved-by-recency list
//  of `FeedItem`s — one item per project per machine — each tagged with its
//  `pairingId`, a resolved machine display name (from `PairingStore`, override >
//  desktop > fallback), and an online/offline flag.
//
//  This is the aggregated INDEX only. Per-session/chat/monitor detail state
//  stays per-instance and is resolved by `pairingId` via the coordinator's
//  `store(for:)` (remote-control-b8d.12). The unified feed UI (b8d.8) renders
//  `items`; detail views (b8d.12) navigate using each item's `pairingId`.
//
//  Reactivity: `items` is a computed property that reads observable state —
//  `coordinator.handles`, each handle's `TransportStore.snapshot` /
//  `.agentEvents` / `.linkState`, and `PairingStore.instances`. Because
//  Observation registers every one of those reads while a SwiftUI view (or a
//  `withObservationTracking` scope) evaluates `items`, the feed live-updates as
//  any instance syncs, as a machine connects/disconnects, and as a machine is
//  paired/unpaired or renamed — with no manual wiring.
//
//  Offline cache / cold start: each `TransportStore` already persists its own
//  last-known snapshot per `pairingId` via `SnapshotCache`, and the coordinator
//  seeds it back into the store (before that client ever connects) in
//  `makeHandle`. So a machine that is offline at cold start still has a
//  populated `snapshot`; the feed renders its projects as usual, flagged
//  `isOnline == false` so the UI can dim them and show an "offline" badge.
//
//  Offline determination: a machine is ONLINE iff its client's relay link is
//  `.connected` (authenticated and live). Any other link state
//  (`.disconnected` / `.connecting` / `.authenticating`) reads as offline — the
//  items shown for it are then its last-known (cached or pre-drop) state. As a
//  side effect the store mirrors that live online/offline into each
//  `PairedInstance.lastKnownOnline` (via `PairingStore.setLastKnownOnline`), so
//  the persisted metadata other consumers read stays fresh.
//

import Foundation
import Observation

/// One row in the unified feed: a single project on a single paired machine.
/// Carries everything the feed UI (remote-control-b8d.8) needs to render the
/// row and everything the detail views (remote-control-b8d.12) need to navigate
/// into the owning instance.
struct FeedItem: Identifiable, Equatable, Sendable {
    /// The pairing this item came from — detail views resolve their per-instance
    /// `TransportStore` with `coordinator.store(for: pairingId)`.
    let pairingId: String
    /// The resolved machine chip label: user override > desktop-reported name >
    /// generic fallback (see `PairedInstance.displayName`).
    let displayName: String
    /// Whether the owning machine's relay link is currently live. `false` →
    /// the item is last-known state; the UI dims it and shows an "offline" badge.
    let isOnline: Bool
    /// The project to render (name, roll-up, sessions).
    let project: Wire.ProjectState
    /// Recency key used to interleave across machines (unix ms): the most recent
    /// agent-event time for this project, or the machine's snapshot time when the
    /// project has no events. Higher = more recent.
    let activityMs: Int64

    /// Stable identity across machines: a machine's `pairingId` plus the
    /// project id (distinct machines can host distinctly-ided projects, but the
    /// join keeps ids unique even if two machines reused a project id).
    var id: String { pairingId + "\u{1f}" + project.projectId.rawValue }

    /// Convenience: the underlying project id.
    var projectId: Wire.ProjectId { project.projectId }

    /// Convenience negation for the dimmed/offline-badge UI.
    var isOffline: Bool { !isOnline }
}

@MainActor
@Observable
final class FeedStore {

    // MARK: - Dependencies

    private let coordinator: TransportCoordinator
    private let pairingStore: PairingStore

    init(coordinator: TransportCoordinator, pairingStore: PairingStore) {
        self.coordinator = coordinator
        self.pairingStore = pairingStore
        // Prime the persisted online flags from the current link states, then
        // keep them in sync as links change.
        refreshOnlineState()
        armOnlineObservation()
    }

    // MARK: - Aggregated feed (downstream API: b8d.8 UI, b8d.12 detail nav)

    /// The interleaved-by-recency feed across every live instance: one item per
    /// project per machine, newest activity first. Reads live observable state,
    /// so SwiftUI views bound to it re-render whenever any instance syncs, a
    /// machine connects/disconnects, or the pairing set / names change.
    var items: [FeedItem] {
        let sources = coordinator.handles.map { handle in
            Source(
                pairingId: handle.pairingId,
                displayName: displayName(for: handle.pairingId, fallback: handle.instance),
                isOnline: Self.isOnline(handle.store.linkState),
                snapshot: handle.store.snapshot,
                events: handle.store.agentEvents
            )
        }
        return Self.buildItems(from: sources)
    }

    // MARK: - Online-state write-back

    /// Mirror each instance's live link state into its persisted
    /// `PairedInstance.lastKnownOnline`. Called at init, whenever a link state
    /// changes (see `armOnlineObservation`), and available to call directly.
    func refreshOnlineState() {
        for handle in coordinator.handles {
            pairingStore.setLastKnownOnline(
                pairingId: handle.pairingId,
                Self.isOnline(handle.store.linkState)
            )
        }
    }

    /// Arm a one-shot observation over every handle's `linkState` (and the
    /// handle set itself). On any change it writes the fresh online flags back
    /// and re-arms — a self-perpetuating reactive loop that also picks up
    /// machines added/removed by `TransportCoordinator.reconcile`.
    private func armOnlineObservation() {
        withObservationTracking {
            for handle in coordinator.handles {
                _ = handle.store.linkState
            }
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.refreshOnlineState()
                self.armOnlineObservation()
            }
        }
    }

    // MARK: - Display-name resolution

    /// Resolve a machine's chip label from `PairingStore` (the source of truth
    /// for override/desktop names), falling back to the coordinator handle's own
    /// copy if the pairing is momentarily absent from the store.
    private func displayName(for pairingId: String, fallback instance: PairedInstance) -> String {
        (pairingStore.instances.first { $0.pairingId == pairingId } ?? instance).displayName
    }

    // MARK: - Pure aggregation core (unit-tested directly)

    /// A snapshot of one instance's inputs to the interleaver. Pure value type so
    /// the ordering/tagging logic can be tested without a live coordinator.
    struct Source {
        let pairingId: String
        let displayName: String
        let isOnline: Bool
        let snapshot: Wire.StateSnapshot?
        let events: [Wire.AgentEvent]
    }

    /// Fold `sources` into the flat, interleaved-by-recency feed. Machines with
    /// no snapshot contribute nothing. Ordering is by `activityMs` descending,
    /// with a stable tie-break that preserves source (machine) order and then
    /// each snapshot's project order — deterministic regardless of the platform
    /// sort's stability guarantees.
    static func buildItems(from sources: [Source]) -> [FeedItem] {
        var items: [FeedItem] = []
        for source in sources {
            guard let snapshot = source.snapshot else { continue }
            for project in snapshot.projects {
                items.append(FeedItem(
                    pairingId: source.pairingId,
                    displayName: source.displayName,
                    isOnline: source.isOnline,
                    project: project,
                    activityMs: activityMs(
                        projectId: project.projectId,
                        events: source.events,
                        serverTimeMs: snapshot.serverTimeMs
                    )
                ))
            }
        }
        return items.enumerated()
            .sorted { lhs, rhs in
                if lhs.element.activityMs != rhs.element.activityMs {
                    return lhs.element.activityMs > rhs.element.activityMs
                }
                return lhs.offset < rhs.offset // stable tie-break
            }
            .map(\.element)
    }

    /// The most-recent activity time for a project: the newest agent-event time
    /// among events that deep-link into it, or the machine's snapshot time when
    /// the project has produced no events.
    static func activityMs(
        projectId: Wire.ProjectId,
        events: [Wire.AgentEvent],
        serverTimeMs: Int64
    ) -> Int64 {
        events
            .filter { $0.deepLink.projectId == projectId }
            .map(\.occurredAtMs)
            .max() ?? serverTimeMs
    }

    /// Online iff the relay link is authenticated and live; every other state
    /// (disconnected / connecting / authenticating) reads as offline.
    static func isOnline(_ linkState: RemoteLinkState) -> Bool {
        if case .connected = linkState { return true }
        return false
    }
}
