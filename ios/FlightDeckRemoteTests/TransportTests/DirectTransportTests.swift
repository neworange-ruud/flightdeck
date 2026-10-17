//
//  DirectTransportTests.swift
//  FlightDeckRemoteTests
//
//  The direct link to the Mac over Tailscale (REMOTE_PROTOCOL §5.10,
//  remote-control-gz0d), driven against a fake direct endpoint — the same
//  scripted-socket double the relay transport tests use, playing the Mac in
//  the relay's role:
//   - connect + auth: the endpoint dialed is `ws://host:port/remote/v1`, no
//     relay password leaks to the Mac, and the auth signature is over the
//     domain-separated `"flightdeck-remote-direct-v1:" ‖ nonce` (and is NOT a
//     valid relay signature over the bare nonce);
//   - resume from the persisted cursor, decrypt, ack;
//   - reconnect after a drop: resume from the advanced cursor, with a
//     replayed duplicate deduped (no double delivery);
//   - `auth_failed`: session ends, nothing is sent blind, backoff (no tight
//     loop);
//   - no push-token frames ever go to the Mac (it would answer `bad_frame`);
//   - transport selection from the pairing record (pure + coordinator);
//   - the relay control-plane client of a direct pairing (push + revoke only);
//   - push-only relays for relay-mode pairings (§5.9);
//   - `PairingRecord` backward compatibility (old JSON = relay mode).
//

import Testing
import Foundation
import CryptoKit
@testable import FlightDeckRemote

private let endpoint = Wire.DirectEndpoint(host: "ruuds-mbp.tail1234.ts.net", port: 47800)
private let pairingId = "pair_test_1"

/// Push the Mac's side of the handshake and wait for `.connected`.
private func macHandshake(_ channel: ScriptedChannel, client: TransportClient,
                          nonceB64: String = TransportFixtures.nonceB64()) async {
    await channel.push(.helloOk(protocolVersion: 4, serverTimeMs: 1, connectionId: "direct"))
    await channel.push(.authChallenge(nonce: nonceB64, serverTimeMs: 1))
    await channel.push(.authOk(pairingIds: [Wire.PairingId(pairingId)]))
    _ = await waitUntil { if case .connected = await client.currentLinkState() { return true }; return false }
}

private func sent<T>(_ channel: ScriptedChannel, _ pick: (Wire.RelayFrame) -> T?) async -> [T] {
    await channel.sentFrames().compactMap(pick)
}

private func resumes(_ channel: ScriptedChannel) async -> [UInt64] {
    await sent(channel) { if case let .resume(_, from) = $0 { return from }; return nil }
}

private func acks(_ channel: ScriptedChannel) async -> [UInt64] {
    await sent(channel) { if case let .ack(_, cursor) = $0 { return cursor }; return nil }
}

private func phoneEnvelopes(_ channel: ScriptedChannel) async -> [Wire.EncryptedEnvelope] {
    await sent(channel) { if case let .envelope(e) = $0, e.sender == .phone { return e }; return nil }
}

private func pushTokenFrames(_ channel: ScriptedChannel) async -> Int {
    await sent(channel) { frame -> Bool? in
        switch frame {
        case .registerPushToken, .unregisterPushToken: return true
        default: return nil
        }
    }.count
}

@Suite struct DirectTransportTests {

    // MARK: - Builders

    private struct Rig {
        let client: TransportClient
        let connector: ScriptedConnector
        let collector: EventCollector
        let recordStore: PairingRecordStore
        let peer: DesktopPeer
        let identity: DeviceIdentity
    }

    private func makeRig(
        directEndpoint: Wire.DirectEndpoint? = endpoint,
        plane: TransportPlane = .data,
        lastReceivedSeq: UInt64 = 0,
        requestSnapshot: Bool = true,
        configure: (inout TransportClient.Config) -> Void = { _ in }
    ) throws -> Rig {
        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(
            keychain: keychain, pairingId: pairingId,
            relayURL: "wss://relay.example/ws",
            lastReceivedSeq: lastReceivedSeq,
            directEndpoint: directEndpoint)
        let recordStore = PairingRecordStore(store: keychain)
        try recordStore.save(peer.record)
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        config.requestSnapshotOnResume = requestSnapshot
        configure(&config)
        let connector = ScriptedConnector(factory: { ScriptedChannel() })
        let client = TransportClient(
            identity: identity,
            keyAgreement: ka,
            recordStore: recordStore,
            pairingId: pairingId,
            plane: plane,
            connector: connector,
            clientInfo: Wire.ClientInfo(appVersion: "test", platform: "ios", osVersion: nil),
            relayPassword: { "relay-secret" },
            config: config,
            jitter: { 0 },
            now: { 1_752_000_100_000 })
        let collector = EventCollector()
        return Rig(client: client, connector: connector, collector: collector,
                   recordStore: recordStore, peer: peer, identity: identity)
    }

    private func start(_ rig: Rig) async -> ScriptedChannel {
        await rig.client.setEventHandler(rig.collector.handler)
        await rig.client.start()
        _ = await waitUntil { rig.connector.channels.count >= 1 }
        return rig.connector.channels[0]
    }

    private func verify(_ signatureB64: String, publicKeyB64: String, message: Data) -> Bool {
        guard let pub = Data(base64Encoded: publicKeyB64),
              let sig = Data(base64Encoded: signatureB64),
              let key = try? P256.Signing.PublicKey(x963Representation: pub),
              let ecdsa = try? P256.Signing.ECDSASignature(rawRepresentation: sig)
        else { return false }
        return key.isValidSignature(ecdsa, for: message)
    }

    // MARK: - Connect + auth

    @Test func directConnectDialsTheMacAndSignsTheDomainSeparatedNonce() async throws {
        let rig = try makeRig()
        let channel = await start(rig)
        let nonce = TransportFixtures.nonceB64()
        await macHandshake(channel, client: rig.client, nonceB64: nonce)

        // The Mac's direct-link endpoint, not the relay.
        #expect(rig.connector.urls.first?.absoluteString == "ws://ruuds-mbp.tail1234.ts.net:47800/remote/v1")
        #expect(await rig.client.currentLink() == .direct(endpoint))

        // hello carries no relay password to the Mac (§5.10: a relay secret).
        let frames = await channel.sentFrames()
        guard case let .hello(version, role, _, _, password) = try #require(frames.first) else {
            Issue.record("first frame must be hello"); return
        }
        #expect(version == 4)
        #expect(role == .phone)
        #expect(password == nil)

        // auth_response: signature over "flightdeck-remote-direct-v1:" ‖ nonce.
        let auth = try #require(frames.first { if case .authResponse = $0 { return true }; return false })
        guard case let .authResponse(deviceId, signature, pairingIds, machineName) = auth else { return }
        #expect(deviceId.rawValue == rig.identity.deviceId)
        #expect(pairingIds == [Wire.PairingId(pairingId)])
        #expect(machineName == nil)
        let nonceBytes = try #require(Data(base64Encoded: nonce))
        var expected = Data("flightdeck-remote-direct-v1:".utf8)
        expected.append(nonceBytes)
        #expect(Wire.directAuthMessage(nonce: nonceBytes) == expected)
        #expect(verify(signature, publicKeyB64: rig.identity.publicKeyBase64, message: expected),
                "direct auth must sign the domain-separated message")
        #expect(!verify(signature, publicKeyB64: rig.identity.publicKeyBase64, message: nonceBytes),
                "a direct-link signature must not double as a relay signature over the bare nonce")

        await rig.client.stop()
    }

    /// The relay path is unchanged: it still signs the bare nonce.
    @Test func relayModeStillSignsTheBareNonce() async throws {
        let rig = try makeRig(directEndpoint: nil)
        let channel = await start(rig)
        let nonce = TransportFixtures.nonceB64()
        await macHandshake(channel, client: rig.client, nonceB64: nonce)

        #expect(rig.connector.urls.first?.absoluteString == "wss://relay.example/ws")
        let frames = await channel.sentFrames()
        guard case let .hello(version, _, _, _, password) = try #require(frames.first) else { return }
        // The relay forwards `direct_endpoint` only to phones that negotiated
        // v4 (`DIRECT_LINK_MIN_VERSION`), so the relay hello must advertise 4.
        #expect(version == 4)
        #expect(password == "relay-secret")
        let auth = try #require(frames.first { if case .authResponse = $0 { return true }; return false })
        guard case let .authResponse(_, signature, _, _) = auth else { return }
        #expect(verify(signature, publicKeyB64: rig.identity.publicKeyBase64,
                       message: try #require(Data(base64Encoded: nonce))))
        await rig.client.stop()
    }

    // MARK: - Resume, receive, ack

    @Test func directResumesFromThePersistedCursorAndAcksNewEnvelopes() async throws {
        let rig = try makeRig(lastReceivedSeq: 7)
        let channel = await start(rig)
        await macHandshake(channel, client: rig.client)

        _ = await waitUntil { await !resumes(channel).isEmpty }
        #expect(await resumes(channel) == [7])
        // The post-auth request_snapshot rides a sealed envelope the Mac opens.
        _ = await waitUntil { await !phoneEnvelopes(channel).isEmpty }
        let snapshotRequest = try #require(await phoneEnvelopes(channel).first)
        let command = try rig.peer.openCommand(snapshotRequest)
        if case .requestSnapshot = command.body {} else { Issue.record("expected request_snapshot") }

        // Mac replays seq 8 → decrypted, folded, acked, cursor persisted.
        try await channel.push(rig.peer.envelopeFrame(.snapshot(Wire.StateSnapshot(serverTimeMs: 8, projects: [])), seq: 8))
        _ = await waitUntil { await acks(channel).contains(8) }
        #expect(await acks(channel) == [8])
        #expect(rig.collector.messages.count == 1)
        #expect(try rig.recordStore.load(pairingId: pairingId)?.lastReceivedSeq == 8)

        await rig.client.stop()
    }

    // MARK: - Reconnect after a drop

    @Test func directReconnectAfterDropResumesFromCursorWithoutDuplicates() async throws {
        // A session that authenticated counts as good immediately, so the
        // automatic reconnect after the drop waits the 1s floor, not 2s.
        let rig = try makeRig(requestSnapshot: false) { $0.minimumGoodSession = .zero }
        let first = await start(rig)
        await macHandshake(first, client: rig.client)

        func snapshot(_ seq: UInt64) throws -> Wire.RelayFrame {
            try rig.peer.envelopeFrame(.snapshot(Wire.StateSnapshot(serverTimeMs: Int64(seq), projects: [])), seq: seq)
        }
        try await first.push(snapshot(1))
        try await first.push(snapshot(2))
        _ = await waitUntil { await acks(first).contains(2) }

        // The link drops (Tailscale blip / Mac asleep): the socket dies.
        await first.close()
        _ = await waitUntil { rig.collector.links.contains(.disconnected) }

        // The supervisor reconnects on its own, to the same direct endpoint.
        _ = await waitUntil(timeout: .seconds(5)) { rig.connector.channels.count >= 2 }
        let second = try #require(rig.connector.channels.dropFirst().first)
        await macHandshake(second, client: rig.client)
        #expect(rig.connector.urls.count == 2)
        #expect(Set(rig.connector.urls.map(\.absoluteString)) == ["ws://ruuds-mbp.tail1234.ts.net:47800/remote/v1"])

        // It resumes from the advanced, persisted cursor.
        _ = await waitUntil { await !resumes(second).isEmpty }
        #expect(await resumes(second) == [2])

        // The Mac's outbox replays seq 2 (ack raced the drop) and then seq 3.
        try await second.push(snapshot(2))
        try await second.push(snapshot(3))
        _ = await waitUntil { await acks(second).contains(3) }

        // Exactly one delivery per seq — the replayed 2 was deduped, not re-acked.
        let delivered = rig.collector.messages.compactMap { message -> Int64? in
            if case let .snapshot(s) = message { return s.serverTimeMs }; return nil
        }
        #expect(delivered == [1, 2, 3])
        #expect(await acks(second) == [3])
        #expect(try rig.recordStore.load(pairingId: pairingId)?.lastReceivedSeq == 3)

        await rig.client.stop()
    }

    // MARK: - auth_failed

    @Test func directAuthFailedEndsTheSessionWithoutSendingBlindAndBacksOff() async throws {
        let rig = try makeRig()
        let first = await start(rig)
        await first.push(.helloOk(protocolVersion: 4, serverTimeMs: 1, connectionId: "direct"))
        await first.push(.authChallenge(nonce: TransportFixtures.nonceB64(), serverTimeMs: 1))
        _ = await waitUntil {
            await first.sentFrames().contains { if case .authResponse = $0 { return true }; return false }
        }
        // The Mac does not know this device/pairing (e.g. revoked on the Mac).
        await first.push(.error(code: .authFailed, message: "unknown device", pairingId: nil, expectedSeq: nil))

        _ = await waitUntil { await first.isClosed() }
        #expect(await first.isClosed())
        #expect(!rig.collector.links.contains { if case .connected = $0 { return true }; return false })
        // Never sent blind: no resume, no envelope, no push frames.
        #expect(await resumes(first).isEmpty)
        #expect(await phoneEnvelopes(first).isEmpty)
        #expect(await pushTokenFrames(first) == 0)

        // A command now fails honestly instead of being queued anywhere.
        let commandId = Wire.CommandId("cmd_after_auth_failed")
        await rig.client.send(Wire.PhoneCommand(commandId: commandId, issuedAtMs: 1, body: .requestSnapshot(projectId: nil)))
        #expect(rig.collector.deliveries(for: commandId) == [.failed(reason: "not connected")])

        // Backoff, not a tight loop: no immediate second connect...
        try await Task.sleep(for: .milliseconds(300))
        #expect(rig.connector.channels.count == 1)
        // ...but it does retry later (an unknown-device state can be fixed on
        // the Mac without touching the phone).
        _ = await waitUntil(timeout: .seconds(5)) { rig.connector.channels.count >= 2 }
        #expect(rig.connector.channels.count >= 2)

        await rig.client.stop()
    }

    // MARK: - Push tokens never go to the Mac

    @Test func directLinkNeverCarriesPushTokenFrames() async throws {
        let rig = try makeRig(requestSnapshot: false)
        let channel = await start(rig)
        await rig.client.applyPush(token: ("apns-token", .sandbox), muted: false)
        await macHandshake(channel, client: rig.client)
        await rig.client.registerPushToken("apns-token-2", environment: .sandbox)
        await rig.client.setPushMuted(true)
        try await Task.sleep(for: .milliseconds(100))
        #expect(await pushTokenFrames(channel) == 0)
        await rig.client.stop()
    }

    /// `revoke` also goes over the direct link (the courtesy copy §5.10 allows).
    @Test func directLinkCarriesRevokeWhenLive() async throws {
        let rig = try makeRig(requestSnapshot: false)
        let channel = await start(rig)
        await macHandshake(channel, client: rig.client)
        #expect(await rig.client.revokePairing())
        #expect(await channel.sentFrames().contains { if case .revoke = $0 { return true }; return false })
        await rig.client.stop()
    }

    // MARK: - Relay control plane of a direct pairing

    @Test func relayControlClientRegistersPushButCarriesNoDataAndReportsNoLinkState() async throws {
        let rig = try makeRig(plane: .relayControl)
        let channel = await start(rig)
        await rig.client.applyPush(token: ("apns-token", .production), muted: false)
        await channel.push(.helloOk(protocolVersion: 4, serverTimeMs: 1, connectionId: "relay", relayMode: .pushOnly))
        let nonce = TransportFixtures.nonceB64()
        await channel.push(.authChallenge(nonce: nonce, serverTimeMs: 1))
        await channel.push(.authOk(pairingIds: [Wire.PairingId(pairingId)]))
        _ = await waitUntil { await pushTokenFrames(channel) >= 1 }

        // It dials the relay, presents the relay password, signs the bare nonce.
        #expect(rig.connector.urls.first?.absoluteString == "wss://relay.example/ws")
        #expect(await rig.client.currentLink() == .relayControl)
        let frames = await channel.sentFrames()
        guard case let .hello(_, _, _, _, password) = try #require(frames.first) else { return }
        #expect(password == "relay-secret")
        let auth = try #require(frames.first { if case .authResponse = $0 { return true }; return false })
        guard case let .authResponse(_, signature, _, _) = auth else { return }
        #expect(verify(signature, publicKeyB64: rig.identity.publicKeyBase64,
                       message: try #require(Data(base64Encoded: nonce))))

        // Push token registered on the relay...
        #expect(await pushTokenFrames(channel) == 1)
        // ...but no data plane: no resume, no snapshot request, no envelopes.
        await channel.push(.peerPresence(pairingId: Wire.PairingId(pairingId), peer: .desktop, state: .connected, atMs: 1))
        try await channel.push(rig.peer.envelopeFrame(.snapshot(Wire.StateSnapshot(serverTimeMs: 1, projects: [])), seq: 1))
        try await Task.sleep(for: .milliseconds(100))
        #expect(await resumes(channel).isEmpty)
        #expect(await phoneEnvelopes(channel).isEmpty)
        #expect(await acks(channel).isEmpty)
        #expect(try rig.recordStore.load(pairingId: pairingId)?.lastReceivedSeq == 0)

        // It never speaks for the data plane's link/presence/messages.
        #expect(rig.collector.links.isEmpty)
        #expect(rig.collector.messages.isEmpty)
        #expect(!rig.collector.events.contains { if case .presence = $0 { return true }; return false })

        // A command handed to it is refused (it has no data plane).
        let commandId = Wire.CommandId("cmd_control")
        await rig.client.send(Wire.PhoneCommand(commandId: commandId, issuedAtMs: 1, body: .requestSnapshot(projectId: nil)))
        #expect(await phoneEnvelopes(channel).isEmpty)

        // But it does surface the relay's revocation notice and machine name.
        await channel.push(.machineName(pairingId: Wire.PairingId(pairingId), machineName: "Ruud's Mac"))
        await channel.push(.pairingRevoked(pairingId: Wire.PairingId(pairingId)))
        _ = await waitUntil { rig.collector.events.contains { if case .pairingRevoked = $0 { return true }; return false } }
        #expect(rig.collector.machineNames == ["Ruud's Mac"])

        await rig.client.stop()
    }

    // MARK: - Push-only relay for a relay-mode pairing (§5.9)

    @Test func pushOnlyRelayAnnouncedInHelloOkCarriesNoDataAndDoesNotLoop() async throws {
        let rig = try makeRig(directEndpoint: nil)
        let channel = await start(rig)
        await rig.client.applyPush(token: ("apns-token", .sandbox), muted: false)
        await channel.push(.helloOk(protocolVersion: 4, serverTimeMs: 1, connectionId: "relay", relayMode: .pushOnly))
        await channel.push(.authChallenge(nonce: TransportFixtures.nonceB64(), serverTimeMs: 1))
        await channel.push(.authOk(pairingIds: [Wire.PairingId(pairingId)]))
        _ = await waitUntil { await pushTokenFrames(channel) >= 1 }

        #expect(await rig.client.relayIsPushOnly())
        _ = await waitUntil { rig.collector.events.contains { if case .relayPushOnly = $0 { return true }; return false } }
        // Pushes still register; the data plane stays silent and reported down.
        #expect(await pushTokenFrames(channel) == 1)
        #expect(await resumes(channel).isEmpty)
        #expect(await phoneEnvelopes(channel).isEmpty)
        #expect(!rig.collector.links.contains { if case .connected = $0 { return true }; return false })
        #expect(await rig.client.currentLinkState() == .disconnected)

        // Commands fail honestly with the push-only reason.
        let commandId = Wire.CommandId("cmd_push_only")
        await rig.client.send(Wire.PhoneCommand(commandId: commandId, issuedAtMs: 1, body: .requestSnapshot(projectId: nil)))
        #expect(rig.collector.deliveries(for: commandId) == [.failed(reason: TransportClient.pushOnlyReason)])

        // The session is kept (useful for push), not torn down into a loop.
        try await Task.sleep(for: .milliseconds(200))
        #expect(!(await channel.isClosed()))
        #expect(rig.connector.channels.count == 1)
        await rig.client.stop()
    }

    @Test func relayModePushOnlyErrorStopsDataTrafficWithoutDroppingTheSession() async throws {
        let rig = try makeRig(directEndpoint: nil, requestSnapshot: false)
        let channel = await start(rig)
        await macHandshake(channel, client: rig.client)
        _ = await waitUntil { await !resumes(channel).isEmpty }

        // A command is in flight when the relay refuses data frames.
        let inFlight = Wire.CommandId("cmd_in_flight")
        await rig.client.send(Wire.PhoneCommand(commandId: inFlight, issuedAtMs: 1, body: .requestSnapshot(projectId: nil)))
        #expect(rig.collector.deliveries(for: inFlight) == [.sending])
        await channel.push(.error(code: .relayModePushOnly, message: "push only",
                                  pairingId: Wire.PairingId(pairingId), expectedSeq: nil))
        _ = await waitUntil { await rig.client.relayIsPushOnly() }
        _ = await waitUntil { rig.collector.deliveries(for: inFlight).count >= 2 }
        #expect(rig.collector.deliveries(for: inFlight).last == .failed(reason: TransportClient.pushOnlyReason))
        #expect(await rig.client.currentLinkState() == .disconnected)

        let envelopesBefore = await phoneEnvelopes(channel).count
        let next = Wire.CommandId("cmd_next")
        await rig.client.send(Wire.PhoneCommand(commandId: next, issuedAtMs: 1, body: .requestSnapshot(projectId: nil)))
        #expect(await phoneEnvelopes(channel).count == envelopesBefore)
        #expect(!(await channel.isClosed()))
        await rig.client.stop()
    }

    // MARK: - Transport selection (pure)

    @Test func transportLinkIsSelectedFromThePairingRecord() throws {
        let relayRecord = PairingRecord(pairingId: "p", peerDeviceId: nil, peerKeyAgreementPublicKeyB64: "k",
                                        saltB64: "", relayURL: "wss://relay.example/ws")
        var directRecord = relayRecord
        directRecord.directEndpoint = endpoint

        #expect(relayRecord.transportMode == .relay)
        #expect(directRecord.transportMode == .direct)

        #expect(TransportLink.resolve(plane: .data, record: relayRecord) == .relay)
        #expect(TransportLink.resolve(plane: .data, record: directRecord) == .direct(endpoint))
        #expect(TransportLink.resolve(plane: .relayControl, record: directRecord) == .relayControl)

        #expect(TransportLink.relay.url(for: relayRecord)?.absoluteString == "wss://relay.example/ws")
        #expect(TransportLink.relayControl.url(for: directRecord)?.absoluteString == "wss://relay.example/ws")
        #expect(TransportLink.direct(endpoint).url(for: directRecord)?.absoluteString
                == "ws://ruuds-mbp.tail1234.ts.net:47800/remote/v1")

        #expect(TransportLink.direct(endpoint).carriesDataPlane)
        #expect(!TransportLink.direct(endpoint).carriesRelayControl)
        #expect(!TransportLink.relayControl.carriesDataPlane)
        #expect(TransportLink.relayControl.carriesRelayControl)
        #expect(TransportLink.relay.carriesDataPlane && TransportLink.relay.carriesRelayControl)
    }

    @Test func directEndpointURLMatchesTheRustShape() {
        #expect(Wire.DirectEndpoint(host: "100.101.102.103", port: 47800).wsURLString
                == "ws://100.101.102.103:47800/remote/v1")
        // IPv6 literal is bracketed (Rust `DirectEndpoint::ws_url`).
        #expect(Wire.DirectEndpoint(host: "fd7a:115c:a1e0::1", port: 47800).wsURLString
                == "ws://[fd7a:115c:a1e0::1]:47800/remote/v1")
        #expect(Wire.DirectEndpoint(host: "fd7a:115c:a1e0::1", port: 47800).wsURL != nil)
    }

    // MARK: - PairingRecord backward compatibility

    @Test func pairingRecordWithoutDirectEndpointDecodesAsRelayMode() throws {
        // The exact shape builds before v4 persisted (synthesized Codable,
        // default JSONEncoder: `pairedAt` as seconds since the reference date).
        let legacy = Data("""
        {"pairingId":"pair_old","peerDeviceId":"desk","peerKeyAgreementPublicKeyB64":"QUJD",
         "saltB64":"NDcyOQ==","relayURL":"wss://relay.flightdeckai.app/ws",
         "lastSentSeq":12,"lastReceivedSeq":34,"pairedAt":782000000}
        """.utf8)
        let record = try JSONDecoder().decode(PairingRecord.self, from: legacy)
        #expect(record.directEndpoint == nil)
        #expect(record.transportMode == .relay)
        #expect(record.lastSentSeq == 12 && record.lastReceivedSeq == 34)
        #expect(TransportLink.resolve(plane: .data, record: record) == .relay)
    }

    @Test func directPairingRecordRoundTripsThroughTheKeychainStore() throws {
        let store = PairingRecordStore(store: InMemoryKeychainStore())
        let record = PairingRecord(pairingId: "pair_direct", peerDeviceId: nil, peerKeyAgreementPublicKeyB64: "k",
                                   saltB64: "", relayURL: "wss://relay.example/ws", directEndpoint: endpoint)
        try store.save(record)
        let loaded = try #require(try store.load(pairingId: "pair_direct"))
        #expect(loaded == record)
        #expect(loaded.transportMode == .direct)
        #expect(loaded.directEndpoint == endpoint)
    }
}

// MARK: - Coordinator: per-pairing selection, control client, revoke

@MainActor
@Suite struct DirectTransportCoordinatorTests {

    private struct Harness {
        let coordinator: TransportCoordinator
        /// The relay pairing's data-plane relay socket.
        let relayData: ScriptedConnector
        /// The direct pairing's relay control-plane socket.
        let relayControl: ScriptedConnector
        /// The direct pairing's data-plane socket to the Mac.
        let directConnector: ScriptedConnector
    }

    /// Hands out preset connectors in `connectorFactory` call order. The
    /// coordinator calls it once per relay-bound client at handle creation,
    /// oldest instance first: the relay pairing's data client, then the direct
    /// pairing's control client.
    private final class ConnectorQueue: @unchecked Sendable {
        private let lock = NSLock()
        private var queue: [ScriptedConnector]
        init(_ connectors: [ScriptedConnector]) { queue = connectors }
        func next() -> any WebSocketConnecting {
            lock.lock(); defer { lock.unlock() }
            return queue.isEmpty ? ScriptedConnector(factory: { ScriptedChannel() }) : queue.removeFirst()
        }
    }

    /// One relay-mode pairing ("pair_relay") and one direct-mode pairing
    /// ("pair_direct"). Relay sockets and direct sockets come from separate
    /// scripted connectors so the test can see which transport each dialed.
    private func makeHarness() throws -> Harness {
        let keychain = InMemoryKeychainStore()
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        let keyAgreement = try KeyAgreementKeys.loadOrCreate(store: keychain)
        let recordStore = PairingRecordStore(store: keychain)
        let (relayPeer, _) = try TransportFixtures.makePeer(
            keychain: keychain, pairingId: "pair_relay", relayURL: "wss://relay.example/ws")
        let (directPeer, _) = try TransportFixtures.makePeer(
            keychain: keychain, pairingId: "pair_direct", relayURL: "wss://relay.example/ws",
            directEndpoint: endpoint)
        try recordStore.save(relayPeer.record)
        try recordStore.save(directPeer.record)

        let relayData = ScriptedConnector(factory: { ScriptedChannel() })
        let relayControl = ScriptedConnector(factory: { ScriptedChannel() })
        let relayQueue = ConnectorQueue([relayData, relayControl])
        let directConnector = ScriptedConnector(factory: { ScriptedChannel() })
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        config.requestSnapshotOnResume = false
        let coordinator = TransportCoordinator(
            identity: identity,
            keyAgreement: keyAgreement,
            recordStore: recordStore,
            connectorFactory: { relayQueue.next() },
            directConnectorFactory: { directConnector },
            clientConfig: config,
            now: { 1_752_000_100_000 })
        coordinator.installInitialInstances([
            PairedInstance(pairingId: "pair_relay", relayURL: URL(string: "wss://relay.example/ws")!,
                           pairedAt: Date(timeIntervalSince1970: 1)),
            PairedInstance(pairingId: "pair_direct", relayURL: URL(string: "wss://relay.example/ws")!,
                           pairedAt: Date(timeIntervalSince1970: 2)),
        ])
        return Harness(coordinator: coordinator, relayData: relayData, relayControl: relayControl,
                       directConnector: directConnector)
    }

    /// Wait until all three clients have dialed.
    private func waitForAllDialed(_ h: Harness) async {
        _ = await waitUntil {
            !h.relayData.channels.isEmpty && !h.relayControl.channels.isEmpty && !h.directConnector.channels.isEmpty
        }
    }

    private func handshake(_ channel: ScriptedChannel, pairingId: String) async {
        await channel.push(.helloOk(protocolVersion: 4, serverTimeMs: 1, connectionId: "c"))
        await channel.push(.authChallenge(nonce: TransportFixtures.nonceB64(), serverTimeMs: 1))
        await channel.push(.authOk(pairingIds: [Wire.PairingId(pairingId)]))
    }

    @Test func eachPairingGetsTheTransportItsRecordSelects() async throws {
        let h = try makeHarness()
        #expect(h.coordinator.store(for: "pair_relay")?.transportMode == .relay)
        #expect(h.coordinator.store(for: "pair_direct")?.transportMode == .direct)
        #expect(h.coordinator.controlClient(for: "pair_relay") == nil)
        let control = try #require(h.coordinator.controlClient(for: "pair_direct"))
        #expect(control.transportPlane == .relayControl)
        #expect(h.coordinator.client(for: "pair_direct")?.transportPlane == .data)

        await h.coordinator.startAll()
        await waitForAllDialed(h)
        // Relay pairing: one relay socket. Direct pairing: the Mac's direct
        // link for data, plus a relay socket for push/revoke.
        #expect(h.relayData.urls.map(\.absoluteString) == ["wss://relay.example/ws"])
        #expect(h.relayControl.urls.map(\.absoluteString) == ["wss://relay.example/ws"])
        #expect(h.directConnector.urls.map(\.absoluteString) == ["ws://ruuds-mbp.tail1234.ts.net:47800/remote/v1"])

        // Background tears down all three sockets.
        await h.coordinator.stopAll()
        for channel in h.relayData.channels + h.relayControl.channels + h.directConnector.channels {
            #expect(await channel.isClosed())
        }
    }

    @Test func directPairingRegistersPushOnTheRelayAndRevokesOnBothLinks() async throws {
        let h = try makeHarness()
        await h.coordinator.startAll()
        await waitForAllDialed(h)
        let direct = try #require(h.directConnector.channels.first)
        let controlChannel = try #require(h.relayControl.channels.first)
        let directData = try #require(h.coordinator.client(for: "pair_direct"))
        let control = try #require(h.coordinator.controlClient(for: "pair_direct"))

        await handshake(direct, pairingId: "pair_direct")
        await handshake(controlChannel, pairingId: "pair_direct")
        _ = await waitUntil { if case .connected = await directData.currentLinkState() { return true }; return false }
        _ = await waitUntil {
            await controlChannel.sentFrames().contains { if case .authResponse = $0 { return true }; return false }
        }
        try await Task.sleep(for: .milliseconds(50))
        #expect(await control.currentLink() == .relayControl)

        h.coordinator.registerPushToken("apns-token", environment: .sandbox)
        _ = await waitUntil { await pushTokenFrames(controlChannel) >= 1 }
        #expect(await pushTokenFrames(controlChannel) == 1, "push token goes to the relay")
        #expect(await pushTokenFrames(direct) == 0, "never to the Mac")

        // The Mac reports itself present on the direct link; the relay's view
        // of the desktop (here: absent, as on a push-only relay whose desktop
        // has no WS) must not leak into the store.
        let store = try #require(h.coordinator.store(for: "pair_direct"))
        await direct.push(.peerPresence(pairingId: Wire.PairingId("pair_direct"), peer: .desktop,
                                        state: .connected, atMs: 1))
        _ = await waitUntilMain { store.peerConnected == true }
        await controlChannel.push(.peerPresence(pairingId: Wire.PairingId("pair_direct"), peer: .desktop,
                                                state: .disconnected, atMs: 2))
        try await Task.sleep(for: .milliseconds(100))
        #expect(store.peerConnected == true)
        if case .connected = store.linkState {} else { Issue.record("store must follow the direct link") }

        // Unpair: revoke goes to the relay AND to the Mac over the direct link.
        #expect(await h.coordinator.revoke(pairingId: "pair_direct"))
        #expect(await controlChannel.sentFrames().contains { if case .revoke = $0 { return true }; return false })
        #expect(await direct.sentFrames().contains { if case .revoke = $0 { return true }; return false })

        await h.coordinator.stopAll()
    }

    /// Connection honesty: with the Mac unreachable the direct pairing's store
    /// shows its cached state as stale and the link as not connected, even
    /// while the relay control link is up.
    @Test func unreachableMacKeepsTheStoreHonestEvenWithTheRelayUp() async throws {
        let h = try makeHarness()
        let store = try #require(h.coordinator.store(for: "pair_direct"))
        store.seedFromCache(SnapshotCache.CachedState(
            snapshot: Wire.StateSnapshot(serverTimeMs: 1, projects: []), transcripts: [], cachedAtMs: 0))
        await h.coordinator.startAll()
        await waitForAllDialed(h)
        // The relay control link authenticates; the Mac never answers.
        await handshake(try #require(h.relayControl.channels.first), pairingId: "pair_direct")
        try await Task.sleep(for: .milliseconds(150))

        if case .connected = store.linkState { Issue.record("store must not report connected without the Mac") }
        #expect(store.isCacheStale)
        #expect(store.snapshot != nil)
        let handle = store.sendCommand(.requestSnapshot(projectId: nil))
        _ = await waitUntilMain { if case .failed = handle.delivery { return true }; return false }
        #expect(handle.delivery == .failed(reason: "not connected"))

        await h.coordinator.stopAll()
    }
}

// MARK: - Pairing stores the direct endpoint from pairing_claimed

@Suite struct DirectPairingTests {

    private func pair(claimedEndpoint: Wire.DirectEndpoint?) async throws -> (PairingRecordStore, Result<PairedDevice, Error>) {
        let keychain = InMemoryKeychainStore()
        let channel = ScriptedChannel()
        let recordStore = PairingRecordStore(store: keychain)
        let service = RealPairingService(
            connector: ScriptedConnector(channel: channel),
            recordStore: recordStore,
            identityStore: keychain,
            relayPasswordStore: RelayPasswordStore(store: keychain),
            clientInfo: Wire.ClientInfo(appVersion: "test", platform: "ios", osVersion: nil),
            timeout: .seconds(5),
            pairingStore: PairingStore(instancesStorage: InMemoryPairedInstancesProvider()))
        let desktopKA = P256.KeyAgreement.PrivateKey().publicKey.x963Representation.base64EncodedString()
        // A push-only relay still bootstraps pairing (§5.9).
        await channel.push(.helloOk(protocolVersion: 4, serverTimeMs: 1, connectionId: "c", relayMode: .pushOnly))
        await channel.push(.authChallenge(nonce: TransportFixtures.nonceB64(), serverTimeMs: 1))
        await channel.push(.pairingClaimed(
            pairingId: Wire.PairingId("pair_direct_1"),
            peerDeviceId: Wire.DeviceId("desk_1"),
            peerKeyAgreementPublicKey: desktopKA,
            directEndpoint: claimedEndpoint))
        await channel.push(.authOk(pairingIds: [Wire.PairingId("pair_direct_1")]))
        do {
            let device = try await service.pair(with: .code("4729", relayURL: URL(string: "wss://relay.example/ws")!))
            // The relay copies `direct_endpoint` into the claim only for a
            // phone that negotiated v4 (`DIRECT_LINK_MIN_VERSION`), so the
            // pairing hello must advertise 4.
            let hello = await channel.sentFrames().first
            guard case let .hello(version, _, _, _, _)? = hello else {
                Issue.record("pairing must open with hello"); return (recordStore, .success(device))
            }
            #expect(version == 4)
            return (recordStore, .success(device))
        } catch {
            return (recordStore, .failure(error))
        }
    }

    @Test func claimWithDirectEndpointStoresADirectModeRecord() async throws {
        let (recordStore, result) = try await pair(claimedEndpoint: endpoint)
        _ = try result.get()
        let record = try #require(try recordStore.load(pairingId: "pair_direct_1"))
        #expect(record.directEndpoint == endpoint)
        #expect(record.transportMode == .direct)
        // The relay URL is kept: push-token registration and revoke stay there.
        #expect(record.relayURL == "wss://relay.example/ws")
    }

    @Test func claimWithoutDirectEndpointStoresARelayModeRecord() async throws {
        let (recordStore, result) = try await pair(claimedEndpoint: nil)
        _ = try result.get()
        let record = try #require(try recordStore.load(pairingId: "pair_direct_1"))
        #expect(record.directEndpoint == nil)
        #expect(record.transportMode == .relay)
    }

    @Test func unusableDirectEndpointFailsPairingInsteadOfFallingBackToTheRelay() async throws {
        for bad in [Wire.DirectEndpoint(host: "", port: 47800), Wire.DirectEndpoint(host: "mac.ts.net", port: 0)] {
            let (recordStore, result) = try await pair(claimedEndpoint: bad)
            guard case let .failure(error) = result else {
                Issue.record("pairing with \(bad) must fail"); continue
            }
            #expect(error is PairingError)
            #expect(try recordStore.load(pairingId: "pair_direct_1") == nil, "no record for an unusable endpoint")
        }
    }
}
