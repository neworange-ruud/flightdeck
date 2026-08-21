//
//  KeyAgreementKeys.swift
//  FlightDeckRemote
//
//  The per-device *key-agreement* keypair used to bootstrap the end-to-end
//  channel (REMOTE_PROTOCOL §5.2 / §7.1). This is deliberately SEPARATE from
//  the `DeviceIdentity` signing key:
//
//   - `DeviceIdentity` is a Secure-Enclave ECDSA P-256 *signing* key. The
//     Enclave performs signatures but never applies its scalar to ECDH, so it
//     cannot be used for key agreement.
//   - This type is therefore a *software* P-256 key-agreement key
//     (`P256.KeyAgreement.PrivateKey`), whose raw 32-byte scalar is available
//     for `E2EChannel.derive(...)`. Its public point is sent to the desktop as
//     `pairing_claim.key_agreement_public_key`.
//
//  Persistence mirrors `DeviceIdentity` exactly: the raw scalar is stored in
//  the Keychain with `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` +
//  `ThisDeviceOnly` (via `KeychainStore`), so the key survives relaunch, can
//  be read after a background wake, and never syncs off-device.
//
//  Wire encoding for the public key (REMOTE_PROTOCOL §5.2): base64(standard,
//  padded) of the X9.63 uncompressed SEC1 point (65 bytes, 0x04 ‖ x ‖ y) —
//  identical to `DeviceIdentity.publicKeyBase64`.
//

import Foundation
import CryptoKit

/// The device's persistent P-256 key-agreement keypair (software-backed).
///
/// Create-or-load: `loadOrCreate()` generates a fresh key on first use and
/// persists its 32-byte `rawRepresentation` scalar in the Keychain. Subsequent
/// launches rehydrate the same key, so `publicKeyBase64` and the derived E2E
/// channel are stable until `destroy()` (unpair) is called.
struct KeyAgreementKeys {
    /// Keychain service shared with `DeviceIdentity` (same app secret namespace).
    static let service = DeviceIdentity.service
    /// Keychain account under which the raw key-agreement scalar is stored.
    static let keyAccount = "device-key-agreement-key"

    private let privateKey: P256.KeyAgreement.PrivateKey

    // MARK: - Lifecycle

    /// Loads the persisted key-agreement key, or creates and persists a new one
    /// on first use.
    static func loadOrCreate(
        store: KeychainStoring = KeychainStore(service: service)
    ) throws -> KeyAgreementKeys {
        if let blob = try store.get(account: keyAccount) {
            do {
                let key = try P256.KeyAgreement.PrivateKey(rawRepresentation: blob)
                return KeyAgreementKeys(privateKey: key)
            } catch {
                throw DeviceIdentityError.corruptStoredKey
            }
        }
        let key = P256.KeyAgreement.PrivateKey()
        try store.set(key.rawRepresentation, account: keyAccount)
        return KeyAgreementKeys(privateKey: key)
    }

    private init(privateKey: P256.KeyAgreement.PrivateKey) {
        self.privateKey = privateKey
    }

    /// Removes the persisted key-agreement key (unpair). After this,
    /// `loadOrCreate` mints a brand-new key.
    static func destroy(
        store: KeychainStoring = KeychainStore(service: service)
    ) throws {
        try store.delete(account: keyAccount)
    }

    // MARK: - Material

    /// The raw 32-byte private scalar, fed to `E2EChannel.derive(...)` as the
    /// local key-agreement private key.
    var privateScalar: Data {
        privateKey.rawRepresentation
    }

    /// The raw X9.63 (SEC1 uncompressed) public-key bytes: 65 bytes, 0x04‖x‖y.
    var publicKeyX963: Data {
        privateKey.publicKey.x963Representation
    }

    /// Public key for the wire: base64(standard, padded) of the X9.63 bytes.
    /// Sent as `pairing_claim.key_agreement_public_key` (REMOTE_PROTOCOL §5.2).
    var publicKeyBase64: String {
        publicKeyX963.base64EncodedString()
    }
}
