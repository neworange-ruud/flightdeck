//
//  TransportStoreTests.swift
//  FlightDeckRemoteTests
//
//  End-to-end through the app-facing facade: `TransportStore` runs a real
//  `TransportClient` over a scripted socket and folds the event stream —
//  snapshot, incremental status/rollup deltas, transcript replace/append,
//  deduped AgentEvents, and per-command delivery honesty.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@MainActor
@Suite struct TransportStoreTests {

    private func git() -> Wire.GitIndicators {
        Wire.GitIndicators(branch: "main", added: 0, modified: 0, removed: 0,
                           ahead: 0, behind: 0, drift: 0, hasUpstream: true)
    }

    private func rollup() -> Wire.StatusRollup {
        Wire.StatusRollup(dot: .idle, summary: "1 agent", working: 0, idle: 1,
                          needsInput: 0, manual: 0, agentCount: 1)
    }

    private func snapshot() -> Wire.StateSnapshot {
        let session = Wire.SessionState(
            sessionId: Wire.SessionId("s1"), projectId: Wire.ProjectId("p1"),
            name: "fix-login", agentType: .claudeCode, status: .idle, git: git(),
            runningTimeSecs: 0, pendingQuestion: nil)
        let project = Wire.ProjectState(projectId: Wire.ProjectId("p1"), name: "Proj",
                                        rollup: rollup(), sessions: [session])
        return Wire.StateSnapshot(serverTimeMs: 1, projects: [project])
    }

    private func makeStore(keychain: InMemoryKeychainStore, channel: ScriptedChannel,
                           peer: DesktopPeer, ka: KeyAgreementKeys) throws -> (TransportStore, TransportClient) {
        let recordStore = PairingRecordStore(store: keychain)
        try recordStore.save(peer.record)
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        config.requestSnapshotOnResume = false
        config.commandTimeout = .seconds(30)
        let client = TransportClient(
            identity: identity, keyAgreement: ka, recordStore: recordStore,
            connector: ScriptedConnector(channel: channel),
            clientInfo: Wire.ClientInfo(appVersion: "test", platform: "ios", osVersion: nil),
            config: config, jitter: { 0 }, now: { 1 })
        let store = TransportStore(client: client, now: { 1 })
        return (store, client)
    }

    private func handshake(_ channel: ScriptedChannel, store: TransportStore) async {
        await channel.push(.helloOk(protocolVersion: 1, serverTimeMs: 1, connectionId: "c"))
        await channel.push(.authChallenge(nonce: TransportFixtures.nonceB64(), serverTimeMs: 1))
        await channel.push(.authOk(pairingIds: [Wire.PairingId("pair_test_1")]))
        _ = await waitUntilMain { if case .connected = store.linkState { return true }; return false }
    }

    @Test func foldsSnapshotStatusRollupTranscriptAndEvents() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let (store, client) = try makeStore(keychain: keychain, channel: channel, peer: peer, ka: ka)
        await store.start()
        await handshake(channel, store: store)

        // Snapshot (seq 1).
        try await channel.push(peer.envelopeFrame(.snapshot(snapshot()), seq: 1))
        _ = await waitUntilMain { store.snapshot != nil }
        #expect(store.snapshot?.projects.first?.sessions.first?.status == .idle)

        // Status delta (seq 2): flip the session to needs_input.
        let delta = Wire.SessionStatusDelta(
            sessionId: Wire.SessionId("s1"), projectId: Wire.ProjectId("p1"),
            status: .needsInput, runningTimeSecs: 42, pendingQuestion: "Run it?")
        try await channel.push(peer.envelopeFrame(.statusUpdate(Wire.StatusUpdate(updates: [delta])), seq: 2))
        _ = await waitUntilMain { store.snapshot?.projects.first?.sessions.first?.status == .needsInput }
        #expect(store.snapshot?.projects.first?.sessions.first?.runningTimeSecs == 42)
        #expect(store.snapshot?.projects.first?.sessions.first?.pendingQuestion == "Run it?")

        // Rollup refresh (seq 3).
        let newRollup = Wire.StatusRollup(dot: .needsInput, summary: "1 needs input",
                                          working: 0, idle: 0, needsInput: 1, manual: 0, agentCount: 1)
        try await channel.push(peer.envelopeFrame(
            .rollup(Wire.RollupUpdate(projects: [Wire.ProjectRollup(projectId: Wire.ProjectId("p1"), rollup: newRollup)])), seq: 3))
        _ = await waitUntilMain { store.snapshot?.projects.first?.rollup.dot == .needsInput }

        // Transcript replace (seq 4) then append (seq 5).
        let item1 = Wire.TranscriptItem.userMessage(itemId: Wire.ItemId("i1"), text: "hello", atMs: 1)
        try await channel.push(peer.envelopeFrame(
            .transcript(Wire.TranscriptFeed(sessionId: Wire.SessionId("s1"), fromIndex: 0, replace: true, items: [item1])), seq: 4))
        _ = await waitUntilMain { store.transcripts[Wire.SessionId("s1")]?.count == 1 }
        let item2 = Wire.TranscriptItem.agentMessage(itemId: Wire.ItemId("i2"), text: "hi", atMs: 2)
        try await channel.push(peer.envelopeFrame(
            .transcriptAppend(Wire.TranscriptFeed(sessionId: Wire.SessionId("s1"), fromIndex: 1, replace: false, items: [item2])), seq: 5))
        _ = await waitUntilMain { store.transcripts[Wire.SessionId("s1")]?.count == 2 }

        // AgentEvents deduped by event_id across two distinct envelopes.
        let event = Wire.AgentEvent(
            eventId: Wire.EventId("evt1"), kind: .finished(summary: "done", filesChanged: 2, readyToPush: true),
            deepLink: Wire.DeepLink(projectId: Wire.ProjectId("p1"), sessionId: Wire.SessionId("s1"), itemId: nil),
            occurredAtMs: 1, title: "finished")
        try await channel.push(peer.envelopeFrame(.event(event), seq: 6))
        _ = await waitUntilMain { store.agentEvents.count == 1 }
        try await channel.push(peer.envelopeFrame(.event(event), seq: 7)) // same event_id, new seq
        try? await Task.sleep(for: .milliseconds(120))
        #expect(store.agentEvents.count == 1)

        await client.stop()
    }

    @Test func appliesABurstOfEventsInFIFOOrder() async throws {
        // A rapid burst pushed with no intermediate awaits exercises the event
        // bridge's ordering: the store must fold transcript appends in emit
        // order (remote-control-qbj). The old per-event `Task { @MainActor }`
        // bridge gave no such guarantee.
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let (store, client) = try makeStore(keychain: keychain, channel: channel, peer: peer, ka: ka)
        await store.start()
        await handshake(channel, store: store)

        try await channel.push(peer.envelopeFrame(.snapshot(snapshot()), seq: 1))
        _ = await waitUntilMain { store.snapshot != nil }

        // Replace with i0, then append i1…i20 back-to-back.
        let s1 = Wire.SessionId("s1")
        try await channel.push(peer.envelopeFrame(
            .transcript(Wire.TranscriptFeed(sessionId: s1, fromIndex: 0, replace: true,
                items: [.userMessage(itemId: Wire.ItemId("i0"), text: "0", atMs: 0)])), seq: 2))
        for n in 1...20 {
            try await channel.push(peer.envelopeFrame(
                .transcriptAppend(Wire.TranscriptFeed(sessionId: s1, fromIndex: UInt64(n), replace: false,
                    items: [.userMessage(itemId: Wire.ItemId("i\(n)"), text: "\(n)", atMs: Int64(n))])), seq: UInt64(n + 2)))
        }

        _ = await waitUntilMain { store.transcripts[s1]?.count == 21 }
        let ids = store.transcripts[s1]?.map(\.itemId.rawValue) ?? []
        #expect(ids == (0...20).map { "i\($0)" })
        await client.stop()
    }

    @Test func sendCommandTracksDeliveryHonestyThroughAck() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let (store, client) = try makeStore(keychain: keychain, channel: channel, peer: peer, ka: ka)
        await store.start()
        await handshake(channel, store: store)

        let handle = store.sendCommand(.reply(sessionId: Wire.SessionId("s1"), text: "yes"))
        #expect(handle.delivery == .sending)

        // Wait for the outbound envelope, then desktop acks the command.
        _ = await waitUntilMain {
            await channel.sentFrames().contains { if case .envelope = $0 { return true }; return false }
        }
        let ack = Wire.CommandAck(commandId: handle.commandId, outcome: .applied, message: nil)
        try await channel.push(peer.envelopeFrame(.commandAck(ack), seq: 1))

        _ = await waitUntilMain { handle.delivery == .delivered(.applied) }
        #expect(handle.delivery == .delivered(.applied))
        await client.stop()
    }

    // MARK: - Machine name (REMOTE_PROTOCOL §5.7, remote-control-b8d.9)

    @Test func foldsMachineNameEventIntoObservableProperty() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let (store, client) = try makeStore(keychain: keychain, channel: channel, peer: peer, ka: ka)
        await store.start()
        await handshake(channel, store: store)

        #expect(store.machineName == nil)

        await channel.push(.machineName(pairingId: Wire.PairingId("pair_test_1"), machineName: "Ruud's MacBook Pro"))
        _ = await waitUntilMain { store.machineName != nil }

        #expect(store.machineName == "Ruud's MacBook Pro")
        await client.stop()
    }

    @Test func aRenameOnTheDesktopUpdatesTheFoldedMachineName() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let (store, client) = try makeStore(keychain: keychain, channel: channel, peer: peer, ka: ka)
        await store.start()
        await handshake(channel, store: store)

        await channel.push(.machineName(pairingId: Wire.PairingId("pair_test_1"), machineName: "Old Name"))
        _ = await waitUntilMain { store.machineName == "Old Name" }

        // The desktop renamed and re-announced on the SAME live session (or a
        // reconnect would send it again post-auth_ok either way) — the folded
        // property must move to the new value, not stick with the first one.
        await channel.push(.machineName(pairingId: Wire.PairingId("pair_test_1"), machineName: "New Name"))
        _ = await waitUntilMain { store.machineName == "New Name" }

        #expect(store.machineName == "New Name")
        await client.stop()
    }
}
