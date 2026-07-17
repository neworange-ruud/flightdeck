//
//  ShellTransportRoutingTests.swift
//  FlightDeckRemoteTests
//
//  The additive TransportStore shell surface: `shell_output` chunks route into
//  `shellOutput` (per shell id), `shell_event` lifecycle events route into
//  `shellEvents` (per session id), and the thin `openShell`/`sendShellInput`/
//  `interruptShell`/`closeShell` wrappers seal the right `CommandBody`. Uses
//  the same scripted-socket harness as `TransportStoreTests`.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@MainActor
@Suite struct ShellTransportRoutingTests {

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

    @Test func routesShellOutputAndEventsIntoStoreState() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let (store, client) = try makeStore(keychain: keychain, channel: channel, peer: peer, ka: ka)
        await store.start()
        await handshake(channel, store: store)

        let sessionId = Wire.SessionId("s1")
        let shellId = Wire.ShellId("sh1")

        // Lifecycle: opened.
        let opened = Wire.ShellEvent(sessionId: sessionId, shellId: shellId,
                                     kind: .opened(cols: 80, rows: 24))
        try await channel.push(peer.envelopeFrame(.shellEvent(opened), seq: 1))
        _ = await waitUntilMain { store.shellEvents[sessionId]?.count == 1 }
        #expect(store.shellEvents[sessionId]?.first?.kind == .opened(cols: 80, rows: 24))

        // Output chunks accumulate per shell id, in arrival order.
        let chunk1 = Wire.ShellOutput(sessionId: sessionId, shellId: shellId,
                                      stream: .stdout, seq: 1, data: "$ ls\r\n")
        let chunk2 = Wire.ShellOutput(sessionId: sessionId, shellId: shellId,
                                      stream: .stdout, seq: 2, data: "\u{1b}[32mok\u{1b}[0m\r\n")
        try await channel.push(peer.envelopeFrame(.shellOutput(chunk1), seq: 2))
        try await channel.push(peer.envelopeFrame(.shellOutput(chunk2), seq: 3))
        _ = await waitUntilMain { store.shellOutput[shellId]?.count == 2 }
        #expect(store.shellOutput[shellId]?.map(\.seq) == [1, 2])
        #expect(store.shellOutput[shellId]?.first?.data == "$ ls\r\n")

        // Lifecycle: exited (slot held) then closed.
        let exited = Wire.ShellEvent(sessionId: sessionId, shellId: shellId, kind: .exited(code: 0))
        try await channel.push(peer.envelopeFrame(.shellEvent(exited), seq: 4))
        _ = await waitUntilMain { store.shellEvents[sessionId]?.count == 2 }
        #expect(store.shellEvents[sessionId]?.last?.kind == .exited(code: 0))

        await client.stop()
    }

    @Test func shellCommandWrappersSealTheRightBodies() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let (store, client) = try makeStore(keychain: keychain, channel: channel, peer: peer, ka: ka)
        await store.start()
        await handshake(channel, store: store)

        let sessionId = Wire.SessionId("s1")
        let shellId = Wire.ShellId("sh1")

        let open = store.openShell(sessionId: sessionId, shellId: shellId, cols: 100, rows: 32)
        #expect(open.body == .shellOpen(sessionId: sessionId, shellId: shellId, cols: 100, rows: 32))

        let input = store.sendShellInput(sessionId: sessionId, shellId: shellId, data: "ls\r")
        #expect(input.body == .shellInput(sessionId: sessionId, shellId: shellId, data: "ls\r"))

        let interrupt = store.interruptShell(sessionId: sessionId, shellId: shellId)
        #expect(interrupt.body == .shellInterrupt(sessionId: sessionId, shellId: shellId))

        let close = store.closeShell(sessionId: sessionId, shellId: shellId)
        #expect(close.body == .shellClose(sessionId: sessionId, shellId: shellId))

        // Each wrapper minted a distinct tracked handle.
        let ids = [open, input, interrupt, close].map(\.commandId)
        #expect(Set(ids).count == 4)

        await client.stop()
    }
}
