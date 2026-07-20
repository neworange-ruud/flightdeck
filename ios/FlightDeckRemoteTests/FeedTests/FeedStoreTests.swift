//
//  FeedStoreTests.swift
//  FlightDeckRemoteTests
//
//  Covers the aggregated multi-pairing feed (remote-control-b8d.6):
//   - the pure interleaver orders items by most-recent activity across 2+
//     machines (agent-event recency, else snapshot time);
//   - each item's machine chip resolves via override > desktop > fallback;
//   - an offline instance's items still appear (from its cached snapshot),
//     flagged offline, alongside a live machine's;
//   - the feed live-updates as an instance's store changes;
//   - live link state is mirrored back into `PairedInstance.lastKnownOnline`.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@MainActor
@Suite struct FeedStoreTests {

    // MARK: - Wire fixtures

    private func rollup() -> Wire.StatusRollup {
        Wire.StatusRollup(dot: .idle, summary: "1 agent", working: 0, idle: 1,
                          needsInput: 0, manual: 0, agentCount: 1)
    }

    private func git() -> Wire.GitIndicators {
        Wire.GitIndicators(branch: "main", added: 0, modified: 0, removed: 0,
                           ahead: 0, behind: 0, drift: 0, hasUpstream: true)
    }

    private func project(_ id: String, name: String? = nil) -> Wire.ProjectState {
        let session = Wire.SessionState(
            sessionId: Wire.SessionId("s_\(id)"), projectId: Wire.ProjectId(id),
            name: "fix-\(id)", agentType: .claudeCode, status: .idle, git: git(),
            runningTimeSecs: 0, pendingQuestion: nil)
        return Wire.ProjectState(projectId: Wire.ProjectId(id), name: name ?? "Project \(id)",
                                 rollup: rollup(), sessions: [session])
    }

    private func snapshot(serverTimeMs: Int64, projects: [Wire.ProjectState]) -> Wire.StateSnapshot {
        Wire.StateSnapshot(serverTimeMs: serverTimeMs, projects: projects)
    }

    private func event(projectId: String, occurredAtMs: Int64) -> Wire.AgentEvent {
        Wire.AgentEvent(
            eventId: Wire.EventId("evt_\(projectId)_\(occurredAtMs)"),
            kind: .finished(summary: "done", filesChanged: 1, readyToPush: false),
            deepLink: Wire.DeepLink(projectId: Wire.ProjectId(projectId),
                                    sessionId: Wire.SessionId("s_\(projectId)")),
            occurredAtMs: occurredAtMs,
            title: "\(projectId) finished")
    }

    // MARK: - Pure interleave core

    @Test func interleavesTwoMachinesByAgentEventRecency() {
        // Machine A: projects a1 (event @300), a2 (event @100).
        // Machine B: projects b1 (event @400), b2 (event @200).
        // Expected global order by recency: b1(400) > a1(300) > b2(200) > a2(100).
        let a = FeedStore.Source(
            pairingId: "A", displayName: "Studio", isOnline: true,
            snapshot: snapshot(serverTimeMs: 10, projects: [project("a1"), project("a2")]),
            events: [event(projectId: "a1", occurredAtMs: 300),
                     event(projectId: "a2", occurredAtMs: 100)])
        let b = FeedStore.Source(
            pairingId: "B", displayName: "Laptop", isOnline: true,
            snapshot: snapshot(serverTimeMs: 20, projects: [project("b1"), project("b2")]),
            events: [event(projectId: "b1", occurredAtMs: 400),
                     event(projectId: "b2", occurredAtMs: 200)])

        let items = FeedStore.buildItems(from: [a, b])

        #expect(items.map(\.projectId.rawValue) == ["b1", "a1", "b2", "a2"])
        #expect(items.map(\.pairingId) == ["B", "A", "B", "A"])
        #expect(items.map(\.activityMs) == [400, 300, 200, 100])
    }

    @Test func projectsWithoutEventsFallBackToSnapshotTime() {
        // No events anywhere → each item's recency is its machine's snapshot
        // time, so the newer machine's projects sort first (stable within a
        // machine by project order).
        let a = FeedStore.Source(
            pairingId: "A", displayName: "A", isOnline: true,
            snapshot: snapshot(serverTimeMs: 100, projects: [project("a1"), project("a2")]),
            events: [])
        let b = FeedStore.Source(
            pairingId: "B", displayName: "B", isOnline: true,
            snapshot: snapshot(serverTimeMs: 500, projects: [project("b1")]),
            events: [])

        let items = FeedStore.buildItems(from: [a, b])

        #expect(items.map(\.projectId.rawValue) == ["b1", "a1", "a2"])
        #expect(items.map(\.activityMs) == [500, 100, 100])
    }

    @Test func machinesWithoutSnapshotContributeNothing() {
        let a = FeedStore.Source(pairingId: "A", displayName: "A", isOnline: false,
                                 snapshot: nil, events: [])
        let b = FeedStore.Source(
            pairingId: "B", displayName: "B", isOnline: true,
            snapshot: snapshot(serverTimeMs: 5, projects: [project("b1")]), events: [])

        let items = FeedStore.buildItems(from: [a, b])

        #expect(items.map(\.projectId.rawValue) == ["b1"])
    }

    @Test func onlineIsTrueOnlyForConnectedLink() {
        #expect(FeedStore.isOnline(.connected(latencyMs: 8)))
        #expect(!FeedStore.isOnline(.disconnected))
        #expect(!FeedStore.isOnline(.connecting))
        #expect(!FeedStore.isOnline(.authenticating))
    }

    // MARK: - Latest-event folding (remote-control-fa8)

    private func kindedEvent(projectId: String, session: String, atMs: Int64,
                             kind: Wire.EventKind) -> Wire.AgentEvent {
        Wire.AgentEvent(
            eventId: Wire.EventId("evt_\(projectId)_\(atMs)"),
            kind: kind,
            deepLink: Wire.DeepLink(projectId: Wire.ProjectId(projectId),
                                    sessionId: Wire.SessionId(session)),
            occurredAtMs: atMs,
            title: "\(projectId) event")
    }

    @Test func eachItemCarriesItsNewestMatchingEvent() {
        let src = FeedStore.Source(
            pairingId: "A", displayName: "A", isOnline: true,
            snapshot: snapshot(serverTimeMs: 10, projects: [project("p1")]),
            events: [
                kindedEvent(projectId: "p1", session: "s_old", atMs: 100,
                            kind: .finished(summary: "old", filesChanged: 0, readyToPush: false)),
                kindedEvent(projectId: "p1", session: "s_new", atMs: 300,
                            kind: .needsInput(preview: "which env?")),
                kindedEvent(projectId: "other", session: "sx", atMs: 999,
                            kind: .error(message: "unrelated")),
            ])

        let item = try! #require(FeedStore.buildItems(from: [src]).first)
        #expect(item.latestEvent?.occurredAtMs == 300)
        #expect(item.isNeedsInputEvent)
        #expect(!item.isErrorEvent)
        #expect(item.needsAttention)
        #expect(item.attentionSessionId?.rawValue == "s_new")
        #expect(item.activityMs == 300)
    }

    @Test func aProjectWithoutMatchingEventsHasNoLatestEvent() {
        let src = FeedStore.Source(
            pairingId: "A", displayName: "A", isOnline: true,
            snapshot: snapshot(serverTimeMs: 42, projects: [project("p1")]),
            events: [])
        let item = try! #require(FeedStore.buildItems(from: [src]).first)
        #expect(item.latestEvent == nil)
        #expect(!item.needsAttention)
        #expect(item.attentionSessionId == nil)
        #expect(item.activityMs == 42) // falls back to snapshot time
    }

    @Test func errorEventDrivesErrorStateNotAttentionSessionFromANonAttentionRow() {
        let errItem = FeedItemFixtures.item(
            pairingId: "A", projectId: "p1", activityMs: 5,
            latestEvent: FeedItemFixtures.event(project: "p1", session: "s_err", atMs: 5,
                                                kind: .error(message: "boom")))
        #expect(errItem.isErrorEvent)
        #expect(errItem.needsAttention)
        #expect(errItem.attentionSessionId?.rawValue == "s_err")

        let doneItem = FeedItemFixtures.item(
            pairingId: "A", projectId: "p2", activityMs: 5,
            latestEvent: FeedItemFixtures.event(project: "p2", session: "s_done", atMs: 5,
                                                kind: .finished(summary: "ok", filesChanged: 1, readyToPush: true)))
        #expect(!doneItem.isErrorEvent)
        #expect(!doneItem.needsAttention)
        #expect(doneItem.attentionSessionId == nil) // finished rows open the sessions list
    }

    // MARK: - Attention-first sort (remote-control-fa8)

    @Test func sortOrdersOnlineAttentionRecencyThenOffline() {
        // Deliberately shuffled input across all ranking axes. The spec's three
        // keys are: online → attention(bool) → activityMs desc (stable). So
        // within the attention band, the MORE RECENT attention row leads.
        let needsInput = FeedItemFixtures.item(
            pairingId: "A", projectId: "needs", dot: .needsInput, isOnline: true, activityMs: 100)
        let error = FeedItemFixtures.item(
            pairingId: "A", projectId: "err", isOnline: true, activityMs: 400,
            latestEvent: FeedItemFixtures.event(project: "err", atMs: 400, kind: .error(message: "x")))
        let finishedNewer = FeedItemFixtures.item(
            pairingId: "A", projectId: "fin2", isOnline: true, activityMs: 300,
            latestEvent: FeedItemFixtures.event(project: "fin2", atMs: 300,
                                                kind: .finished(summary: "y", filesChanged: 0, readyToPush: false)))
        let idleOlder = FeedItemFixtures.item(
            pairingId: "A", projectId: "idle", isOnline: true, activityMs: 50)
        let offlineNeedsInput = FeedItemFixtures.item(
            pairingId: "B", projectId: "offneeds", dot: .needsInput, isOnline: false, activityMs: 999)

        let sorted = FeedStore.sorted([idleOlder, offlineNeedsInput, finishedNewer, needsInput, error])
        // Online first; within online, attention (err, needs) before calm
        // (fin2, idle), each band by recency desc; offline always last (even a
        // high-recency offline needs-input row).
        #expect(sorted.map(\.projectId.rawValue) == ["err", "needs", "fin2", "idle", "offneeds"])
    }

    @Test func sortPutsAttentionAheadOfMoreRecentCalmRow() {
        // A needs-input row with OLDER activity still outranks a newer calm row.
        let attentionOld = FeedItemFixtures.item(
            pairingId: "A", projectId: "att", dot: .needsInput, isOnline: true, activityMs: 1)
        let calmNew = FeedItemFixtures.item(
            pairingId: "A", projectId: "calm", isOnline: true, activityMs: 9_999)
        #expect(FeedStore.sorted([calmNew, attentionOld]).map(\.projectId.rawValue) == ["att", "calm"])
    }

    // MARK: - Store-backed harness

    private struct Harness {
        let coordinator: TransportCoordinator
        let pairingStore: PairingStore
        let feed: FeedStore
    }

    /// Build a coordinator with one (unstarted) client/store per spec, a
    /// `PairingStore` seeded with matching `PairedInstance`s (carrying the
    /// requested name config), and a `FeedStore` over both. The clients are
    /// never started — tests drive each store directly via `debugSeed`.
    private func makeHarness(
        _ specs: [(id: String, desktop: String?, override: String?)]
    ) throws -> Harness {
        let keychain = InMemoryKeychainStore()
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        let keyAgreement = try KeyAgreementKeys.loadOrCreate(store: keychain)
        let recordStore = PairingRecordStore(store: keychain)

        var instances: [PairedInstance] = []
        for (index, spec) in specs.enumerated() {
            let (peer, _) = try TransportFixtures.makePeer(
                keychain: keychain, pairingId: spec.id,
                salt: Data("salt-\(spec.id)-000000000000".utf8),
                relayURL: "wss://relay.example/\(spec.id)")
            try recordStore.save(peer.record)
            instances.append(PairedInstance(
                pairingId: spec.id,
                machineNameFromDesktop: spec.desktop,
                userOverrideName: spec.override,
                relayURL: URL(string: peer.record.relayURL)!,
                pairedAt: Date(timeIntervalSince1970: TimeInterval(1_000 + index))))
        }

        let pairingStore = PairingStore(
            storage: InMemoryPairingStateProvider(initial: true),
            instancesStorage: InMemoryPairedInstancesProvider(initial: instances))

        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        let coordinator = TransportCoordinator(
            identity: identity, keyAgreement: keyAgreement, recordStore: recordStore,
            connectorFactory: { ScriptedConnector(channel: ScriptedChannel()) },
            clientConfig: config, now: { 1 })
        coordinator.installInitialInstances(instances)

        let feed = FeedStore(coordinator: coordinator, pairingStore: pairingStore)
        return Harness(coordinator: coordinator, pairingStore: pairingStore, feed: feed)
    }

    // MARK: - Display-name resolution

    @Test func machineChipResolvesOverrideThenDesktopThenFallback() throws {
        let h = try makeHarness([
            ("over", "DesktopName", "MyMac"),   // override wins
            ("desk", "DesktopName", nil),        // desktop name
            ("fall", nil, nil),                  // fallback
        ])
        // Seed each store live with one project so all three surface.
        h.coordinator.store(for: "over")?.debugSeed(snapshot: snapshot(serverTimeMs: 30, projects: [project("po")]))
        h.coordinator.store(for: "desk")?.debugSeed(snapshot: snapshot(serverTimeMs: 20, projects: [project("pd")]))
        h.coordinator.store(for: "fall")?.debugSeed(snapshot: snapshot(serverTimeMs: 10, projects: [project("pf")]))

        let byPairing = Dictionary(uniqueKeysWithValues: h.feed.items.map { ($0.pairingId, $0.displayName) })
        #expect(byPairing["over"] == "MyMac")
        #expect(byPairing["desk"] == "DesktopName")
        #expect(byPairing["fall"] == PairedInstance.fallbackDisplayName)
    }

    @Test func renameViaPairingStoreUpdatesChip() throws {
        let h = try makeHarness([("m", "OldDesktop", nil)])
        h.coordinator.store(for: "m")?.debugSeed(snapshot: snapshot(serverTimeMs: 1, projects: [project("p")]))
        #expect(h.feed.items.first?.displayName == "OldDesktop")

        h.pairingStore.setOverrideName(pairingId: "m", "Renamed")
        #expect(h.feed.items.first?.displayName == "Renamed")
    }

    // MARK: - Offline machine still appears (from cache), flagged offline

    @Test func offlineMachineItemsAppearFlaggedAlongsideLiveMachine() throws {
        let h = try makeHarness([("live", "Live", nil), ("down", "Down", nil)])
        // Live machine: connected snapshot (newer).
        h.coordinator.store(for: "live")?.debugSeed(
            snapshot: snapshot(serverTimeMs: 200, projects: [project("pl")]),
            linkState: .connected(latencyMs: 5))
        // Offline machine: a last-known (cache-seeded) snapshot, link down.
        h.coordinator.store(for: "down")?.debugSeed(
            snapshot: snapshot(serverTimeMs: 100, projects: [project("pd")]),
            linkState: .disconnected)

        let items = h.feed.items
        #expect(items.count == 2)

        let live = try #require(items.first { $0.pairingId == "live" })
        let down = try #require(items.first { $0.pairingId == "down" })
        #expect(live.isOnline)
        #expect(down.isOffline)
        // Both render; the offline one is still present (from its cached snapshot).
        #expect(down.project.projectId.rawValue == "pd")
        // Interleaved by recency: the live (newer) machine sorts first.
        #expect(items.map(\.pairingId) == ["live", "down"])
    }

    // MARK: - Live update as an instance syncs

    @Test func feedReflectsStoreChanges() throws {
        let h = try makeHarness([("m", "M", nil)])
        let store = try #require(h.coordinator.store(for: "m"))

        store.debugSeed(snapshot: snapshot(serverTimeMs: 1, projects: [project("p1")]))
        #expect(h.feed.items.map(\.projectId.rawValue) == ["p1"])

        // A later sync adds a second project — the feed recomputes from live state.
        store.debugSeed(snapshot: snapshot(serverTimeMs: 2, projects: [project("p1"), project("p2")]))
        #expect(Set(h.feed.items.map(\.projectId.rawValue)) == ["p1", "p2"])
    }

    // MARK: - Online-state write-back

    @Test func refreshOnlineStateMirrorsLinkIntoLastKnownOnline() throws {
        let h = try makeHarness([("up", "Up", nil), ("down", "Down", nil)])
        h.coordinator.store(for: "up")?.debugSeed(
            snapshot: snapshot(serverTimeMs: 1, projects: [project("pu")]),
            linkState: .connected(latencyMs: 3))
        h.coordinator.store(for: "down")?.debugSeed(
            snapshot: snapshot(serverTimeMs: 1, projects: [project("pd")]),
            linkState: .disconnected)

        h.feed.refreshOnlineState()

        let up = try #require(h.pairingStore.instances.first { $0.pairingId == "up" })
        let down = try #require(h.pairingStore.instances.first { $0.pairingId == "down" })
        #expect(up.lastKnownOnline)
        #expect(!down.lastKnownOnline)
    }

    @Test func linkStateChangeAutomaticallyUpdatesLastKnownOnline() async throws {
        let h = try makeHarness([("m", "M", nil)])
        // At init the store is disconnected → lastKnownOnline primed to false.
        // The prime is deferred to the next main-actor tick (FeedStore.init must
        // not mutate the shared @Observable PairingStore synchronously — doing so
        // during SwiftUI view construction reentrantly hangs app launch), so await
        // it rather than reading synchronously.
        let primedOffline = await waitUntilMain { h.pairingStore.instances.first?.lastKnownOnline == false }
        #expect(primedOffline)

        // Flip the link live; the armed observation writes it back.
        h.coordinator.store(for: "m")?.debugSeed(
            snapshot: snapshot(serverTimeMs: 1, projects: [project("p")]),
            linkState: .connected(latencyMs: 2))

        let updated = await waitUntilMain { h.pairingStore.instances.first?.lastKnownOnline == true }
        #expect(updated)
    }
}
