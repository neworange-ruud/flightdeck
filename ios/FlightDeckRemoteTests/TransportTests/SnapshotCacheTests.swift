//
//  SnapshotCacheTests.swift
//  FlightDeckRemoteTests
//
//  Covers the offline last-known-state cache (PRD §9.2): disk round-trip,
//  the ~2s write debounce (driven by an injected `SnapshotCacheClock` gate,
//  never a real clock), the 100-item/session transcript cap, and the stale
//  flag lifecycle on `TransportStore` — cache-seeded ⇒ stale, live snapshot
//  arrives ⇒ stale cleared.
//

import Testing
import Foundation
@testable import FlightDeckRemote

// MARK: - Test clocks

/// A clock whose `sleep` returns immediately — collapses the debounce so
/// round-trip/cap tests don't care about timing.
private struct ImmediateClock: SnapshotCacheClock {
    func sleep(for duration: Duration) async {}
}

/// A clock whose `sleep` suspends until the test explicitly releases it, so
/// the debounce window is held open deterministically.
private final class GateClock: SnapshotCacheClock, @unchecked Sendable {
    private let lock = NSLock()
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func sleep(for duration: Duration) async {
        await withCheckedContinuation { continuation in
            lock.lock()
            waiters.append(continuation)
            lock.unlock()
        }
    }

    /// Number of sleeps currently suspended.
    var pendingCount: Int {
        lock.lock(); defer { lock.unlock() }
        return waiters.count
    }

    /// Resumes every suspended sleep ("the debounce interval elapsed").
    func releaseAll() {
        lock.lock()
        let released = waiters
        waiters = []
        lock.unlock()
        released.forEach { $0.resume() }
    }
}

// MARK: - Tests

@MainActor
@Suite struct SnapshotCacheTests {

    private func tempFileURL() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("snapshot-cache-tests-\(UUID().uuidString).json")
    }

    private func snapshot(serverTimeMs: Int64 = 1) -> Wire.StateSnapshot {
        let session = Wire.SessionState(
            sessionId: Wire.SessionId("s1"), projectId: Wire.ProjectId("p1"),
            name: "fix-login", agentType: .claudeCode, status: .idle,
            git: Wire.GitIndicators(branch: "main", added: 0, modified: 0, removed: 0,
                                     ahead: 0, behind: 0, drift: 0, hasUpstream: true),
            runningTimeSecs: 0, pendingQuestion: nil)
        let project = Wire.ProjectState(
            projectId: Wire.ProjectId("p1"), name: "Proj",
            rollup: Wire.StatusRollup(dot: .idle, summary: "1 agent", working: 0, idle: 1,
                                       needsInput: 0, manual: 0, agentCount: 1),
            sessions: [session])
        return Wire.StateSnapshot(serverTimeMs: serverTimeMs, projects: [project])
    }

    private func items(_ count: Int) -> [Wire.TranscriptItem] {
        (0..<count).map { .agentMessage(itemId: Wire.ItemId("i\($0)"), text: "t\($0)", atMs: Int64($0)) }
    }

    // MARK: Round trip

    @Test func loadReturnsNilWhenNothingWasEverSaved() {
        let cache = SnapshotCache(fileURL: tempFileURL(), clock: ImmediateClock())
        #expect(cache.load() == nil)
    }

    @Test func savedStateRoundTripsThroughDisk() async {
        let url = tempFileURL()
        let cache = SnapshotCache(fileURL: url, clock: ImmediateClock(), now: { 42 })
        let snap = snapshot()
        cache.scheduleSave(snapshot: snap, transcripts: [Wire.SessionId("s1"): items(3)])

        _ = await waitUntilMain { cache.load() != nil }
        let loaded = cache.load()
        #expect(loaded?.snapshot == snap)
        #expect(loaded?.cachedAtMs == 42)
        #expect(loaded?.transcripts.count == 1)
        #expect(loaded?.transcripts.first?.sessionId == Wire.SessionId("s1"))
        #expect(loaded?.transcripts.first?.items == items(3))

        // A fresh cache instance over the same file sees the same state
        // (the app-restart path `TransportStoreFactory` takes).
        let reloaded = SnapshotCache(fileURL: url, clock: ImmediateClock())
        #expect(reloaded.load() == loaded)
    }

    @Test func corruptFileLoadsAsNilInsteadOfCrashing() throws {
        let url = tempFileURL()
        try Data("not json {".utf8).write(to: url)
        let cache = SnapshotCache(fileURL: url, clock: ImmediateClock())
        #expect(cache.load() == nil)
    }

    // MARK: Debounce

    @Test func writeIsHeldUntilTheDebounceIntervalElapses() async {
        let url = tempFileURL()
        let gate = GateClock()
        let cache = SnapshotCache(fileURL: url, clock: gate)
        cache.scheduleSave(snapshot: snapshot(), transcripts: [:])

        // The debounce sleep is pending; nothing is on disk yet.
        _ = await waitUntilMain { gate.pendingCount == 1 }
        #expect(cache.load() == nil)

        gate.releaseAll()
        _ = await waitUntilMain { cache.load() != nil }
        #expect(cache.load()?.snapshot == snapshot())
    }

    @Test func rapidSavesCoalesceIntoOneWriteCarryingTheLatestState() async {
        let url = tempFileURL()
        let gate = GateClock()
        let cache = SnapshotCache(fileURL: url, clock: gate)

        cache.scheduleSave(snapshot: snapshot(serverTimeMs: 1), transcripts: [:])
        cache.scheduleSave(snapshot: snapshot(serverTimeMs: 2), transcripts: [:])
        cache.scheduleSave(snapshot: snapshot(serverTimeMs: 3), transcripts: [:])
        _ = await waitUntilMain { gate.pendingCount == 3 }
        #expect(cache.load() == nil, "nothing should reach disk inside the debounce window")

        gate.releaseAll()
        _ = await waitUntilMain { cache.load() != nil }
        // Only the newest state landed — stale generations were dropped.
        #expect(cache.load()?.snapshot.serverTimeMs == 3)
    }

    // MARK: Transcript cap

    @Test func transcriptsAreCappedToTheLastHundredItemsPerSession() async {
        let cache = SnapshotCache(fileURL: tempFileURL(), clock: ImmediateClock())
        cache.scheduleSave(snapshot: snapshot(), transcripts: [
            Wire.SessionId("s1"): items(150),
            Wire.SessionId("s2"): items(10),
        ])

        _ = await waitUntilMain { cache.load() != nil }
        let loaded = cache.load()
        let s1 = loaded?.transcripts.first(where: { $0.sessionId == Wire.SessionId("s1") })
        let s2 = loaded?.transcripts.first(where: { $0.sessionId == Wire.SessionId("s2") })
        #expect(s1?.items.count == SnapshotCache.transcriptCapPerSession)
        // The cap keeps the *trailing* window — newest items survive.
        #expect(s1?.items.first?.itemId == Wire.ItemId("i50"))
        #expect(s1?.items.last?.itemId == Wire.ItemId("i149"))
        #expect(s2?.items.count == 10)
    }

    // MARK: Stale flag lifecycle (TransportStore integration)

    private func makeStore(keychain: InMemoryKeychainStore, channel: ScriptedChannel,
                           peer: DesktopPeer, ka: KeyAgreementKeys,
                           cache: SnapshotCache?) throws -> (TransportStore, TransportClient) {
        let recordStore = PairingRecordStore(store: keychain)
        try recordStore.save(peer.record)
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        config.requestSnapshotOnResume = false
        let client = TransportClient(
            identity: identity, keyAgreement: ka, recordStore: recordStore,
            connector: ScriptedConnector(channel: channel),
            clientInfo: Wire.ClientInfo(appVersion: "test", platform: "ios", osVersion: nil),
            config: config, jitter: { 0 }, now: { 1 })
        let store = TransportStore(client: client, cache: cache, now: { 1 })
        return (store, client)
    }

    @Test func seedFromCachePopulatesStateAndMarksItStale() throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let (store, _) = try makeStore(keychain: keychain, channel: ScriptedChannel(),
                                       peer: peer, ka: ka, cache: nil)

        let cached = SnapshotCache.CachedState(
            snapshot: snapshot(),
            transcripts: [.init(sessionId: Wire.SessionId("s1"), items: items(2))],
            cachedAtMs: 7)
        store.seedFromCache(cached)

        #expect(store.isCacheStale)
        #expect(store.snapshot == snapshot())
        #expect(store.transcripts[Wire.SessionId("s1")] == items(2))
    }

    @Test func liveSnapshotClearsTheStaleFlagAndPersistsFreshState() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let cache = SnapshotCache(fileURL: tempFileURL(), clock: ImmediateClock(), now: { 9 })
        let (store, client) = try makeStore(keychain: keychain, channel: channel,
                                            peer: peer, ka: ka, cache: cache)

        // Launch path: seeded from cache, stale.
        store.seedFromCache(SnapshotCache.CachedState(snapshot: snapshot(), transcripts: [], cachedAtMs: 1))
        #expect(store.isCacheStale)

        // Transport connects and a real snapshot arrives.
        await store.start()
        await channel.push(.helloOk(protocolVersion: 1, serverTimeMs: 1, connectionId: "c"))
        await channel.push(.authChallenge(nonce: TransportFixtures.nonceB64(), serverTimeMs: 1))
        await channel.push(.authOk(pairingIds: [Wire.PairingId("pair_test_1")]))
        _ = await waitUntilMain { if case .connected = store.linkState { return true }; return false }

        let live = snapshot(serverTimeMs: 99)
        try await channel.push(peer.envelopeFrame(.snapshot(live), seq: 1))
        _ = await waitUntilMain { store.isCacheStale == false }

        #expect(store.isCacheStale == false)
        #expect(store.snapshot == live)

        // The live snapshot also reached the cache (debounced write, but the
        // immediate clock collapses the wait).
        _ = await waitUntilMain { cache.load()?.snapshot.serverTimeMs == 99 }
        #expect(cache.load()?.snapshot.serverTimeMs == 99)

        await client.stop()
    }
}
