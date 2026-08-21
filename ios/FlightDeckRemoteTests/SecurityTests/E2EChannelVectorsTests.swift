//
//  E2EChannelVectorsTests.swift
//  FlightDeckRemoteTests
//
//  The cross-language contract for the end-to-end channel crypto
//  (REMOTE_PROTOCOL §7). Reads the Rust-generated vectors at
//  `remote/protocol/tests/fixtures/e2e_crypto/vectors.json`, re-derives the
//  channel keys from the same inputs, and proves byte-for-byte interop:
//
//   * the derived d2p/p2d keys equal the Rust-derived keys (HKDF + ECDH match);
//   * every Rust ciphertext opens to the expected plaintext (AEAD + AAD match);
//   * re-sealing with the fixed nonce reproduces the exact Rust ciphertext;
//   * the tampered header is rejected.
//
//  The vectors file is loaded from the repo working tree via `#filePath`. iOS
//  Simulator tests run on the host, so the host filesystem path resolves — this
//  keeps the test hermetic without wiring the fixture into the app bundle.
//

import Testing
import Foundation
import CryptoKit
@testable import FlightDeckRemote

struct E2EChannelVectorsTests {

    // MARK: - Vector model

    private struct Vectors: Decodable {
        let cases: [Case]
    }

    private struct Case: Decodable {
        let name: String
        let pairingID: String
        let roleInput: RoleInput
        let saltB64: String
        let derived: Derived
        let messages: [Message]
        let tamper: Tamper

        enum CodingKeys: String, CodingKey {
            case name
            case pairingID = "pairing_id"
            case roleInput = "role_input"
            case saltB64 = "salt_b64"
            case derived, messages, tamper
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
        let d2pKeyHex: String
        let p2dKeyHex: String

        enum CodingKeys: String, CodingKey {
            case d2pKeyHex = "d2p_key_hex"
            case p2dKeyHex = "p2d_key_hex"
        }
    }

    private struct Message: Decodable {
        let direction: String
        let sender: String
        let seq: UInt64
        let sentAtMs: Int64
        let aad: String
        let plaintextUTF8: String
        let nonceB64: String
        let ciphertextB64: String

        enum CodingKeys: String, CodingKey {
            case direction, sender, seq, aad
            case sentAtMs = "sent_at_ms"
            case plaintextUTF8 = "plaintext_utf8"
            case nonceB64 = "nonce_b64"
            case ciphertextB64 = "ciphertext_b64"
        }
    }

    private struct Tamper: Decodable {
        let basedOnMessageIndex: Int
        let tamperedSeq: UInt64
        let expect: String

        enum CodingKeys: String, CodingKey {
            case basedOnMessageIndex = "based_on_message_index"
            case tamperedSeq = "tampered_seq"
            case expect
        }
    }

    // MARK: - Fixture loading

    private static func loadVectors() throws -> Vectors {
        let thisFile = URL(fileURLWithPath: #filePath)
        let repoRoot = thisFile
            .deletingLastPathComponent() // SecurityTests
            .deletingLastPathComponent() // FlightDeckRemoteTests
            .deletingLastPathComponent() // ios
            .deletingLastPathComponent() // repo root
        let url = repoRoot.appendingPathComponent(
            "remote/protocol/tests/fixtures/e2e_crypto/vectors.json"
        )
        let data = try Data(contentsOf: url)
        return try JSONDecoder().decode(Vectors.self, from: data)
    }

    private static func hexData(_ hex: String) -> Data {
        var out = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            out.append(UInt8(hex[index..<next], radix: 16)!)
            index = next
        }
        return out
    }

    /// Build both endpoints' channels for a case (desktop uses its scalar + the
    /// phone's public key, and vice-versa).
    private func channels(for c: Case) throws -> (desktop: E2EChannel, phone: E2EChannel) {
        let salt = try #require(Data(base64Encoded: c.saltB64))
        let desktopScalar = Self.hexData(c.roleInput.desktopPrivateScalarHex)
        let phoneScalar = Self.hexData(c.roleInput.phonePrivateScalarHex)
        let desktopPub = try #require(Data(base64Encoded: c.roleInput.desktopPublicX963B64))
        let phonePub = try #require(Data(base64Encoded: c.roleInput.phonePublicX963B64))

        let desktop = try E2EChannel.derive(
            identityPrivateScalar: desktopScalar,
            peerPublicKeyX963: phonePub,
            pairingID: c.pairingID,
            salt: salt,
            role: .desktop
        )
        let phone = try E2EChannel.derive(
            identityPrivateScalar: phoneScalar,
            peerPublicKeyX963: desktopPub,
            pairingID: c.pairingID,
            salt: salt,
            role: .phone
        )
        return (desktop, phone)
    }

    // MARK: - The proof

    @Test func crossLanguageVectorsInteroperate() throws {
        let vectors = try Self.loadVectors()
        #expect(vectors.cases.isEmpty == false)

        for c in vectors.cases {
            let (desktop, phone) = try channels(for: c)

            // 1. Derived keys match the Rust-generated keys exactly.
            let keys = desktop.derivedKeysForCrossLanguageProof()
            #expect(keys.d2p == Self.hexData(c.derived.d2pKeyHex), "d2p key mismatch in \(c.name)")
            #expect(keys.p2d == Self.hexData(c.derived.p2dKeyHex), "p2d key mismatch in \(c.name)")
            // Both endpoints derive the identical pair.
            let phoneKeys = phone.derivedKeysForCrossLanguageProof()
            #expect(phoneKeys.d2p == keys.d2p)
            #expect(phoneKeys.p2d == keys.p2d)

            for message in c.messages {
                let sender: E2EChannel.Role = message.sender == "desktop" ? .desktop : .phone
                // The receiving endpoint is the peer of the sender.
                let receiver = sender == .desktop ? phone : desktop
                let sealer = sender == .desktop ? desktop : phone
                let expectedPlaintext = Data(message.plaintextUTF8.utf8)

                // 2. Every Rust ciphertext opens to the expected plaintext.
                let opened = try receiver.open(
                    seq: message.seq,
                    sender: sender,
                    sentAtMs: message.sentAtMs,
                    nonceB64: message.nonceB64,
                    ciphertextB64: message.ciphertextB64
                )
                #expect(opened == expectedPlaintext, "plaintext mismatch in \(c.name)/\(message.direction)")

                // 3. Re-sealing with the fixed nonce reproduces the exact bytes.
                let nonce = try #require(Data(base64Encoded: message.nonceB64))
                let resealed = try sealer.sealWithNonce(
                    expectedPlaintext,
                    seq: message.seq,
                    sentAtMs: message.sentAtMs,
                    nonce: nonce
                )
                #expect(resealed.nonceB64 == message.nonceB64)
                #expect(
                    resealed.ciphertextB64 == message.ciphertextB64,
                    "ciphertext not byte-identical in \(c.name)/\(message.direction)"
                )

                // The AAD the vectors record must match what Swift computes.
                #expect(
                    E2EChannel.headerAAD(
                        pairingID: c.pairingID,
                        seq: message.seq,
                        sender: sender,
                        sentAtMs: message.sentAtMs
                    ) == message.aad
                )
            }

            // 4. Tampering with the header (wrong seq) must be rejected.
            let base = c.messages[c.tamper.basedOnMessageIndex]
            let sender: E2EChannel.Role = base.sender == "desktop" ? .desktop : .phone
            let receiver = sender == .desktop ? phone : desktop
            #expect(throws: E2EChannelError.self) {
                _ = try receiver.open(
                    seq: c.tamper.tamperedSeq, // tampered
                    sender: sender,
                    sentAtMs: base.sentAtMs,
                    nonceB64: base.nonceB64,
                    ciphertextB64: base.ciphertextB64
                )
            }
            // And the honest header still opens (control).
            #expect(throws: Never.self) {
                _ = try receiver.open(
                    seq: base.seq,
                    sender: sender,
                    sentAtMs: base.sentAtMs,
                    nonceB64: base.nonceB64,
                    ciphertextB64: base.ciphertextB64
                )
            }
        }
    }

    // MARK: - Local (non-vector) round-trips

    @Test func localRoundTripAndTamperRejection() throws {
        // A pure-Swift derive→seal→open cycle, independent of the vectors, to
        // guard the Swift path in isolation.
        let desktopScalar = Data((1...32).map { UInt8($0) })
        let phoneScalar = Data((1...32).reversed().map { UInt8($0) })
        let desktopPub = try P256.KeyAgreement.PrivateKey(rawRepresentation: desktopScalar).publicKey.x963Representation
        let phonePub = try P256.KeyAgreement.PrivateKey(rawRepresentation: phoneScalar).publicKey.x963Representation
        let salt = Data("some-bootstrap-secret".utf8)

        let desktop = try E2EChannel.derive(
            identityPrivateScalar: desktopScalar,
            peerPublicKeyX963: phonePub,
            pairingID: "pair_local",
            salt: salt,
            role: .desktop
        )
        let phone = try E2EChannel.derive(
            identityPrivateScalar: phoneScalar,
            peerPublicKeyX963: desktopPub,
            pairingID: "pair_local",
            salt: salt,
            role: .phone
        )

        let plaintext = Data(#"{"type":"snapshot"}"#.utf8)
        let sealed = try desktop.seal(plaintext, seq: 5, sentAtMs: 1_752_000_000_000)
        let opened = try phone.open(
            seq: 5,
            sender: .desktop,
            sentAtMs: 1_752_000_000_000,
            nonceB64: sealed.nonceB64,
            ciphertextB64: sealed.ciphertextB64
        )
        #expect(opened == plaintext)

        // Wrong sender role (wrong key) fails.
        #expect(throws: E2EChannelError.self) {
            _ = try phone.open(
                seq: 5,
                sender: .phone,
                sentAtMs: 1_752_000_000_000,
                nonceB64: sealed.nonceB64,
                ciphertextB64: sealed.ciphertextB64
            )
        }
    }
}
