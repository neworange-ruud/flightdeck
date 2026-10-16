//
//  PushVectorsTests.swift
//  FlightDeckRemoteTests
//
//  The cross-language contract for push payload sealing (spec §7.2,
//  remote-control-bza0). Reads the vectors the desktop crate generates at
//  `remote/protocol/tests/fixtures/e2e_crypto/push_vectors.json` and proves the
//  extension's `PushPayloadCrypto` is byte-compatible with the Rust sealer:
//
//   * the phone derives the exact push key the desktop did (ECDH + HKDF + info);
//   * the AAD Swift builds equals the recorded AAD;
//   * every Rust ciphertext opens to the recorded plaintext;
//   * re-sealing with the recorded nonce reproduces the Rust ciphertext;
//   * the plaintext decodes as a `PushPayload` through the full envelope path;
//   * the recorded tamper (another event id in the AAD) is rejected.
//
//  Loaded from the repo working tree via `#filePath`, exactly like
//  `E2EChannelVectorsTests` loads `vectors.json`.
//

import CryptoKit
import Foundation
import Testing
@testable import FlightDeckRemote

struct PushVectorsTests {

    // MARK: - Vector model

    private struct Vectors: Decodable {
        let cases: [Case]
    }

    private struct Case: Decodable {
        let name: String
        let pairingId: String
        let eventId: String
        let eventClass: String
        let claimToken: String
        let saltB64: String
        let info: String
        let aad: String
        let roleInput: RoleInput
        let derived: Derived
        let plaintextUTF8: String
        let nonceB64: String
        let ciphertextB64: String
        let tamper: Tamper

        enum CodingKeys: String, CodingKey {
            case name, info, aad, derived, tamper
            case pairingId = "pairing_id"
            case eventId = "event_id"
            case eventClass = "class"
            case claimToken = "claim_token"
            case saltB64 = "salt_b64"
            case roleInput = "role_input"
            case plaintextUTF8 = "plaintext_utf8"
            case nonceB64 = "nonce_b64"
            case ciphertextB64 = "ciphertext_b64"
        }
    }

    private struct RoleInput: Decodable {
        let desktopPrivateScalarHex: String
        let phonePrivateScalarHex: String
        let desktopPublicX963B64: String
        let phonePublicX963B64: String

        enum CodingKeys: String, CodingKey {
            case desktopPrivateScalarHex = "desktop_private_scalar_hex"
            case phonePrivateScalarHex = "phone_private_scalar_hex"
            case desktopPublicX963B64 = "desktop_public_x963_b64"
            case phonePublicX963B64 = "phone_public_x963_b64"
        }
    }

    private struct Derived: Decodable {
        let pushKeyHex: String

        enum CodingKeys: String, CodingKey {
            case pushKeyHex = "push_key_hex"
        }
    }

    private struct Tamper: Decodable {
        let tamperedEventId: String
        let tamperedAAD: String
        let expect: String

        enum CodingKeys: String, CodingKey {
            case tamperedEventId = "tampered_event_id"
            case tamperedAAD = "tampered_aad"
            case expect
        }
    }

    // MARK: - Fixture loading

    private static var vectorsURL: URL {
        URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // PushTests
            .deletingLastPathComponent() // FlightDeckRemoteTests
            .deletingLastPathComponent() // ios
            .deletingLastPathComponent() // repo root
            .appendingPathComponent("remote/protocol/tests/fixtures/e2e_crypto/push_vectors.json")
    }

    private static func loadVectors() throws -> Vectors {
        let data = try Data(contentsOf: vectorsURL)
        return try JSONDecoder().decode(Vectors.self, from: data)
    }

    // MARK: - The proof

    @Test func pushVectorsInteroperateWithTheDesktop() throws {
        let vectors = try Self.loadVectors()
        #expect(vectors.cases.isEmpty == false)

        for c in vectors.cases {
            let salt = try #require(Data(base64Encoded: c.saltB64))
            // The salt is the claim token's UTF-8 bytes (spec §7.1 step 2).
            #expect(salt == Data(c.claimToken.utf8), "salt is not the claim token in \(c.name)")
            #expect(PushPayloadCrypto.info(pairingId: c.pairingId) == Data(c.info.utf8), "info mismatch in \(c.name)")

            let phoneScalar = SealedPushFixtures.hexData(c.roleInput.phonePrivateScalarHex)
            let desktopScalar = SealedPushFixtures.hexData(c.roleInput.desktopPrivateScalarHex)
            let desktopPub = try #require(Data(base64Encoded: c.roleInput.desktopPublicX963B64))
            let phonePub = try #require(Data(base64Encoded: c.roleInput.phonePublicX963B64))

            // 1. Both ends derive the Rust-derived key exactly.
            let phoneMaterial = PushKeyMaterial(privateScalar: phoneScalar, peerPublicKeyX963: desktopPub, salt: salt)
            let desktopMaterial = PushKeyMaterial(privateScalar: desktopScalar, peerPublicKeyX963: phonePub, salt: salt)
            let key = try PushPayloadCrypto.deriveKey(material: phoneMaterial, pairingId: c.pairingId)
            let expectedKey = SealedPushFixtures.hexData(c.derived.pushKeyHex)
            #expect(key.withUnsafeBytes { Data($0) } == expectedKey, "push key mismatch in \(c.name)")
            let desktopKey = try PushPayloadCrypto.deriveKey(material: desktopMaterial, pairingId: c.pairingId)
            #expect(desktopKey.withUnsafeBytes { Data($0) } == expectedKey, "desktop-side key mismatch in \(c.name)")

            // 2. AAD.
            #expect(PushPayloadCrypto.aad(pairingId: c.pairingId, eventId: c.eventId) == Data(c.aad.utf8), "AAD mismatch in \(c.name)")

            // 3. The Rust ciphertext opens to the recorded plaintext.
            let opened = try PushPayloadCrypto.open(
                nonceB64: c.nonceB64, ciphertextB64: c.ciphertextB64,
                key: key, pairingId: c.pairingId, eventId: c.eventId)
            #expect(opened == Data(c.plaintextUTF8.utf8), "plaintext mismatch in \(c.name)")

            // 4. Re-sealing with the recorded nonce is byte-identical.
            let resealed = try PushPayloadCrypto.seal(
                opened, key: key, pairingId: c.pairingId, eventId: c.eventId,
                nonce: try #require(Data(base64Encoded: c.nonceB64)))
            #expect(resealed.nonceB64 == c.nonceB64)
            #expect(resealed.ciphertextB64 == c.ciphertextB64, "ciphertext not byte-identical in \(c.name)")

            // 5. The full envelope path (as the extension runs it) accepts it.
            let envelope = SealedPushEnvelope(
                pairingId: c.pairingId, eventId: c.eventId, classHint: c.eventClass,
                nonceB64: c.nonceB64, ciphertextB64: c.ciphertextB64)
            let payload = try PushPayloadCrypto.openEnvelope(envelope, material: phoneMaterial)
            #expect(payload.eventId == c.eventId)
            #expect(payload.eventClass.rawValue == c.eventClass)

            // 6. The recorded tamper is rejected (and its AAD is what we'd build).
            #expect(c.tamper.expect == "reject")
            #expect(PushPayloadCrypto.aad(pairingId: c.pairingId, eventId: c.tamper.tamperedEventId) == Data(c.tamper.tamperedAAD.utf8))
            #expect(throws: SealedPushError.openFailed) {
                _ = try PushPayloadCrypto.open(
                    nonceB64: c.nonceB64, ciphertextB64: c.ciphertextB64,
                    key: key, pairingId: c.pairingId, eventId: c.tamper.tamperedEventId)
            }
        }
    }
}
