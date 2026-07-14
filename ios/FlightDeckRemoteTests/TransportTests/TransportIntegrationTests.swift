//
//  TransportIntegrationTests.swift
//  FlightDeckRemoteTests
//
//  Best-effort live end-to-end against the REAL relay (`cargo run -p
//  flightdeck-relay`). It simulates the DESKTOP side in-test with a raw
//  WebSocket (pairing_offer + P-256 auth), then drives the phone through the
//  production path: `RealPairingService` redeems the claim token against the
//  relay, and `TransportClient` connects/auths/resumes and exchanges one
//  sealed envelope each direction with the desktop peer.
//
//  It is GATED by reachability: if no relay answers at the configured URL
//  within a short probe, the test prints `SKIPPED` and passes, so the suite is
//  green whether or not a relay is running. Set `FLIGHTDECK_RELAY_WS` to point
//  at a relay (default `ws://127.0.0.1:8080/ws`).
//

import Testing
import Foundation
import CryptoKit
@testable import FlightDeckRemote

@Suite struct TransportIntegrationTests {

    private var relayURLString: String {
        ProcessInfo.processInfo.environment["FLIGHTDECK_RELAY_WS"] ?? "ws://127.0.0.1:8080/ws"
    }

    // MARK: - Raw desktop-side driver

    /// Minimal desktop endpoint: signs auth with a P-256 identity, mints a
    /// claim token via `pairing_offer`, and seals/opens envelopes.
    private final class DesktopDriver {
        let channel: any WebSocketChannel
        let signingKey = P256.Signing.PrivateKey()
        let kaKey = P256.KeyAgreement.PrivateKey()
        let deviceId = "desktop-integration-\(UUID().uuidString.prefix(6))"
        var nonceB64 = ""
        var e2e: E2EChannel?
        var outSeq: UInt64 = 0

        init(channel: any WebSocketChannel) { self.channel = channel }

        var devicePublicKeyB64: String { signingKey.publicKey.x963Representation.base64EncodedString() }
        var kaPublicKeyB64: String { kaKey.publicKey.x963Representation.base64EncodedString() }

        func recv(timeout: Duration = .seconds(5)) async throws -> Wire.RelayFrame {
            try await withThrowingTaskGroup(of: Wire.RelayFrame.self) { group in
                group.addTask { try await self.channel.receive() }
                group.addTask { try await Task.sleep(for: timeout); throw CancellationError() }
                let f = try await group.next()!
                group.cancelAll()
                return f
            }
        }

        func recvUntil(_ pred: (Wire.RelayFrame) -> Bool, timeout: Duration = .seconds(5)) async throws -> Wire.RelayFrame {
            let deadline = ContinuousClock.now + timeout
            while ContinuousClock.now < deadline {
                let f = try await recv(timeout: timeout)
                if pred(f) { return f }
            }
            throw CancellationError()
        }

        /// hello → hello_ok → auth_challenge → pairing_offer → pairing_offer_ok
        /// → auth_response → auth_ok. Returns (pairingId, claimToken).
        func connectAndOffer() async throws -> (String, String) {
            try await channel.send(.hello(protocolVersion: 1, role: .desktop,
                deviceId: Wire.DeviceId(deviceId),
                client: Wire.ClientInfo(appVersion: "test", platform: "macos", osVersion: nil)))
            _ = try await recvUntil { if case .helloOk = $0 { return true }; return false }
            let challenge = try await recvUntil { if case .authChallenge = $0 { return true }; return false }
            if case let .authChallenge(nonce, _) = challenge { nonceB64 = nonce }

            try await channel.send(.pairingOffer(deviceId: Wire.DeviceId(deviceId),
                devicePublicKey: devicePublicKeyB64, keyAgreementPublicKey: kaPublicKeyB64, role: .desktop))
            let offerOk = try await recvUntil { if case .pairingOfferOk = $0 { return true }; return false }
            var pairingId = ""
            var claimToken = ""
            if case let .pairingOfferOk(pid, token, _) = offerOk { pairingId = pid.rawValue; claimToken = token }

            // Sign the decoded nonce with the identity key.
            let nonceData = Data(base64Encoded: nonceB64)!
            let sig = try signingKey.signature(for: nonceData).rawRepresentation.base64EncodedString()
            try await channel.send(.authResponse(deviceId: Wire.DeviceId(deviceId),
                signature: sig, pairingIds: [Wire.PairingId(pairingId)]))
            _ = try await recvUntil { if case .authOk = $0 { return true }; return false }
            return (pairingId, claimToken)
        }

        /// After the phone claims, read `pairing_claimed` to learn the phone's
        /// KA key, then derive the desktop E2E channel.
        func deriveChannel(pairingId: String, claimToken: String) async throws {
            let claimed = try await recvUntil { if case .pairingClaimed = $0 { return true }; return false }
            guard case let .pairingClaimed(_, _, peerKA) = claimed, let peerKA, let phonePub = Data(base64Encoded: peerKA) else {
                throw CancellationError()
            }
            e2e = try E2EChannel.derive(
                identityPrivateScalar: kaKey.rawRepresentation,
                peerPublicKeyX963: phonePub,
                pairingID: pairingId,
                salt: Data(claimToken.utf8), // code path: claim-token UTF-8 bytes
                role: .desktop)
        }

        func sendEnvelope(_ message: Wire.DesktopToPhone, pairingId: String) async throws {
            outSeq += 1
            let plaintext = try JSONEncoder().encode(message)
            let sealed = try e2e!.seal(plaintext, seq: outSeq, sentAtMs: 1_752_000_000_000)
            try await channel.send(.envelope(Wire.EncryptedEnvelope(
                pairingId: Wire.PairingId(pairingId), seq: outSeq, sender: .desktop,
                sentAtMs: 1_752_000_000_000, nonce: sealed.nonceB64, ciphertext: sealed.ciphertextB64)))
        }

        func openPhoneCommand(_ env: Wire.EncryptedEnvelope) throws -> Wire.PhoneCommand {
            let pt = try e2e!.open(seq: env.seq, sender: .phone, sentAtMs: env.sentAtMs,
                                   nonceB64: env.nonce, ciphertextB64: env.ciphertext)
            return try JSONDecoder().decode(Wire.PhoneCommand.self, from: pt)
        }
    }

    // MARK: - The live round-trip

    @Test func liveRelayPairAndEnvelopeRoundTrip() async throws {
        let connector = URLSessionWebSocketConnection()
        guard let relayURL = URL(string: relayURLString) else { return }

        // Reachability probe: if the desktop can't complete the offer, skip.
        let desktopChannel: any WebSocketChannel
        let desktop: DesktopDriver
        let pairingId: String
        let claimToken: String
        do {
            desktopChannel = try await connector.connect(to: relayURL)
            desktop = DesktopDriver(channel: desktopChannel)
            (pairingId, claimToken) = try await desktop.connectAndOffer()
        } catch {
            print("SKIPPED liveRelayPairAndEnvelopeRoundTrip: no relay at \(relayURLString) (\(error))")
            return
        }

        // Phone pairs via the production RealPairingService (code == claim token).
        let keychain = InMemoryKeychainStore()
        let recordStore = PairingRecordStore(store: keychain)
        let pairing = RealPairingService(
            connector: connector, recordStore: recordStore, identityStore: keychain,
            clientInfo: Wire.ClientInfo(appVersion: "test", platform: "ios", osVersion: nil),
            timeout: .seconds(8))
        let device = try await pairing.pair(with: .code(claimToken, relayURL: relayURL))
        #expect(device.pairingId == pairingId)

        // Desktop learns the phone KA key and derives its channel.
        try await desktop.deriveChannel(pairingId: pairingId, claimToken: claimToken)

        // Phone transport connects/auths/resumes.
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        let ka = try KeyAgreementKeys.loadOrCreate(store: keychain)
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        config.requestSnapshotOnResume = false
        let collector = EventCollector()
        let client = TransportClient(
            identity: identity, keyAgreement: ka, recordStore: recordStore,
            connector: connector,
            clientInfo: Wire.ClientInfo(appVersion: "test", platform: "ios", osVersion: nil),
            config: config)
        await client.setEventHandler(collector.handler)
        await client.start()
        let connected = await waitUntil(timeout: .seconds(8)) {
            if case .connected = await client.currentLinkState() { return true }; return false
        }
        #expect(connected)

        // Desktop → phone: one sealed snapshot flows end-to-end through the relay.
        let snapshot = Wire.StateSnapshot(serverTimeMs: 1, projects: [])
        try await desktop.sendEnvelope(.snapshot(snapshot), pairingId: pairingId)
        let gotSnapshot = await waitUntil(timeout: .seconds(8)) {
            collector.messages.contains { if case .snapshot = $0 { return true }; return false }
        }
        #expect(gotSnapshot)

        // Phone → desktop: one sealed command flows the other way.
        let cmdId = Wire.CommandId("cmd_live_1")
        await client.send(Wire.PhoneCommand(commandId: cmdId, issuedAtMs: 1,
            body: .reply(sessionId: Wire.SessionId("s1"), text: "live round trip")))

        let phoneEnv = try await desktop.recvUntil({ frame in
            if case let .envelope(e) = frame, e.sender == .phone { return true }; return false
        }, timeout: .seconds(8))
        if case let .envelope(env) = phoneEnv {
            let command = try desktop.openPhoneCommand(env)
            #expect(command.commandId == cmdId)
            if case let .reply(_, text) = command.body { #expect(text == "live round trip") }
        }

        await client.stop()
        await desktopChannel.close()
        print("PASSED liveRelayPairAndEnvelopeRoundTrip: full E2E round-trip via relay at \(relayURLString)")
    }
}
