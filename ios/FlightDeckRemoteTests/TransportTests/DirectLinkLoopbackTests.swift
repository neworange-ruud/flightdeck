//
//  DirectLinkLoopbackTests.swift
//  FlightDeckRemoteTests
//
//  End-to-end over a REAL socket: the production `DirectConnection`
//  (Network.framework WebSocket) + `TransportClient` against a fake Mac served
//  in-process by an `NWListener` bound to 127.0.0.1 only. Where the scripted-
//  socket tests (DirectTransportTests) pin the state machine, this pins the
//  socket layer itself — text framing both ways, the handshake with the
//  domain-separated auth signature, sealed envelopes, and that a Mac-side close
//  (restart) wakes the phone's receive loop so it reconnects and resumes.
//

import Testing
import Foundation
import Network
import CryptoKit
@testable import FlightDeckRemote

/// A minimal fake Mac direct-link endpoint on loopback. Each accepted
/// connection is numbered; inbound frames surface as `(connection, frame)`.
private final class LoopbackMac: @unchecked Sendable {
    private let listener: NWListener
    private let queue = DispatchQueue(label: "agency.neworange.flightdeck.remote.tests.loopback-mac")
    private let lock = NSLock()
    private var connections: [NWConnection] = []
    private var inbox: [(Int, Wire.RelayFrame)] = []

    init() throws {
        let parameters = NWParameters.tcp
        let webSocket = NWProtocolWebSocket.Options()
        webSocket.autoReplyPing = true
        parameters.defaultProtocolStack.applicationProtocols.insert(webSocket, at: 0)
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        listener = try NWListener(using: parameters)
    }

    /// Start listening; returns the bound port.
    func start() async throws -> UInt16 {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<UInt16, Error>) in
            let once = OnceFlag()
            listener.stateUpdateHandler = { [weak self] state in
                switch state {
                case .ready:
                    if once.claim() { continuation.resume(returning: self?.listener.port?.rawValue ?? 0) }
                case let .failed(error):
                    if once.claim() { continuation.resume(throwing: error) }
                default: break
                }
            }
            listener.newConnectionHandler = { [weak self] connection in self?.accept(connection) }
            listener.start(queue: queue)
        }
    }

    private func accept(_ connection: NWConnection) {
        lock.lock()
        connections.append(connection)
        let index = connections.count - 1
        lock.unlock()
        connection.start(queue: queue)
        receive(on: connection, index: index)
    }

    private func receive(on connection: NWConnection, index: Int) {
        connection.receiveMessage { [weak self] content, context, _, error in
            guard let self, error == nil else { return }
            let metadata = context?.protocolMetadata(definition: NWProtocolWebSocket.definition)
                as? NWProtocolWebSocket.Metadata
            if metadata?.opcode == .text, let content,
               let frame = try? JSONDecoder().decode(Wire.RelayFrame.self, from: content) {
                self.lock.lock(); self.inbox.append((index, frame)); self.lock.unlock()
            }
            if metadata?.opcode == .close || (content == nil && metadata == nil) { return }
            self.receive(on: connection, index: index)
        }
    }

    var connectionCount: Int { lock.lock(); defer { lock.unlock() }; return connections.count }

    func frames(on index: Int) -> [Wire.RelayFrame] {
        lock.lock(); defer { lock.unlock() }
        return inbox.filter { $0.0 == index }.map(\.1)
    }

    func send(_ frame: Wire.RelayFrame, on index: Int) async throws {
        lock.lock(); let connection = connections[index]; lock.unlock()
        let data = try JSONEncoder().encode(frame)
        let context = NWConnection.ContentContext(
            identifier: "text", metadata: [NWProtocolWebSocket.Metadata(opcode: .text)])
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.send(content: data, contentContext: context, isComplete: true,
                            completion: .contentProcessed { error in
                if let error { continuation.resume(throwing: error) } else { continuation.resume() }
            })
        }
    }

    /// Drop one connection (the Mac restarting / going to sleep).
    func drop(_ index: Int) {
        lock.lock(); let connection = connections[index]; lock.unlock()
        connection.cancel()
    }

    func stop() {
        lock.lock(); let all = connections; lock.unlock()
        all.forEach { $0.cancel() }
        listener.cancel()
    }
}

private final class OnceFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var claimed = false
    func claim() -> Bool { lock.lock(); defer { lock.unlock() }; if claimed { return false }; claimed = true; return true }
}

@Suite struct DirectLinkLoopbackTests {

    @Test func realSocketHandshakeResumeEnvelopeAndReconnectAfterMacRestart() async throws {
        let mac = try LoopbackMac()
        let port = try await mac.start()
        defer { mac.stop() }
        #expect(port != 0)

        let keychain = InMemoryKeychainStore()
        let (peer, ka) = try TransportFixtures.makePeer(
            keychain: keychain, pairingId: "pair_loop", relayURL: "wss://relay.example/ws",
            directEndpoint: Wire.DirectEndpoint(host: "127.0.0.1", port: port))
        let recordStore = PairingRecordStore(store: keychain)
        try recordStore.save(peer.record)
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        config.requestSnapshotOnResume = false
        config.minimumGoodSession = .zero
        let client = TransportClient(
            identity: identity, keyAgreement: ka, recordStore: recordStore,
            pairingId: "pair_loop", connector: DirectConnection(connectTimeout: .seconds(3)),
            clientInfo: Wire.ClientInfo(appVersion: "test", platform: "ios", osVersion: nil),
            config: config, jitter: { 0 })
        let collector = EventCollector()
        await client.setEventHandler(collector.handler)
        await client.start()

        func handshake(_ index: Int) async throws {
            _ = await waitUntil { mac.frames(on: index).contains { if case .hello = $0 { return true }; return false } }
            try await mac.send(.helloOk(protocolVersion: 4, serverTimeMs: 1, connectionId: "mac"), on: index)
            let nonce = Data((0..<32).map { _ in UInt8.random(in: 0...255) })
            try await mac.send(.authChallenge(nonce: nonce.base64EncodedString(), serverTimeMs: 1), on: index)
            _ = await waitUntil {
                mac.frames(on: index).contains { if case .authResponse = $0 { return true }; return false }
            }
            // The Mac verifies the domain-separated signature (§5.10).
            let auth = try #require(mac.frames(on: index).first { if case .authResponse = $0 { return true }; return false })
            guard case let .authResponse(_, signatureB64, _, _) = auth else { return }
            let key = try P256.Signing.PublicKey(x963Representation: try #require(Data(base64Encoded: identity.publicKeyBase64)))
            let signature = try P256.Signing.ECDSASignature(rawRepresentation: try #require(Data(base64Encoded: signatureB64)))
            #expect(key.isValidSignature(signature, for: Wire.directAuthMessage(nonce: nonce)))
            try await mac.send(.authOk(pairingIds: [Wire.PairingId("pair_loop")]), on: index)
            try await mac.send(.peerPresence(pairingId: Wire.PairingId("pair_loop"), peer: .desktop,
                                             state: .connected, atMs: 1), on: index)
        }

        func resumeFrom(_ index: Int) -> UInt64? {
            mac.frames(on: index).compactMap { if case let .resume(_, from) = $0 { return from }; return nil }.first
        }
        func acked(_ index: Int) -> [UInt64] {
            mac.frames(on: index).compactMap { if case let .ack(_, cursor) = $0 { return cursor }; return nil }
        }

        func snapshot(_ seq: UInt64) throws -> Wire.RelayFrame {
            try peer.envelopeFrame(.snapshot(Wire.StateSnapshot(serverTimeMs: Int64(seq), projects: [])), seq: seq)
        }

        // Session 1: handshake, resume from 0, two sealed envelopes, acks.
        _ = await waitUntil { mac.connectionCount >= 1 }
        try await handshake(0)
        _ = await waitUntil { resumeFrom(0) != nil }
        #expect(resumeFrom(0) == 0)
        try await mac.send(snapshot(1), on: 0)
        try await mac.send(snapshot(2), on: 0)
        _ = await waitUntil { acked(0).contains(2) }
        #expect(acked(0) == [1, 2])
        #expect(await client.currentLinkState() != .disconnected)

        // The Mac restarts: its socket goes away. The phone notices, reports
        // the drop, reconnects on its own and resumes from the durable cursor.
        mac.drop(0)
        _ = await waitUntil { collector.links.contains(.disconnected) }
        #expect(collector.links.contains(.disconnected))
        _ = await waitUntil(timeout: .seconds(6)) { mac.connectionCount >= 2 }
        try await handshake(1)
        _ = await waitUntil { resumeFrom(1) != nil }
        #expect(resumeFrom(1) == 2)

        // A duplicate of seq 2 (its ack raced the restart) then seq 3: one
        // delivery each — the duplicate is deduped, not re-acked. (A replayed
        // seq 1 is deliberately not used: §6.4 reads seq 1 over a cursor ≥ 1
        // as a stream restart, and a conforming outbox replays strictly above
        // `from_seq` anyway.)
        try await mac.send(snapshot(2), on: 1)
        try await mac.send(snapshot(3), on: 1)
        _ = await waitUntil { acked(1).contains(3) }
        #expect(acked(1) == [3])
        let delivered = collector.messages.compactMap { m -> Int64? in
            if case let .snapshot(s) = m { return s.serverTimeMs }; return nil
        }
        #expect(delivered == [1, 2, 3])

        await client.stop()
    }
}
