//
//  RealPairingServiceTests.swift
//  FlightDeckRemoteTests
//
//  Drives `RealPairingService` against a scripted relay: the happy path
//  (claim → claimed → auth → persisted `PairingRecord`) and rejection mapping
//  to typed `PairingError`s (REMOTE_PROTOCOL §5.2).
//

import Testing
import Foundation
import CryptoKit
@testable import FlightDeckRemote

@Suite struct RealPairingServiceTests {

    private func desktopKAPublicB64() -> String {
        P256.KeyAgreement.PrivateKey().publicKey.x963Representation.base64EncodedString()
    }

    private func makeService(keychain: InMemoryKeychainStore, channel: ScriptedChannel) -> (RealPairingService, PairingRecordStore) {
        let recordStore = PairingRecordStore(store: keychain)
        let service = RealPairingService(
            connector: ScriptedConnector(channel: channel),
            recordStore: recordStore,
            identityStore: keychain,
            clientInfo: Wire.ClientInfo(appVersion: "test", platform: "ios", osVersion: nil),
            timeout: .seconds(5)
        )
        return (service, recordStore)
    }

    @Test func codePathHappyPathPersistsRecord() async throws {
        let keychain = InMemoryKeychainStore()
        let channel = ScriptedChannel()
        let (service, recordStore) = makeService(keychain: keychain, channel: channel)
        let desktopKA = desktopKAPublicB64()

        await channel.push(.helloOk(protocolVersion: 1, serverTimeMs: 1, connectionId: "c"))
        await channel.push(.authChallenge(nonce: TransportFixtures.nonceB64(), serverTimeMs: 1))
        await channel.push(.pairingClaimed(
            pairingId: Wire.PairingId("pair_real_1"),
            peerDeviceId: Wire.DeviceId("desk_1"),
            peerKeyAgreementPublicKey: desktopKA))
        await channel.push(.authOk(pairingIds: [Wire.PairingId("pair_real_1")]))

        let relayURL = URL(string: "wss://relay.example/v1")!
        let device = try await service.pair(with: .code("4729", relayURL: relayURL))

        #expect(device.pairingId == "pair_real_1")
        #expect(!device.peerName.isEmpty)

        let record = try #require(try recordStore.load())
        #expect(record.pairingId == "pair_real_1")
        #expect(record.peerDeviceId == "desk_1")
        #expect(record.peerKeyAgreementPublicKeyB64 == desktopKA)
        #expect(record.relayURL == relayURL.absoluteString)
        // Code path: salt is the claim-token (== the code) UTF-8 bytes.
        #expect(record.salt == Data("4729".utf8))

        // The wire frames the phone emitted: hello, pairing_claim, auth_response.
        let sent = await channel.sentFrames()
        #expect(sent.contains { if case .hello = $0 { return true }; return false })
        let claim = try #require(sent.first { if case .pairingClaim = $0 { return true }; return false })
        if case let .pairingClaim(token, _, devPub, kaPub, role) = claim {
            #expect(token == "4729")
            #expect(role == .phone)
            #expect(devPub != kaPub) // identity key ≠ key-agreement key (§5.2)
        }
        #expect(sent.contains { if case .authResponse = $0 { return true }; return false })
    }

    @Test func qrPathAlsoUsesClaimTokenAsSalt() async throws {
        let keychain = InMemoryKeychainStore()
        let channel = ScriptedChannel()
        let (service, recordStore) = makeService(keychain: keychain, channel: channel)
        let desktopKA = desktopKAPublicB64()

        await channel.push(.helloOk(protocolVersion: 1, serverTimeMs: 1, connectionId: "c"))
        await channel.push(.authChallenge(nonce: TransportFixtures.nonceB64(), serverTimeMs: 1))
        await channel.push(.pairingClaimed(
            pairingId: Wire.PairingId("pair_qr_1"),
            peerDeviceId: Wire.DeviceId("desk_1"),
            peerKeyAgreementPublicKey: desktopKA))
        await channel.push(.authOk(pairingIds: [Wire.PairingId("pair_qr_1")]))

        let secretBytes = Data((0..<32).map { UInt8($0) })
        let payload = PairingQRPayload(
            claimToken: "clm_abc",
            pairingSecret: secretBytes.base64URLEncodedStringNoPadding(),
            relayURL: URL(string: "wss://relay.example/v1")!)

        _ = try await service.pair(with: .qr(payload))
        let record = try #require(try recordStore.load())
        // §7.1 reconciled contract: the salt is the claim-token UTF-8 bytes on
        // BOTH paths; the QR pairing_secret is wire-compat only.
        #expect(record.salt == Data("clm_abc".utf8))
        #expect(record.salt != secretBytes)
    }

    @Test func codeRejectionMapsToInvalidCode() async throws {
        let keychain = InMemoryKeychainStore()
        let channel = ScriptedChannel()
        let (service, _) = makeService(keychain: keychain, channel: channel)

        await channel.push(.helloOk(protocolVersion: 1, serverTimeMs: 1, connectionId: "c"))
        await channel.push(.authChallenge(nonce: TransportFixtures.nonceB64(), serverTimeMs: 1))
        await channel.push(.error(code: .pairingClaimRejected, message: "bad token", pairingId: nil))

        await #expect(throws: PairingError.self) {
            _ = try await service.pair(with: .code("0000", relayURL: URL(string: "wss://relay.example/v1")!))
        }
    }

    @Test func qrRejectionMapsToExpiredOrUsedToken() async throws {
        let keychain = InMemoryKeychainStore()
        let channel = ScriptedChannel()
        let (service, _) = makeService(keychain: keychain, channel: channel)

        await channel.push(.helloOk(protocolVersion: 1, serverTimeMs: 1, connectionId: "c"))
        await channel.push(.authChallenge(nonce: TransportFixtures.nonceB64(), serverTimeMs: 1))
        await channel.push(.error(code: .pairingClaimRejected, message: "expired", pairingId: nil))

        let payload = PairingQRPayload(
            claimToken: "clm_abc",
            pairingSecret: Data((0..<32).map { UInt8($0) }).base64URLEncodedStringNoPadding(),
            relayURL: URL(string: "wss://relay.example/v1")!)

        do {
            _ = try await service.pair(with: .qr(payload))
            Issue.record("expected rejection")
        } catch let error as PairingError {
            #expect(error == .expiredOrUsedToken)
        }
    }

    @Test func malformedQRIsRejectedBeforeAnyNetwork() async throws {
        let keychain = InMemoryKeychainStore()
        let channel = ScriptedChannel()
        let (service, _) = makeService(keychain: keychain, channel: channel)
        // Empty claim token → rejected during input resolution, no frames sent.
        let payload = PairingQRPayload(claimToken: "", pairingSecret: "abc", relayURL: URL(string: "wss://relay.example/v1")!)
        do {
            _ = try await service.pair(with: .qr(payload))
            Issue.record("expected malformed rejection")
        } catch let error as PairingError {
            #expect(error == .malformedQRPayload)
        }
        let sent = await channel.sentFrames()
        #expect(sent.isEmpty)
    }
}
