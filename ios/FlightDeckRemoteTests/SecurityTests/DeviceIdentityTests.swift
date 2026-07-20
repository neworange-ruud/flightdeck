//
//  DeviceIdentityTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the per-device identity keypair (REMOTE_PROTOCOL §5.1, PRD §9):
//  create → sign → verify round-trips through the base64 wire format, the
//  device id is stable across loads and changes after destroy, and the
//  exported encodings have the exact byte shapes the relay expects.
//
//  Functional tests use an in-memory KeychainStoring so they are hermetic and
//  free of simulator Keychain-entitlement flakiness (see task note). One test
//  reports the KeyBacking the simulator actually selected.
//

import Testing
import Foundation
import CryptoKit
@testable import FlightDeckRemote

/// In-memory `KeychainStoring` for deterministic, entitlement-free tests.
/// Injecting this exercises the exact production create/load/destroy code
/// paths without weakening `KeychainStore` itself.
final class InMemoryKeychainStore: KeychainStoring {
    private var items: [String: Data] = [:]

    func get(account: String) throws -> Data? { items[account] }
    func set(_ data: Data, account: String) throws { items[account] = data }
    func delete(account: String) throws { items[account] = nil }
    func accounts() throws -> [String] { Array(items.keys) }
}

/// Test-only verification helper: verify a raw r‖s signature over `nonce`
/// against an X9.63 public key, exactly as the Rust relay will
/// (`from_sec1_bytes` + `VerifyingKey`).
private func verify(
    publicKeyBase64: String,
    signatureBase64: String,
    nonce: Data
) throws -> Bool {
    let pubData = try #require(Data(base64Encoded: publicKeyBase64))
    let sigData = try #require(Data(base64Encoded: signatureBase64))
    let publicKey = try P256.Signing.PublicKey(x963Representation: pubData)
    let signature = try P256.Signing.ECDSASignature(rawRepresentation: sigData)
    return publicKey.isValidSignature(signature, for: nonce)
}

struct DeviceIdentityTests {

    @Test func createSignVerifyRoundTripViaWireFormat() throws {
        let store = InMemoryKeychainStore()
        let identity = try DeviceIdentity.loadOrCreate(store: store)

        // Random 32-byte nonce → base64 → signBase64 → decode → verify.
        var nonceBytes = [UInt8](repeating: 0, count: 32)
        for i in nonceBytes.indices { nonceBytes[i] = UInt8.random(in: 0...255) }
        let nonce = Data(nonceBytes)
        let nonceBase64 = nonce.base64EncodedString()

        let signatureBase64 = try identity.signBase64(nonceBase64: nonceBase64)

        #expect(try verify(
            publicKeyBase64: identity.publicKeyBase64,
            signatureBase64: signatureBase64,
            nonce: nonce
        ))

        // A tampered nonce must NOT validate.
        var tampered = nonceBytes
        tampered[0] ^= 0xFF
        #expect(try verify(
            publicKeyBase64: identity.publicKeyBase64,
            signatureBase64: signatureBase64,
            nonce: Data(tampered)
        ) == false)
    }

    @Test func deviceIdStableAcrossLoadsAndChangesAfterDestroy() throws {
        let store = InMemoryKeychainStore()

        let first = try DeviceIdentity.loadOrCreate(store: store)
        let firstId = first.deviceId

        // Second load rehydrates the same persisted key.
        let second = try DeviceIdentity.loadOrCreate(store: store)
        #expect(second.deviceId == firstId)

        // Destroy + recreate mints a brand-new key with a different id.
        try DeviceIdentity.destroy(store: store)
        let third = try DeviceIdentity.loadOrCreate(store: store)
        #expect(third.deviceId != firstId)
    }

    @Test func encodingsHaveExactByteShapes() throws {
        let store = InMemoryKeychainStore()
        let identity = try DeviceIdentity.loadOrCreate(store: store)

        // Public key: 65 bytes, uncompressed SEC1 marker 0x04.
        let pub = try #require(Data(base64Encoded: identity.publicKeyBase64))
        #expect(pub.count == 65)
        #expect(pub.first == 0x04)

        // Signature: raw r‖s, exactly 64 bytes.
        let nonce = Data((0..<32).map { _ in UInt8.random(in: 0...255) })
        let sigBase64 = try identity.signBase64(nonceBase64: nonce.base64EncodedString())
        let sig = try #require(Data(base64Encoded: sigBase64))
        #expect(sig.count == 64)

        // deviceId is base64url without padding (no +, /, or =).
        #expect(identity.deviceId.contains("+") == false)
        #expect(identity.deviceId.contains("/") == false)
        #expect(identity.deviceId.contains("=") == false)
    }

    @Test func persistenceReturnsSamePublicKey() throws {
        let store = InMemoryKeychainStore()
        let a = try DeviceIdentity.loadOrCreate(store: store)
        let b = try DeviceIdentity.loadOrCreate(store: store)
        #expect(a.publicKeyBase64 == b.publicKeyBase64)
    }

    @Test func reportsKeyBackingAndPrintsInteropTriple() throws {
        // Report which backing the *real* environment selects. The in-memory
        // store keeps this hermetic; the backing is decided by
        // SecureEnclave.isAvailable, which reflects this simulator/host.
        let store = InMemoryKeychainStore()
        let identity = try DeviceIdentity.loadOrCreate(store: store)

        print("SECURE_ENCLAVE_AVAILABLE=\(SecureEnclave.isAvailable)")
        print("KEY_BACKING=\(identity.backing)")

        // Emit one (publicKey, nonce, signature) triple for the Rust
        // cross-language interop check. Deterministic nonce for easy copying.
        let nonce = Data((0..<32).map { UInt8($0) }) // 0x00..0x1f
        let nonceBase64 = nonce.base64EncodedString()
        let sigBase64 = try identity.signBase64(nonceBase64: nonceBase64)
        print("INTEROP_PUBLIC_KEY_B64=\(identity.publicKeyBase64)")
        print("INTEROP_NONCE_B64=\(nonceBase64)")
        print("INTEROP_SIGNATURE_B64=\(sigBase64)")

        // Sanity: the triple we print must verify locally.
        #expect(try verify(
            publicKeyBase64: identity.publicKeyBase64,
            signatureBase64: sigBase64,
            nonce: nonce
        ))
    }
}
