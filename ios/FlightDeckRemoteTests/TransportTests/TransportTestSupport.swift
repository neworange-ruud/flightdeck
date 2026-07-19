//
//  TransportTestSupport.swift
//  FlightDeckRemoteTests
//
//  Shared harness for the transport tests: a scripted `WebSocketChannel` the
//  test drives frame-by-frame, a matching connector, a thread-safe event
//  collector, a desktop-side E2E sealing helper, and small async wait utilities.
//

import Foundation
import CryptoKit
@testable import FlightDeckRemote

// MARK: - Scripted WebSocket

/// A `WebSocketChannel` whose inbound frames the test pushes explicitly and
/// whose outbound frames it can inspect. `receive()` suspends until a frame is
/// pushed (or the channel is closed).
actor ScriptedChannel: WebSocketChannel {
    private var inbound: [Wire.RelayFrame] = []
    private var waiter: CheckedContinuation<Wire.RelayFrame, Error>?
    private var closed = false
    private(set) var sent: [Wire.RelayFrame] = []
    private(set) var pings = 0

    // WebSocketChannel

    func send(_ frame: Wire.RelayFrame) async throws {
        if closed { throw RelayConnectionError.closed }
        sent.append(frame)
    }

    func receive() async throws -> Wire.RelayFrame {
        if !inbound.isEmpty {
            return inbound.removeFirst()
        }
        if closed { throw RelayConnectionError.closed }
        return try await withCheckedThrowingContinuation { continuation in
            self.waiter = continuation
        }
    }

    func ping() async throws {
        pings += 1
    }

    func close() async {
        closed = true
        if let waiter {
            self.waiter = nil
            waiter.resume(throwing: RelayConnectionError.closed)
        }
    }

    // Test controls

    /// Push an inbound frame the client will receive.
    func push(_ frame: Wire.RelayFrame) {
        if let waiter {
            self.waiter = nil
            waiter.resume(returning: frame)
        } else {
            inbound.append(frame)
        }
    }

    /// The outbound frames captured so far.
    func sentFrames() -> [Wire.RelayFrame] { sent }

    /// Whether `close()` has been called (teardown assertion — no lingering
    /// socket after `stop`/`stopAll`).
    func isClosed() -> Bool { closed }
}

/// A `WebSocketConnecting` that hands out scripted channels. By default it
/// always returns the same channel; a factory supports reconnect scenarios.
final class ScriptedConnector: WebSocketConnecting, @unchecked Sendable {
    private let make: @Sendable () -> ScriptedChannel
    private let lock = NSLock()
    private var _channels: [ScriptedChannel] = []

    init(channel: ScriptedChannel) {
        self.make = { channel }
    }

    init(factory: @escaping @Sendable () -> ScriptedChannel) {
        self.make = factory
    }

    func connect(to url: URL) async throws -> any WebSocketChannel {
        let channel = make()
        lock.lock(); _channels.append(channel); lock.unlock()
        return channel
    }

    var channels: [ScriptedChannel] {
        lock.lock(); defer { lock.unlock() }; return _channels
    }
}

// MARK: - Event collector

/// Thread-safe sink for `TransportEvent`s emitted by the actor client.
final class EventCollector: @unchecked Sendable {
    private let lock = NSLock()
    private var _events: [TransportEvent] = []

    var handler: @Sendable (TransportEvent) -> Void {
        { [weak self] event in
            guard let self else { return }
            self.lock.lock(); self._events.append(event); self.lock.unlock()
        }
    }

    var events: [TransportEvent] {
        lock.lock(); defer { lock.unlock() }; return _events
    }

    var messages: [Wire.DesktopToPhone] {
        events.compactMap { if case let .message(m) = $0 { return m }; return nil }
    }

    var links: [RemoteLinkState] {
        events.compactMap { if case let .link(s) = $0 { return s }; return nil }
    }

    func deliveries(for id: Wire.CommandId) -> [CommandDeliveryState] {
        events.compactMap {
            if case let .delivery(cid, state) = $0, cid == id { return state }
            return nil
        }
    }
}

// MARK: - Desktop-side crypto helper

/// Builds a paired (phone, desktop) crypto context for a test: it generates a
/// desktop key-agreement keypair, records the resulting `PairingRecord` (whose
/// peer KA key is the desktop's public point), and exposes the desktop
/// `E2EChannel` so the test can seal `DesktopToPhone` frames the client opens.
struct DesktopPeer {
    let record: PairingRecord
    let desktopChannel: E2EChannel
    let pairingId: String

    /// Seal a desktop→phone message into an `envelope` relay frame at `seq`.
    func envelopeFrame(_ message: Wire.DesktopToPhone, seq: UInt64, sentAtMs: Int64 = 1_752_000_000_000) throws -> Wire.RelayFrame {
        let plaintext = try JSONEncoder().encode(message)
        let sealed = try desktopChannel.seal(plaintext, seq: seq, sentAtMs: sentAtMs)
        return .envelope(Wire.EncryptedEnvelope(
            pairingId: Wire.PairingId(pairingId),
            seq: seq,
            sender: .desktop,
            sentAtMs: sentAtMs,
            nonce: sealed.nonceB64,
            ciphertext: sealed.ciphertextB64
        ))
    }

    /// Open a phone→desktop envelope the client sent, decoding the command.
    func openCommand(_ envelope: Wire.EncryptedEnvelope) throws -> Wire.PhoneCommand {
        let plaintext = try desktopChannel.open(
            seq: envelope.seq,
            sender: .phone,
            sentAtMs: envelope.sentAtMs,
            nonceB64: envelope.nonce,
            ciphertextB64: envelope.ciphertext
        )
        return try JSONDecoder().decode(Wire.PhoneCommand.self, from: plaintext)
    }
}

enum TransportFixtures {
    /// Build a fully-wired phone/desktop pair over an in-memory keychain.
    static func makePeer(
        keychain: KeychainStoring,
        pairingId: String = "pair_test_1",
        salt: Data = Data("bootstrap-secret-bytes-000000".utf8),
        relayURL: String = "wss://relay.example/v1",
        lastReceivedSeq: UInt64 = 0,
        lastSentSeq: UInt64 = 0
    ) throws -> (peer: DesktopPeer, keyAgreement: KeyAgreementKeys) {
        let phoneKA = try KeyAgreementKeys.loadOrCreate(store: keychain)

        // Desktop key-agreement keypair (software, as on the wire).
        let desktopPriv = P256.KeyAgreement.PrivateKey()
        let desktopPubB64 = desktopPriv.publicKey.x963Representation.base64EncodedString()

        let record = PairingRecord(
            pairingId: pairingId,
            peerDeviceId: "desktop-device-id",
            peerKeyAgreementPublicKeyB64: desktopPubB64,
            saltB64: salt.base64EncodedString(),
            relayURL: relayURL,
            lastSentSeq: lastSentSeq,
            lastReceivedSeq: lastReceivedSeq
        )

        let desktopChannel = try E2EChannel.derive(
            identityPrivateScalar: desktopPriv.rawRepresentation,
            peerPublicKeyX963: phoneKA.publicKeyX963,
            pairingID: pairingId,
            salt: salt,
            role: .desktop
        )

        return (DesktopPeer(record: record, desktopChannel: desktopChannel, pairingId: pairingId), phoneKA)
    }

    /// A well-formed base64 nonce for `auth_challenge` (any value; the scripted
    /// relay does not verify).
    static func nonceB64() -> String {
        Data((0..<32).map { UInt8($0) }).base64EncodedString()
    }
}

// MARK: - Async waiting

/// Poll `condition` until it is true or `timeout` elapses.
@discardableResult
func waitUntil(
    timeout: Duration = .seconds(3),
    _ condition: @Sendable () async -> Bool
) async -> Bool {
    let deadline = ContinuousClock.now + timeout
    while ContinuousClock.now < deadline {
        if await condition() { return true }
        try? await Task.sleep(for: .milliseconds(10))
    }
    return await condition()
}

/// MainActor variant so tests can poll `@MainActor` store state.
@MainActor
@discardableResult
func waitUntilMain(
    timeout: Duration = .seconds(3),
    _ condition: @MainActor () async -> Bool
) async -> Bool {
    let deadline = ContinuousClock.now + timeout
    while ContinuousClock.now < deadline {
        if await condition() { return true }
        try? await Task.sleep(for: .milliseconds(10))
    }
    return await condition()
}
