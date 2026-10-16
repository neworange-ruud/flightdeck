//
//  SealedPushTests.swift
//  FlightDeckRemoteTests
//
//  Push payload sealing (spec §7.2, remote-control-bza0), self-contained: the
//  desktop side is sealed in the test straight from CryptoKit and the spec
//  text, and opened with the extension's `PushPayloadCrypto`. The cross-
//  language proof against the desktop's Rust output is `PushVectorsTests`.
//

import CryptoKit
import Foundation
import Testing
@testable import FlightDeckRemote

struct SealedPushTests {
    typealias F = SealedPushFixtures

    // MARK: - Labels

    @Test func infoAndAADAreTheSpecStrings() {
        #expect(PushPayloadCrypto.info(pairingId: "pair_x") == Data("flightdeck-remote-e2e-v1:pair_x:push".utf8))
        #expect(PushPayloadCrypto.aad(pairingId: "pair_x", eventId: "evt_1") == Data("push:pair_x:evt_1".utf8))
    }

    // MARK: - Derivation

    @Test func bothEndsDeriveTheSameKeyAsTheSpec() throws {
        let phone = try PushPayloadCrypto.deriveKey(material: F.phoneMaterial, pairingId: F.pairingId)
        let desktop = try PushPayloadCrypto.deriveKey(material: F.desktopMaterial, pairingId: F.pairingId)
        let reference = try F.desktopKey()
        let bytes = { (key: SymmetricKey) in key.withUnsafeBytes { Data($0) } }
        #expect(bytes(phone) == bytes(reference))
        #expect(bytes(desktop) == bytes(reference))
        #expect(bytes(phone).count == 32)
    }

    @Test func pushKeyDiffersFromTheEnvelopeChannelKeys() throws {
        // Domain separation: the push key must not equal either direction's
        // envelope key derived from the same pairing material.
        let channel = try E2EChannel.derive(
            identityPrivateScalar: F.phoneScalar,
            peerPublicKeyX963: F.desktopPublicX963,
            pairingID: F.pairingId,
            salt: F.salt,
            role: .phone)
        let keys = channel.derivedKeysForCrossLanguageProof()
        let push = try PushPayloadCrypto.deriveKey(material: F.phoneMaterial, pairingId: F.pairingId)
            .withUnsafeBytes { Data($0) }
        #expect(push != keys.d2p)
        #expect(push != keys.p2d)
    }

    @Test func invalidKeyMaterialIsRejected() {
        let bad = PushKeyMaterial(privateScalar: Data(repeating: 0, count: 5), peerPublicKeyX963: F.desktopPublicX963, salt: F.salt)
        #expect(throws: SealedPushError.invalidKeyMaterial) {
            _ = try PushPayloadCrypto.deriveKey(material: bad, pairingId: F.pairingId)
        }
        let badPeer = PushKeyMaterial(privateScalar: F.phoneScalar, peerPublicKeyX963: Data([0x04, 0x01]), salt: F.salt)
        #expect(throws: SealedPushError.invalidKeyMaterial) {
            _ = try PushPayloadCrypto.deriveKey(material: badPeer, pairingId: F.pairingId)
        }
    }

    // MARK: - Round trip

    @Test func desktopSealedPayloadOpens() throws {
        let envelope = try SealedPushEnvelope(userInfo: F.sealedUserInfo())
        let payload = try PushPayloadCrypto.openEnvelope(envelope, material: F.phoneMaterial)
        #expect(payload.eventId == F.eventId)
        #expect(payload.eventClass == .needsInput)
        #expect(payload.title == "fd-api needs input")
        #expect(payload.body == "Allow `rm -rf dist/`?")
        #expect(payload.deepLink == .init(projectId: "proj_1", sessionId: "sess_1", itemId: "item_9"))
        #expect(payload.projectName == "fd-api")
        #expect(payload.occurredAtMs == 1_752_412_802_000)
    }

    @Test func swiftSealMatchesTheIndependentSealer() throws {
        let key = try PushPayloadCrypto.deriveKey(material: F.desktopMaterial, pairingId: F.pairingId)
        let nonce = Data((0..<12).map { UInt8($0) })
        let plaintext = F.payloadJSON()
        let ours = try PushPayloadCrypto.seal(plaintext, key: key, pairingId: F.pairingId, eventId: F.eventId, nonce: nonce)

        let box = try ChaChaPoly.seal(
            plaintext, using: try F.desktopKey(), nonce: try ChaChaPoly.Nonce(data: nonce),
            authenticating: Data("push:\(F.pairingId):\(F.eventId)".utf8))
        #expect(ours.nonceB64 == nonce.base64EncodedString())
        #expect(ours.ciphertextB64 == (box.ciphertext + box.tag).base64EncodedString())
    }

    @Test func nullAndAbsentOptionalFieldsDecode() throws {
        let plaintext = F.payloadJSON(eventClass: "done", body: "", itemId: nil, projectName: nil)
        let sealed = try F.desktopSeal(plaintext)
        let envelope = try SealedPushEnvelope(userInfo: F.userInfo(nonce: sealed.nonce, ciphertext: sealed.ciphertext))
        let payload = try PushPayloadCrypto.openEnvelope(envelope, material: F.phoneMaterial)
        #expect(payload.eventClass == .done)
        #expect(payload.deepLink.itemId == nil)
        #expect(payload.projectName == nil)
        #expect(payload.body.isEmpty)
    }

    // MARK: - Rejections

    @Test func wrongAADFails() throws {
        // Sealed for another event id: the AAD no longer matches.
        let sealed = try F.desktopSeal(F.payloadJSON(), aadEventId: "another-event")
        let envelope = try SealedPushEnvelope(userInfo: F.userInfo(nonce: sealed.nonce, ciphertext: sealed.ciphertext))
        #expect(throws: SealedPushError.openFailed) {
            _ = try PushPayloadCrypto.openEnvelope(envelope, material: F.phoneMaterial)
        }
    }

    @Test func wrongPairingFails() throws {
        // Sealed under pairing B's key, delivered claiming pairing A.
        let otherKey = try F.desktopKey(pairingId: "pair_other")
        let sealed = try F.desktopSeal(F.payloadJSON(), key: otherKey)
        let envelope = try SealedPushEnvelope(userInfo: F.userInfo(nonce: sealed.nonce, ciphertext: sealed.ciphertext))
        #expect(throws: SealedPushError.openFailed) {
            _ = try PushPayloadCrypto.openEnvelope(envelope, material: F.phoneMaterial)
        }
        // And the right pairing id but a different salt (another pairing's
        // claim token) fails too.
        let otherSalt = PushKeyMaterial(privateScalar: F.phoneScalar, peerPublicKeyX963: F.desktopPublicX963, salt: Data("other".utf8))
        let honest = try SealedPushEnvelope(userInfo: F.sealedUserInfo())
        #expect(throws: SealedPushError.openFailed) {
            _ = try PushPayloadCrypto.openEnvelope(honest, material: otherSalt)
        }
    }

    @Test func tamperedCiphertextFails() throws {
        let sealed = try F.desktopSeal(F.payloadJSON())
        var bytes = try #require(Data(base64Encoded: sealed.ciphertext))
        bytes[0] ^= 0x01
        let envelope = try SealedPushEnvelope(userInfo: F.userInfo(nonce: sealed.nonce, ciphertext: bytes.base64EncodedString()))
        #expect(throws: SealedPushError.openFailed) {
            _ = try PushPayloadCrypto.openEnvelope(envelope, material: F.phoneMaterial)
        }
    }

    @Test func garbageInputsAreRejectedNotCrashed() throws {
        let key = try F.desktopKey()
        #expect(throws: SealedPushError.invalidBase64) {
            _ = try PushPayloadCrypto.open(nonceB64: "!!!", ciphertextB64: "AAAA", key: key, pairingId: F.pairingId, eventId: F.eventId)
        }
        #expect(throws: SealedPushError.invalidBase64) {
            _ = try PushPayloadCrypto.open(nonceB64: Data(count: 12).base64EncodedString(), ciphertextB64: "not base64", key: key, pairingId: F.pairingId, eventId: F.eventId)
        }
        #expect(throws: SealedPushError.invalidNonceLength) {
            _ = try PushPayloadCrypto.open(nonceB64: Data(count: 8).base64EncodedString(), ciphertextB64: Data(count: 32).base64EncodedString(), key: key, pairingId: F.pairingId, eventId: F.eventId)
        }
        // Shorter than a tag.
        #expect(throws: SealedPushError.openFailed) {
            _ = try PushPayloadCrypto.open(nonceB64: Data(count: 12).base64EncodedString(), ciphertextB64: Data(count: 4).base64EncodedString(), key: key, pairingId: F.pairingId, eventId: F.eventId)
        }
        // Random bytes of a plausible length.
        #expect(throws: SealedPushError.openFailed) {
            _ = try PushPayloadCrypto.open(nonceB64: Data(count: 12).base64EncodedString(), ciphertextB64: Data(repeating: 0xAB, count: 200).base64EncodedString(), key: key, pairingId: F.pairingId, eventId: F.eventId)
        }
    }

    @Test func plaintextThatIsNotAPushPayloadIsRejected() throws {
        for plaintext in [Data("not json".utf8), Data(#"{"event_id":"x"}"#.utf8), F.payloadJSON(eventClass: "exploded")] {
            let sealed = try F.desktopSeal(plaintext)
            let envelope = try SealedPushEnvelope(userInfo: F.userInfo(nonce: sealed.nonce, ciphertext: sealed.ciphertext))
            #expect(throws: SealedPushError.malformedPayload) {
                _ = try PushPayloadCrypto.openEnvelope(envelope, material: F.phoneMaterial)
            }
        }
        // Empty deep-link ids cannot route a tap.
        let sealed = try F.desktopSeal(F.payloadJSON(projectId: ""))
        let envelope = try SealedPushEnvelope(userInfo: F.userInfo(nonce: sealed.nonce, ciphertext: sealed.ciphertext))
        #expect(throws: SealedPushError.malformedPayload) {
            _ = try PushPayloadCrypto.openEnvelope(envelope, material: F.phoneMaterial)
        }
    }

    @Test func payloadForAnotherEventIsRejected() throws {
        // Correctly sealed under this event's AAD, but the plaintext names a
        // different event (a desktop bug) — never show it under this id.
        let sealed = try F.desktopSeal(F.payloadJSON(eventId: "different-event"))
        let envelope = try SealedPushEnvelope(userInfo: F.userInfo(nonce: sealed.nonce, ciphertext: sealed.ciphertext))
        #expect(throws: SealedPushError.eventMismatch) {
            _ = try PushPayloadCrypto.openEnvelope(envelope, material: F.phoneMaterial)
        }
    }

    // MARK: - Envelope parsing

    @Test func envelopeParsingRejectsBadShapes() throws {
        let sealed = try F.desktopSeal(F.payloadJSON())
        #expect(throws: SealedPushError.missingEnvelope) {
            _ = try SealedPushEnvelope(userInfo: ["aps": ["alert": "x"]])
        }
        #expect(throws: SealedPushError.missingEnvelope) {
            _ = try SealedPushEnvelope(userInfo: ["fd_push": "a string"])
        }
        #expect(throws: SealedPushError.unsupportedVersion) {
            _ = try SealedPushEnvelope(userInfo: F.userInfo(nonce: sealed.nonce, ciphertext: sealed.ciphertext, version: 2))
        }
        #expect(throws: SealedPushError.unsupportedVersion) {
            _ = try SealedPushEnvelope(userInfo: F.userInfo(nonce: sealed.nonce, ciphertext: sealed.ciphertext, version: "1"))
        }
        #expect(throws: SealedPushError.malformedEnvelope) {
            _ = try SealedPushEnvelope(userInfo: F.userInfo(pairingId: "", nonce: sealed.nonce, ciphertext: sealed.ciphertext))
        }
        #expect(throws: SealedPushError.malformedEnvelope) {
            _ = try SealedPushEnvelope(userInfo: F.userInfo(nonce: "", ciphertext: sealed.ciphertext))
        }
        var missingEvent = F.userInfo(nonce: sealed.nonce, ciphertext: sealed.ciphertext)
        var push = try #require(missingEvent["fd_push"] as? [String: Any])
        push.removeValue(forKey: "event_id")
        missingEvent["fd_push"] = push
        #expect(throws: SealedPushError.malformedEnvelope) {
            _ = try SealedPushEnvelope(userInfo: missingEvent)
        }
    }

    @Test func oversizedCiphertextIsRejectedBeforeAnyCrypto() throws {
        let huge = String(repeating: "A", count: SealedPushEnvelope.maxCiphertextB64Length + 4)
        #expect(throws: SealedPushError.ciphertextTooLarge) {
            _ = try SealedPushEnvelope(userInfo: F.userInfo(nonce: Data(count: 12).base64EncodedString(), ciphertext: huge))
        }
        // Exactly at the cap is accepted by the parser.
        let atCap = String(repeating: "A", count: SealedPushEnvelope.maxCiphertextB64Length)
        #expect(throws: Never.self) {
            _ = try SealedPushEnvelope(userInfo: F.userInfo(nonce: Data(count: 12).base64EncodedString(), ciphertext: atCap))
        }
    }

    @Test func aMaximumSizePayloadFitsUnderTheCap() throws {
        // The desktop caps the serialized plaintext at 2048 bytes (spec §7.2);
        // its sealed base64 must fit under the 3000-char relay cap.
        let sealed = try F.desktopSeal(Data(repeating: 0x41, count: 2048))
        #expect(sealed.ciphertext.count <= SealedPushEnvelope.maxCiphertextB64Length)
    }
}
