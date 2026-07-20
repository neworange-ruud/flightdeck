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
    /// The project's most recent `Wire.AgentEvent` (across every machine's
    /// stream that deep-links into it), or `nil` if it has produced none. The
    /// unified Feed (remote-control-fa8) folds Activity's value in here: it
    /// drives the event-derived summary line (needs-input preview / finished
    /// summary / error message), the red error state, the attention ordering,
    /// and the per-event deep-link on tap. `Wire.RollupDot` deliberately has no
    /// finished/error — those live ONLY on `Wire.AgentEvent.EventKind`.
    let latestEvent: Wire.AgentEvent?

    /// Stable identity across machines: a machine's `pairingId` plus the
    /// project id (distinct machines can host distinctly-ided projects, but the
    /// join keeps ids unique even if two machines reused a project id). Also the
    /// per-item key the `FeedUnreadStore` watermarks against.
    var id: String { pairingId + "\u{1f}" + project.projectId.rawValue }

    /// Convenience: the underlying project id.
    var projectId: Wire.ProjectId { project.projectId }

    /// Convenience negation for the dimmed/offline-badge UI.
    var isOffline: Bool { !isOnline }

    /// The latest event is an error (`EventKind.error`) — the row styles red.
    var isErrorEvent: Bool {
        guard let kind = latestEvent?.kind else { return false }
        if case .error = kind { return true }
        return false
    }

    /// The latest event is a needs-input stop (`EventKind.needsInput`).
    var isNeedsInputEvent: Bool {
        guard let kind = latestEvent?.kind else { return false }
        if case .needsInput = kind { return true }
        return false
    }

    /// Whether the row demands attention (sorts above the calm rows): the LIVE
    /// roll-up dot is needs-input, OR the latest event is needs-input/error.
    var needsAttention: Bool {
        project.rollup.dot == .needsInput || isNeedsInputEvent || isErrorEvent
    }

    /// The session a tap should deep-link into when the latest event is a
    /// needs-input/error one — straight to that agent's chat (remote-control-fa8).
    /// `nil` for a calm row (finished/idle) or no event, which opens the
    /// project's sessions list instead.
    var attentionSessionId: Wire.SessionId? {
        (isNeedsInputEvent || isErrorEvent) ? latestEvent?.deepLink.sessionId : nil
    }
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
        // Keep the persisted online flags in sync as links change. Arming only
        // *reads* observable state (to register tracking), which is safe here.
        armOnlineObservation()
        // The initial prime is DEFERRED to the next main-actor tick. FeedStore
        // is constructed inside `MainTabView.init` (b8d.8); mutating the shared
        // @Observable `PairingStore` synchronously during SwiftUI's view-
        // construction pass reentrantly retriggers that update, rebuilding
        // `MainTabView` (and a fresh `TransportCoordinator` + Secure Enclave
        // keys) in an infinite loop that hangs app launch until the process is
        // killed. Deferring the first write breaks the reentrancy.
        Task { @MainActor [weak self] in self?.refreshOnlineState() }
    }

    // MARK: - Aggregated feed (downstream API: b8d.8 UI, b8d.12 detail nav)

    /// The interleaved-by-recency feed across every live instance: one item per
    /// project per machine, newest activity first. Reads live observable state,
    /// so SwiftUI views bound to it re-render whenever any instance syncs, a
    /// machine connects/disconnects, or the pairing set / names change.
    var items: [FeedItem] {
        #if DEBUG
        if let debugFixtureSources { return Self.buildItems(from: debugFixtureSources) }
        #endif
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

    #if DEBUG
    /// Canned sources rendered instead of the (empty) coordinator handles under
    /// the `-uitest-fixture-activity` seam — a device "paired" via the DEBUG
    /// toggle has no live handles, so the Feed would otherwise be empty. Seeds
    /// the same fixture snapshot + events the Activity tab used, so UI tests can
    /// exercise unread rows/badge, error styling, and event deep-links against
    /// the Feed deterministically. `nil` (default) in production and normal runs.
    var debugFixtureSources: [Source]?
    #endif

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

    /// Fold `sources` into the flat feed. Machines with no snapshot contribute
    /// nothing. Each item carries the project's latest matching `AgentEvent`
    /// (remote-control-fa8). Ordering is ATTENTION-FIRST (see `sorted(_:)`).
    static func buildItems(from sources: [Source]) -> [FeedItem] {
        var items: [FeedItem] = []
        for source in sources {
            guard let snapshot = source.snapshot else { continue }
            for project in snapshot.projects {
                let latest = latestEvent(projectId: project.projectId, events: source.events)
                items.append(FeedItem(
                    pairingId: source.pairingId,
                    displayName: source.displayName,
                    isOnline: source.isOnline,
                    project: project,
                    activityMs: latest?.occurredAtMs ?? snapshot.serverTimeMs,
                    latestEvent: latest
                ))
            }
        }
        return sorted(items)
    }

    /// The attention-first ordering the approved mockup shows (remote-control-fa8):
    ///   1. online before offline;
    ///   2. attention (live needs-input OR latest event needs-input/error)
    ///      before calm;
    ///   3. most-recent activity (`activityMs`) descending;
    ///   4. a stable tie-break preserving source (machine) order then project
    ///      order — deterministic regardless of the platform sort's stability.
    /// Net effect: the attention rows (needs-input / error) float to the top of
    /// the online group, calm rows below, and every offline row last — matching
    /// the approved mockup. Pure and unit-tested (like `buildItems`).
    static func sorted(_ items: [FeedItem]) -> [FeedItem] {
        items.enumerated()
            .sorted { lhs, rhs in
                let l = lhs.element, r = rhs.element
                if l.isOnline != r.isOnline { return l.isOnline }
                if l.needsAttention != r.needsAttention { return l.needsAttention }
                if l.activityMs != r.activityMs { return l.activityMs > r.activityMs }
                return lhs.offset < rhs.offset // stable tie-break
            }
            .map(\.element)
    }

    /// The most-recent `AgentEvent` deep-linking into `projectId`, or `nil` when
    /// the project has produced none.
    static func latestEvent(
        projectId: Wire.ProjectId,
        events: [Wire.AgentEvent]
    ) -> Wire.AgentEvent? {
        events
            .filter { $0.deepLink.projectId == projectId }
            .max { $0.occurredAtMs < $1.occurredAtMs }
    }

    /// Online iff the relay link is authenticated and live; every other state
    /// (disconnected / connecting / authenticating) reads as offline.
    static func isOnline(_ linkState: RemoteLinkState) -> Bool {
        if case .connected = linkState { return true }
        return false
    }
}

#if DEBUG
extension FeedStore {
    /// The synthetic machine the `-uitest-fixture-activity` feed is attributed
    /// to (no live handle exists under the DEBUG pairing toggle).
    enum Fixture {
        static let pairingId = "uitest-fixture-machine"
        static let displayName = "Studio"

        /// One canned source: the same `uiTestFixture` projects the
        /// Projects/Sessions tests use, folded with `ActivityFixtures`' mixed
        /// needs-input / finished / error events so the Feed shows one row per
        /// attention variant, all initially unread.
        static func source() -> FeedStore.Source {
            FeedStore.Source(
                pairingId: pairingId,
                displayName: displayName,
                isOnline: true,
                snapshot: .uiTestFixture,
                events: ActivityFixtures.events()
            )
        }
    }
}
#endif
