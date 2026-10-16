//
//  KeychainStore.swift
//  FlightDeckRemote + FlightDeckNotificationService
//
//  Minimal typed Keychain wrapper used by DeviceIdentity to persist the
//  per-device identity private key (PRD §9, REMOTE_PROTOCOL §5.1), and by the
//  key-agreement key and pairing records the Notification Service Extension
//  reads (remote-control-bza0).
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
///  - *AfterFirstUnlock*: the Notification Service Extension must be able to
///    read the pairing material to decrypt a push that lands while the phone is
///    locked (after its first unlock since boot).
///  - *ThisDeviceOnly*: the identity key must never sync to iCloud Keychain or
///    migrate to a restored/other device — the pairing is bound to this device.
///
/// Access groups (remote-control-bza0): `accessGroupForAccount` picks the group
/// a NEW item is written into — the shared group for the extension's material
/// (`SharedContainer.accessGroup`), the default group otherwise. Reads, deletes
/// and enumeration deliberately pass NO access group, which on iOS searches
/// every group the process is entitled to: an item written before the shared
/// group existed is still found, and `delete` removes every copy.
struct KeychainStore: KeychainStoring {
    let service: String
    private let accessGroupForAccount: (String) -> String?

    init(
        service: String,
        accessGroupForAccount: ((String) -> String?)? = nil
    ) {
        self.service = service
        self.accessGroupForAccount = accessGroupForAccount ?? { account in
            SharedContainer.accessGroup(service: service, account: account)
        }
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
        // avoids merging attributes from a prior item. The delete spans every
        // access group, so no stale copy survives in the old group.
        try delete(account: account)

        var attributes = baseQuery(account: account)
        attributes[kSecValueData as String] = data
        attributes[kSecAttrAccessible as String] =
            kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly

        if let group = accessGroupForAccount(account) {
            var grouped = attributes
            grouped[kSecAttrAccessGroup as String] = group
            let status = SecItemAdd(grouped as CFDictionary, nil)
            if status == errSecSuccess { return }
            // Not entitled to the shared group (an unsigned or ad-hoc simulator
            // build, or a profile missing the group): keep the app working by
            // writing to the default group. The extension then can't decrypt
            // pushes for this pairing and shows the generic text — the same
            // fallback as any other extension failure.
            guard status == errSecMissingEntitlement else {
                throw KeychainError.unexpectedStatus(status)
            }
        }

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
            return KeychainGroupMigration.uniqueAccounts(
                items.compactMap { $0[kSecAttrAccount as String] as? String })
        case errSecItemNotFound:
            return []
        default:
            throw KeychainError.unexpectedStatus(status)
        }
    }
}

// MARK: - Shared access group migration (remote-control-bza0)

/// One stored item as the migration sees it.
struct KeychainItemLocation: Equatable {
    let account: String
    /// The item's access group; `nil` if the Keychain did not report one.
    let accessGroup: String?
}

/// The access-group-aware Keychain operations the migration needs, behind a seam
/// so the algorithm is unit-testable with an in-memory multi-group double (the
/// unsigned simulator test host has no Keychain entitlement at all).
protocol KeychainGroupMigrating {
    /// Every item under the service, with its access group.
    func locations() throws -> [KeychainItemLocation]
    /// The data of the item for `account` in exactly `accessGroup`.
    func data(account: String, accessGroup: String?) throws -> Data?
    /// Add `data` for `account` into `accessGroup`. Returns `false` when an
    /// item already exists there (not an error: an earlier, interrupted run
    /// already copied it).
    func add(_ data: Data, account: String, accessGroup: String) throws -> Bool
    /// Delete the item for `account` in exactly `accessGroup`.
    func delete(account: String, accessGroup: String?) throws
}

enum KeychainGroupMigration {
    /// Outcome counts, for logging and tests.
    struct Report: Equatable {
        var moved: Int = 0
        var alreadyShared: Int = 0
    }

    /// Move every item `shouldMove` selects into `targetGroup`, crash-safely:
    /// copy into the target group and verify the readback FIRST, then delete
    /// the original. An interruption at any point leaves at least one intact
    /// copy (reads search every group), and a re-run completes the move — so an
    /// upgrading user never loses a pairing and never has to re-pair.
    ///
    /// Any thrown error (typically `errSecMissingEntitlement` in a build without
    /// the shared group) aborts the run with the originals untouched; the caller
    /// simply tries again on the next launch.
    @discardableResult
    static func run(
        store: KeychainGroupMigrating,
        targetGroup: String,
        shouldMove: (String) -> Bool
    ) throws -> Report {
        var report = Report()
        for location in try store.locations() where shouldMove(location.account) {
            if location.accessGroup == targetGroup {
                report.alreadyShared += 1
                continue
            }
            guard let data = try store.data(account: location.account, accessGroup: location.accessGroup) else {
                continue
            }
            // `false` means a copy is already in the target group (an earlier run
            // was interrupted before its delete). The app's `set` deletes every
            // copy before writing, so that shared copy is the newer or an
            // identical one: keep it and drop the stale original below.
            _ = try store.add(data, account: location.account, accessGroup: targetGroup)
            guard try store.data(account: location.account, accessGroup: targetGroup) != nil else {
                // Readback failed: leave the original in place for a retry.
                continue
            }
            try store.delete(account: location.account, accessGroup: location.accessGroup)
            report.moved += 1
        }
        return report
    }

    /// `accounts` with duplicates removed, first occurrence wins. While an item
    /// is being moved it briefly exists in two groups under one account, and a
    /// caller enumerating a keyed collection must still see it once.
    static func uniqueAccounts(_ accounts: [String]) -> [String] {
        var seen = Set<String>()
        return accounts.filter { seen.insert($0).inserted }
    }
}

/// The real `KeychainGroupMigrating`, over one service.
struct SystemKeychainGroupMigrator: KeychainGroupMigrating {
    let service: String

    func locations() throws -> [KeychainItemLocation] {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecReturnAttributes as String: true,
            kSecMatchLimit as String: kSecMatchLimitAll,
        ]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            return ((result as? [[String: Any]]) ?? []).compactMap { item in
                guard let account = item[kSecAttrAccount as String] as? String else { return nil }
                return KeychainItemLocation(
                    account: account,
                    accessGroup: item[kSecAttrAccessGroup as String] as? String)
            }
        case errSecItemNotFound:
            return []
        default:
            throw KeychainError.unexpectedStatus(status)
        }
    }

    private func query(account: String, accessGroup: String?) -> [String: Any] {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        if let accessGroup { query[kSecAttrAccessGroup as String] = accessGroup }
        return query
    }

    func data(account: String, accessGroup: String?) throws -> Data? {
        var query = query(account: account, accessGroup: accessGroup)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            guard let data = result as? Data else { throw KeychainError.dataConversionFailed }
            return data
        case errSecItemNotFound:
            return nil
        default:
            throw KeychainError.unexpectedStatus(status)
        }
    }

    func add(_ data: Data, account: String, accessGroup: String) throws -> Bool {
        var attributes = query(account: account, accessGroup: accessGroup)
        attributes[kSecValueData as String] = data
        attributes[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(attributes as CFDictionary, nil)
        switch status {
        case errSecSuccess: return true
        case errSecDuplicateItem: return false
        default: throw KeychainError.unexpectedStatus(status)
        }
    }

    func delete(account: String, accessGroup: String?) throws {
        let status = SecItemDelete(query(account: account, accessGroup: accessGroup) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw KeychainError.unexpectedStatus(status)
        }
    }
}
