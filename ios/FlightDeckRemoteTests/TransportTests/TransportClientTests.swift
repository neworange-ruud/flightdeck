//
//  TransportClientTests.swift
//  FlightDeckRemoteTests
//
//  Drives the `TransportClient` state machine against a scripted WebSocket:
//  the full happy path (hello → auth → resume → snapshot), auth-signature
//  validity, resume cursor, inbound dedup + cumulative ack, outbound seal +
//  gapless seq, and delivery-honesty (ack vs. timeout).
//

import Testing
import Foundation
import CryptoKit
@testable import FlightDeckRemote

@Suite struct TransportClientTests {

    // MARK: - Builders

    private func makeClient(
        keychain: InMemoryKeychainStore,
        peer: DesktopPeer,
        keyAgreement: KeyAgreementKeys,
        channel: ScriptedChannel,
        collector: EventCollector,
        config: TransportClient.Config
    ) throws -> TransportClient {
        let recordStore = PairingRecordStore(store: keychain)
        try recordStore.save(peer.record)
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        let connector = ScriptedConnector(channel: channel)
        let client = TransportClient(
            identity: identity,
            keyAgreement: keyAgreement,
            recordStore: recordStore,
            connector: connector,
            clientInfo: Wire.ClientInfo(appVersion: "test", platform: "ios", osVersion: nil),
            config: config,
            jitter: { 0 },
            now: { 1_752_000_100_000 }
        )
        return client
    }

    private func verifySignature(_ frame: Wire.RelayFrame, identityPublicKeyB64: String, nonceB64: String) -> Bool {
        guard case let .authResponse(_, signature, _) = frame,
              let pub = Data(base64Encoded: identityPublicKeyB64),
              let sig = Data(base64Encoded: signature),
              let nonce = Data(base64Encoded: nonceB64),
              let key = try? P256.Signing.PublicKey(x963Representation: pub),
              let ecdsa = try? P256.Signing.ECDSASignature(rawRepresentation: sig)
        else { return false }
        return key.isValidSignature(ecdsa, for: nonce)
    }

    /// Push the scripted handshake and wait until the client reaches `.live`.
    private func handshake(_ channel: ScriptedChannel, client: TransportClient, nonceB64: String) async {
        await channel.push(.helloOk(protocolVersion: 1, serverTimeMs: 1, connectionId: "c1"))
        await channel.push(.authChallenge(nonce: nonceB64, serverTimeMs: 1))
        await channel.push(.authOk(pairingIds: [Wire.PairingId("pair_test_1")]))
        _ = await waitUntil { if case .connected = await client.currentLinkState() { return true }; return false }
    }

    // MARK: - Happy path

    @Test func fullHappyPathHandshakeResumeAndSnapshot() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let collector = EventCollector()
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        let client = try makeClient(keychain: keychain, peer: peer, keyAgreement: ka,
                                    channel: channel, collector: collector, config: config)
        await client.setEventHandler(collector.handler)
        await client.start()

        let nonce = TransportFixtures.nonceB64()
        await handshake(channel, client: client, nonceB64: nonce)

        // resume + request_snapshot were sent after auth_ok.
        _ = await waitUntil {
            await channel.sentFrames().contains { if case .resume = $0 { return true }; return false }
        }
        let sent = await channel.sentFrames()

        // Frame order: hello, auth_response, resume, envelope(request_snapshot).
        #expect({ if case .hello = sent.first { return true }; return false }())
        let authResp = try #require(sent.first { if case .authResponse = $0 { return true }; return false })
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        #expect(verifySignature(authResp, identityPublicKeyB64: identity.publicKeyBase64, nonceB64: nonce))

        let resume = try #require(sent.first { if case .resume = $0 { return true }; return false })
        if case let .resume(pairingId, fromSeq) = resume {
            #expect(pairingId.rawValue == "pair_test_1")
            #expect(fromSeq == 0)
        }

        // The request_snapshot rides an outbound envelope at gapless seq 1.
        let snapEnvelope = try #require(sent.compactMap { frame -> Wire.EncryptedEnvelope? in
            if case let .envelope(e) = frame { return e }; return nil
        }.first)
        #expect(snapEnvelope.seq == 1)
        #expect(snapEnvelope.sender == .phone)
        let decoded = try peer.openCommand(snapEnvelope)
        if case .requestSnapshot = decoded.body { } else { Issue.record("expected request_snapshot") }

        // Desktop replies with a snapshot at inbound seq 1 → folded + acked.
        let snapshot = Wire.StateSnapshot(serverTimeMs: 1, projects: [])
        try await channel.push(peer.envelopeFrame(.snapshot(snapshot), seq: 1))
        _ = await waitUntil { collector.messages.contains { if case .snapshot = $0 { return true }; return false } }

        #expect(collector.messages.contains { if case .snapshot = $0 { return true }; return false })
        _ = await waitUntil {
            await channel.sentFrames().contains { if case let .ack(_, cursor) = $0 { return cursor == 1 }; return false }
        }
        let acks = await channel.sentFrames().compactMap { frame -> UInt64? in
            if case let .ack(_, cursor) = frame { return cursor }; return nil
        }
        #expect(acks.contains(1))

        await client.stop()
    }

    // MARK: - Resume cursor

    @Test func resumeUsesPersistedInboundCursor() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain, lastReceivedSeq: 7)
        let channel = ScriptedChannel()
        let collector = EventCollector()
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        config.requestSnapshotOnResume = false
        let client = try makeClient(keychain: keychain, peer: peer, keyAgreement: ka,
                                    channel: channel, collector: collector, config: config)
        await client.setEventHandler(collector.handler)
        await client.start()
        await handshake(channel, client: client, nonceB64: TransportFixtures.nonceB64())

        _ = await waitUntil {
            await channel.sentFrames().contains { if case .resume = $0 { return true }; return false }
        }
        let resume = try #require(await channel.sentFrames().first { if case .resume = $0 { return true }; return false })
        if case let .resume(_, fromSeq) = resume { #expect(fromSeq == 7) }
        await client.stop()
    }

    // MARK: - Dedup

    @Test func inboundDedupIgnoresReplayedSeq() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let collector = EventCollector()
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        config.requestSnapshotOnResume = false
        let client = try makeClient(keychain: keychain, peer: peer, keyAgreement: ka,
                                    channel: channel, collector: collector, config: config)
        await client.setEventHandler(collector.handler)
        await client.start()
        await handshake(channel, client: client, nonceB64: TransportFixtures.nonceB64())

        let event = Wire.AgentEvent(
            eventId: Wire.EventId("evt1"),
            kind: .needsInput(preview: "?"),
            deepLink: Wire.DeepLink(projectId: Wire.ProjectId("p"), sessionId: Wire.SessionId("s"), itemId: nil),
            occurredAtMs: 1,
            title: "t"
        )
        // First delivery at seq 5 is accepted; two replays are ignored.
        try await channel.push(peer.envelopeFrame(.event(event), seq: 5))
        _ = await waitUntil { collector.messages.count == 1 }
        try await channel.push(peer.envelopeFrame(.event(event), seq: 5))
        try await channel.push(peer.envelopeFrame(.event(event), seq: 3))
        // Give the client a beat to (not) process the replays.
        try? await Task.sleep(for: .milliseconds(150))

        #expect(collector.messages.count == 1)
        let acks = await channel.sentFrames().compactMap { frame -> UInt64? in
            if case let .ack(_, cursor) = frame { return cursor }; return nil
        }
        #expect(acks == [5]) // exactly one ack, for the one accepted envelope
        await client.stop()
    }

    // MARK: - Delivery honesty

    @Test func deliveryTimesOutToFailedWithoutAck() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let collector = EventCollector()
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        config.requestSnapshotOnResume = false
        config.commandTimeout = .milliseconds(150)
        let client = try makeClient(keychain: keychain, peer: peer, keyAgreement: ka,
                                    channel: channel, collector: collector, config: config)
        await client.setEventHandler(collector.handler)
        await client.start()
        await handshake(channel, client: client, nonceB64: TransportFixtures.nonceB64())

        let id = Wire.CommandId("cmd_reply_1")
        let command = Wire.PhoneCommand(commandId: id, issuedAtMs: 1, body: .reply(sessionId: Wire.SessionId("s"), text: "hi"))
        await client.send(command)

        _ = await waitUntil { collector.deliveries(for: id).contains(.failed(reason: "timed out")) }
        let states = collector.deliveries(for: id)
        #expect(states.first == .sending)
        #expect(states.contains(.failed(reason: "timed out")))

        // The outbound envelope was sealed at gapless seq 1 and decodes back.
        let env = try #require(await channel.sentFrames().compactMap { frame -> Wire.EncryptedEnvelope? in
            if case let .envelope(e) = frame { return e }; return nil
        }.first)
        #expect(env.seq == 1)
        let decoded = try peer.openCommand(env)
        #expect(decoded.commandId == id)
        await client.stop()
    }

    @Test func deliveryResolvesToDeliveredOnCommandAck() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let collector = EventCollector()
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        config.requestSnapshotOnResume = false
        config.commandTimeout = .seconds(30)
        let client = try makeClient(keychain: keychain, peer: peer, keyAgreement: ka,
                                    channel: channel, collector: collector, config: config)
        await client.setEventHandler(collector.handler)
        await client.start()
        await handshake(channel, client: client, nonceB64: TransportFixtures.nonceB64())

        let id = Wire.CommandId("cmd_reply_2")
        let command = Wire.PhoneCommand(commandId: id, issuedAtMs: 1, body: .reply(sessionId: Wire.SessionId("s"), text: "go"))
        await client.send(command)
        _ = await waitUntil { collector.deliveries(for: id).contains(.sending) }

        let ack = Wire.CommandAck(commandId: id, outcome: .applied, message: nil)
        try await channel.push(peer.envelopeFrame(.commandAck(ack), seq: 1))

        _ = await waitUntil { collector.deliveries(for: id).contains(.delivered(.applied)) }
        #expect(collector.deliveries(for: id).contains(.delivered(.applied)))
        await client.stop()
    }

    // MARK: - Relay-restart seq recovery (remote-control-bbf)

    /// The relay lost its per-pairing seq state (restart/redeploy) and the desktop
    /// restarts its outbound stream from seq 1. The phone already holds a higher
    /// receive cursor, but must accept the reset instead of dropping seq 1 as a
    /// duplicate — otherwise the recovered agent feed never reaches the screen.
    @Test func acceptsDesktopStreamResetAfterRelayRestart() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain, lastReceivedSeq: 5)
        let channel = ScriptedChannel()
        let collector = EventCollector()
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        config.requestSnapshotOnResume = false
        let client = try makeClient(keychain: keychain, peer: peer, keyAgreement: ka,
                                    channel: channel, collector: collector, config: config)
        await client.setEventHandler(collector.handler)
        await client.start()
        await handshake(channel, client: client, nonceB64: TransportFixtures.nonceB64())

        // Desktop restarts its stream from seq 1 while our cursor is 5.
        let snapshot = Wire.StateSnapshot(serverTimeMs: 1, projects: [])
        try await channel.push(peer.envelopeFrame(.snapshot(snapshot), seq: 1))
        _ = await waitUntil { collector.messages.contains { if case .snapshot = $0 { return true }; return false } }
        #expect(collector.messages.contains { if case .snapshot = $0 { return true }; return false })

        // The following seq 2 continues normally — proving the cursor reset to 1.
        let event = Wire.AgentEvent(
            eventId: Wire.EventId("e1"),
            kind: .finished(summary: "done", filesChanged: 0, readyToPush: false),
            deepLink: Wire.DeepLink(projectId: Wire.ProjectId("p"), sessionId: Wire.SessionId("s"), itemId: nil),
            occurredAtMs: 1,
            title: "t"
        )
        try await channel.push(peer.envelopeFrame(.event(event), seq: 2))
        _ = await waitUntil { collector.messages.count == 2 }
        #expect(collector.messages.count == 2)

        // It acked the reset stream at cursor 1.
        let acks = await channel.sentFrames().compactMap { f -> UInt64? in
            if case let .ack(_, c) = f { return c }; return nil
        }
        #expect(acks.contains(1))
        await client.stop()
    }

    /// After a relay restart the phone's own outbound command seq is ahead of the
    /// fresh relay, which rejects it with `seq_violation`. The client must NOT
    /// tear the link down; it re-syncs its outbound cursor so the next command
    /// restarts at seq 1 (which a fresh relay accepts).
    @Test func resyncsOutboundCursorOnSeqViolation() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain, lastSentSeq: 5)
        let channel = ScriptedChannel()
        let collector = EventCollector()
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        config.requestSnapshotOnResume = false
        config.commandTimeout = .seconds(30)
        let recordStore = PairingRecordStore(store: keychain)
        try recordStore.save(peer.record)
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        let client = TransportClient(
            identity: identity,
            keyAgreement: ka,
            recordStore: recordStore,
            connector: ScriptedConnector(channel: channel),
            clientInfo: Wire.ClientInfo(appVersion: "test", platform: "ios", osVersion: nil),
            config: config,
            jitter: { 0 },
            now: { 1_752_000_100_000 }
        )
        await client.setEventHandler(collector.handler)
        await client.start()
        await handshake(channel, client: client, nonceB64: TransportFixtures.nonceB64())

        // First command goes out at seq 6 (persisted lastSentSeq 5). The fresh
        // relay rejects it as non-monotonic.
        let id1 = Wire.CommandId("cmd_1")
        await client.send(Wire.PhoneCommand(commandId: id1, issuedAtMs: 1,
                                            body: .reply(sessionId: Wire.SessionId("s"), text: "a")))
        _ = await waitUntil {
            await channel.sentFrames().contains {
                if case let .envelope(e) = $0 { return e.sender == .phone && e.seq == 6 }; return false
            }
        }
        try await channel.push(.error(code: .seqViolation,
                                      message: "envelope seq is not gapless/monotonic",
                                      pairingId: Wire.PairingId("pair_test_1")))

        // The cursor re-syncs to 0 (recovery), and the link stays up.
        _ = await waitUntil { (try? recordStore.load())?.lastSentSeq == 0 }
        #expect((try? recordStore.load())?.lastSentSeq == 0)
        #expect(await client.currentLinkState() != .disconnected)

        // The next command restarts the outbound stream at seq 1.
        let id2 = Wire.CommandId("cmd_2")
        await client.send(Wire.PhoneCommand(commandId: id2, issuedAtMs: 2,
                                            body: .reply(sessionId: Wire.SessionId("s"), text: "b")))
        _ = await waitUntil {
            await channel.sentFrames().contains {
                if case let .envelope(e) = $0 { return e.sender == .phone && e.seq == 1 }; return false
            }
        }
        let restarted = await channel.sentFrames().contains {
            if case let .envelope(e) = $0 { return e.sender == .phone && e.seq == 1 }; return false
        }
        #expect(restarted)
        await client.stop()
    }

    // MARK: - No pairing → stays disconnected

    @Test func withNoPairingRecordStaysDisconnected() async throws {
        let keychain = InMemoryKeychainStore()
        let ka = try KeyAgreementKeys.loadOrCreate(store: keychain)
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        let channel = ScriptedChannel()
        let collector = EventCollector()
        let client = TransportClient(
            identity: identity,
            keyAgreement: ka,
            recordStore: PairingRecordStore(store: keychain),
            connector: ScriptedConnector(channel: channel),
            clientInfo: Wire.ClientInfo(appVersion: "test", platform: "ios", osVersion: nil),
            jitter: { 0 }
        )
        await client.setEventHandler(collector.handler)
        await client.start()
        try? await Task.sleep(for: .milliseconds(100))
        let state = await client.currentLinkState()
        #expect(state == .disconnected)
        let sent = await channel.sentFrames()
        #expect(sent.isEmpty)
        await client.stop()
    }

    // MARK: - Machine name (REMOTE_PROTOCOL §5.7, remote-control-b8d.9)

    @Test func machineNameFrameEmitsSanitizedEventForOurPairing() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let collector = EventCollector()
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        let client = try makeClient(keychain: keychain, peer: peer, keyAgreement: ka,
                                    channel: channel, collector: collector, config: config)
        await client.setEventHandler(collector.handler)
        await client.start()
        let nonce = TransportFixtures.nonceB64()
        await handshake(channel, client: client, nonceB64: nonce)

        await channel.push(.machineName(
            pairingId: Wire.PairingId("pair_test_1"),
            machineName: "  Ruud's MacBook Pro  "))
        _ = await waitUntil { collector.machineNames.contains("Ruud's MacBook Pro") }

        #expect(collector.machineNames == ["Ruud's MacBook Pro"])
        await client.stop()
    }

    @Test func machineNameFrameForADifferentPairingIsIgnored() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let collector = EventCollector()
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        let client = try makeClient(keychain: keychain, peer: peer, keyAgreement: ka,
                                    channel: channel, collector: collector, config: config)
        await client.setEventHandler(collector.handler)
        await client.start()
        let nonce = TransportFixtures.nonceB64()
        await handshake(channel, client: client, nonceB64: nonce)

        // This client is bound to "pair_test_1" (via its loaded record) — a
        // frame announced for some OTHER pairing must never leak in.
        await channel.push(.machineName(pairingId: Wire.PairingId("some_other_pairing"), machineName: "Not Ours"))
        // Follow with a pong carrying a NON-zero round trip (500ms earlier
        // than the fixed `now: { 1_752_000_100_000 }` clock) so its distinct
        // `.connected(latencyMs: 500)` state is a reliable "the frame ahead
        // of me in this single actor's serial receive loop has already been
        // handled" signal, without depending on wall-clock sleeps.
        await channel.push(.pong(clientTimeMs: 1_752_000_099_500, serverTimeMs: 1))
        _ = await waitUntil { await client.currentLinkState() == .connected(latencyMs: 500) }

        #expect(collector.machineNames.isEmpty)
        await client.stop()
    }

    @Test func machineNameFrameIsBoundedToSixtyFourCharacters() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let collector = EventCollector()
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        let client = try makeClient(keychain: keychain, peer: peer, keyAgreement: ka,
                                    channel: channel, collector: collector, config: config)
        await client.setEventHandler(collector.handler)
        await client.start()
        let nonce = TransportFixtures.nonceB64()
        await handshake(channel, client: client, nonceB64: nonce)

        let longName = String(repeating: "x", count: 100)
        await channel.push(.machineName(pairingId: Wire.PairingId("pair_test_1"), machineName: longName))
        _ = await waitUntil { !collector.machineNames.isEmpty }

        #expect(collector.machineNames.first?.count == 64)
        #expect(collector.machineNames.first == String(repeating: "x", count: 64))
        await client.stop()
    }

    @Test func machineNameFrameThatIsAllWhitespaceIsDropped() async throws {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let channel = ScriptedChannel()
        let collector = EventCollector()
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        let client = try makeClient(keychain: keychain, peer: peer, keyAgreement: ka,
                                    channel: channel, collector: collector, config: config)
        await client.setEventHandler(collector.handler)
        await client.start()
        let nonce = TransportFixtures.nonceB64()
        await handshake(channel, client: client, nonceB64: nonce)

        await channel.push(.machineName(pairingId: Wire.PairingId("pair_test_1"), machineName: "   "))
        await channel.push(.pong(clientTimeMs: 1_752_000_099_500, serverTimeMs: 1))
        _ = await waitUntil { await client.currentLinkState() == .connected(latencyMs: 500) }

        #expect(collector.machineNames.isEmpty)
        await client.stop()
    }
}
