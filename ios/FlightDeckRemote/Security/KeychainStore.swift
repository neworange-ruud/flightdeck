//
//  KeychainStore.swift
//  FlightDeckRemote
//
//  Minimal typed Keychain wrapper used by DeviceIdentity to persist the
//  per-device identity private key (PRD §9, REMOTE_PROTOCOL §5.1).
//
//  We store the key blob as a `kSecClassGenericPassword` item rather than a
//  `kSecClassKey` item. Rationale: what we persist is CryptoKit's opaque
//  `dataRepresentation` — for a Secure Enclave key this is a *wrapped* key
//  reference the Enclave produces (the private key material never leaves the
//  Enclave), and for the software fallback it is the raw scalar. Neither is a
//  DER/PKCS#8-shaped `SecKey` blob, so `kSecClassKey` (which wants a real
//  `SecKeyCreateWithData` payload) is the wrong fit; a generic-password item
//  is the idiomatic home for an opaque application secret keyed by account.
//

import Foundation
import Security

/// Errors surfaced by `KeychainStore`. `status` carries the raw
/// `OSStatus` from the Security framework for diagnostics.
enum KeychainError: Error, Equatable {
    /// A `SecItem*` call failed with an unexpected status.
    case unexpectedStatus(OSStatus)
    /// The stored item existed but its data could not be read back as `Data`.
    case dataConversionFailed
}

/// Abstraction over the Keychain so tests can inject an in-memory store
/// without weakening the production path (see task note on entitlements).
protocol KeychainStoring {
    /// Returns the stored bytes for `account`, or `nil` if no item exists.
    func get(account: String) throws -> Data?
    /// Stores `data` for `account`, replacing any existing item.
    func set(_ data: Data, account: String) throws
    /// Deletes the item for `account`. Missing items are treated as success.
    func delete(account: String) throws
    /// Returns the account strings of every item stored under this service.
    /// Used to enumerate a keyed collection (e.g. the per-pairing records
    /// keyed by `pairingId`). Order is unspecified.
    func accounts() throws -> [String]
}

/// Concrete Keychain-backed store. Items are scoped to one service string and
/// keyed by an account string. All items are written with
/// `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`:
///  - *AfterFirstUnlock*: the relay can wake the app in the background to
///    reconnect, at which point we must be able to read the key and sign the
///    auth nonce even though the user has not just unlocked the phone.
///  - *ThisDeviceOnly*: the identity key must never sync to iCloud Keychain or
///    migrate to a restored/other device — the pairing is bound to this device.
struct KeychainStore: KeychainStoring {
    let service: String

    init(service: String) {
        self.service = service
    }

    private func baseQuery(account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    func get(account: String) throws -> Data? {
        var query = baseQuery(account: account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne

        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            guard let data = result as? Data else {
                throw KeychainError.dataConversionFailed
            }
            return data
        case errSecItemNotFound:
            return nil
        default:
            throw KeychainError.unexpectedStatus(status)
        }
    }

    func set(_ data: Data, account: String) throws {
        // Delete-then-add keeps the accessibility attribute authoritative and
        // avoids merging attributes from a prior item.
        try delete(account: account)

        var attributes = baseQuery(account: account)
        attributes[kSecValueData as String] = data
        attributes[kSecAttrAccessible as String] =
            kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly

        let status = SecItemAdd(attributes as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw KeychainError.unexpectedStatus(status)
        }
    }

    func delete(account: String) throws {
        let status = SecItemDelete(baseQuery(account: account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw KeychainError.unexpectedStatus(status)
        }
    }

    func accounts() throws -> [String] {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecReturnAttributes as String: true,
            kSecMatchLimit as String: kSecMatchLimitAll,
        ]
        query[kSecReturnData as String] = false

        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            let items = (result as? [[String: Any]]) ?? []
            return items.compactMap { $0[kSecAttrAccount as String] as? String }
        case errSecItemNotFound:
            return []
        default:
            throw KeychainError.unexpectedStatus(status)
        }
    }
}
