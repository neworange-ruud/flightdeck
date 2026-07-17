//
//  DeviceIdentity.swift
//  FlightDeckRemote
//
//  The per-device identity keypair used to authenticate this phone to the
//  relay (REMOTE_PROTOCOL §5.1, PRD §9). The signing algorithm is ECDSA over
//  NIST P-256 with SHA-256 — chosen because the private key must be
//  Secure-Enclave-resident and the Enclave only supports P-256.
//
//  Wire encodings (normative, REMOTE_PROTOCOL §5.1):
//   - public key: base64(standard, padded) of the X9.63 uncompressed SEC1
//     point (65 bytes, 0x04 ‖ x ‖ y) == CryptoKit `x963Representation`.
//   - signature: base64(standard, padded) of raw r ‖ s (64 bytes) ==
//     CryptoKit `ECDSASignature.rawRepresentation`.
//   - message signed: the exact decoded nonce bytes (CryptoKit hashes with
//     SHA-256 internally as part of standard ECDSA signing).
//

import Foundation
import CryptoKit

/// Which key store backs the current identity. The Secure Enclave is
/// preferred; the software key is a fallback for environments where the
/// Enclave is unavailable (many simulators, some CI hosts).
enum KeyBacking: Equatable {
    case secureEnclave
    case software
}

/// Errors from creating, loading, or using the device identity.
enum DeviceIdentityError: Error, Equatable {
    /// The stored key blob was empty or its backing tag byte was unrecognized.
    case corruptStoredKey
    /// A base64 string on the wire could not be decoded (e.g. a bad nonce).
    case invalidBase64
}

/// The device's persistent signing identity.
///
/// Create-or-load: `loadOrCreate()` generates a P-256 signing key in the
/// Secure Enclave on first use (falling back to a software key when the
/// Enclave is unavailable) and persists its `dataRepresentation` in the
/// Keychain. Subsequent launches rehydrate the same key, so `deviceId` and
/// `publicKeyBase64` are stable until `destroy()` (unpair) is called.
struct DeviceIdentity {
    /// Keychain service shared by all identity items.
    static let service = "agency.neworange.flightdeck.remote"
    /// Keychain account under which the private-key blob is stored.
    static let keyAccount = "device-identity-key"

    /// Tag byte prefixed to the stored blob so we know how to rehydrate it.
    private enum BackingTag: UInt8 {
        case secureEnclave = 0x01
        case software = 0x02
    }

    /// One of the two CryptoKit signing key types, unified behind a small
    /// closure-based facade so the rest of the type is backing-agnostic.
    private enum SigningKey {
        case secureEnclave(SecureEnclave.P256.Signing.PrivateKey)
        case software(P256.Signing.PrivateKey)

        var publicKey: P256.Signing.PublicKey {
            switch self {
            case .secureEnclave(let k): return k.publicKey
            case .software(let k): return k.publicKey
            }
        }

        func signature(for data: Data) throws -> P256.Signing.ECDSASignature {
            switch self {
            case .secureEnclave(let k): return try k.signature(for: data)
            case .software(let k): return try k.signature(for: data)
            }
        }
    }

    private let key: SigningKey

    /// Which key store backs this identity.
    let backing: KeyBacking

    // MARK: - Lifecycle

    /// Loads the persisted identity, or creates and persists a new one on
    /// first use. Prefers a Secure Enclave key; falls back to a software key
    /// when `SecureEnclave.isAvailable` is false.
    static func loadOrCreate(
        store: KeychainStoring = KeychainStore(service: service)
    ) throws -> DeviceIdentity {
        if let blob = try store.get(account: keyAccount) {
            return try DeviceIdentity(storedBlob: blob)
        }
        return try create(store: store)
    }

    /// Generates a fresh key, persists it, and returns the identity.
    private static func create(store: KeychainStoring) throws -> DeviceIdentity {
        if SecureEnclave.isAvailable {
            let seKey = try SecureEnclave.P256.Signing.PrivateKey()
            try store.set(
                tagged(.secureEnclave, seKey.dataRepresentation),
                account: keyAccount
            )
            return DeviceIdentity(key: .secureEnclave(seKey), backing: .secureEnclave)
        } else {
            // Software keys expose `rawRepresentation` (the 32-byte scalar);
            // `dataRepresentation` is Secure-Enclave-only.
            let swKey = P256.Signing.PrivateKey()
            try store.set(
                tagged(.software, swKey.rawRepresentation),
                account: keyAccount
            )
            return DeviceIdentity(key: .software(swKey), backing: .software)
        }
    }

    /// Rehydrates from the tagged blob persisted by `create`.
    private init(storedBlob blob: Data) throws {
        guard let first = blob.first,
              let tag = BackingTag(rawValue: first) else {
            throw DeviceIdentityError.corruptStoredKey
        }
        let payload = blob.dropFirst()
        switch tag {
        case .secureEnclave:
            let k = try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: payload)
            self.key = .secureEnclave(k)
            self.backing = .secureEnclave
        case .software:
            let k = try P256.Signing.PrivateKey(rawRepresentation: payload)
            self.key = .software(k)
            self.backing = .software
        }
    }

    private init(key: SigningKey, backing: KeyBacking) {
        self.key = key
        self.backing = backing
    }

    /// Removes the persisted identity (unpair). After this, `loadOrCreate`
    /// mints a brand-new key with a different `deviceId`.
    static func destroy(
        store: KeychainStoring = KeychainStore(service: service)
    ) throws {
        try store.delete(account: keyAccount)
    }

    // MARK: - Public identity

    /// The raw X9.63 (SEC1 uncompressed) public-key bytes: 65 bytes, 0x04‖x‖y.
    private var publicKeyX963: Data {
        key.publicKey.x963Representation
    }

    /// Stable device id: base64url-without-padding of SHA-256 of the X9.63
    /// public key. Deterministic from the key — no separately stored id.
    var deviceId: String {
        let digest = SHA256.hash(data: publicKeyX963)
        return Data(digest).base64URLEncodedStringNoPadding()
    }

    /// Public key for the wire: base64(standard, padded) of X9.63 bytes.
    var publicKeyBase64: String {
        publicKeyX963.base64EncodedString()
    }

    // MARK: - Signing

    /// Signs the given nonce bytes, returning the raw r‖s form (64 bytes).
    func sign(nonce: Data) throws -> Data {
        try key.signature(for: nonce).rawRepresentation
    }

    /// Wire-format convenience: decode a base64(standard) nonce, sign it, and
    /// return the base64(standard, padded) raw r‖s signature.
    func signBase64(nonceBase64: String) throws -> String {
        guard let nonce = Data(base64Encoded: nonceBase64) else {
            throw DeviceIdentityError.invalidBase64
        }
        return try sign(nonce: nonce).base64EncodedString()
    }

    // MARK: - Helpers

    private static func tagged(_ tag: BackingTag, _ payload: Data) -> Data {
        var out = Data([tag.rawValue])
        out.append(payload)
        return out
    }
}

extension Data {
    /// base64url per RFC 4648 §5 with padding removed (`-`/`_`, no `=`).
    func base64URLEncodedStringNoPadding() -> String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
