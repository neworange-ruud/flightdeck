//
//  RelayPasswordStore.swift
//  FlightDeckRemote
//
//  Secure storage for the OPTIONAL shared relay password (remote-control-uq7).
//  The relay's Azure Container Apps IP allowlist was replaced by a shared
//  password: a relay configured with `FLIGHTDECK_RELAY_PASSWORD` gates the
//  WebSocket `hello` on it (constant-time compare on the relay), so the phone
//  must present the same value on EVERY connect — at pairing time and on every
//  later reconnect (pairing itself happens over the relay). A relay with no
//  password ignores the field, so this is `nil` for local/dev relays.
//
//  It is a coarse, shared network-admission secret — NOT a per-device auth
//  credential — but it is still stored in the Keychain (never `UserDefaults`)
//  and never logged, alongside the device identity and pairing records under
//  the same service, with the same `AfterFirstUnlockThisDeviceOnly`
//  accessibility (background relay wakes must be able to read it). One item for
//  the whole app: the phone talks to a single relay, so the password is not
//  per-pairing.
//

import Foundation

/// Keychain-backed load/save for the shared relay password. Injectable
/// `KeychainStoring` keeps tests hermetic, matching `DeviceIdentity` /
/// `PairingRecordStore`.
struct RelayPasswordStore {
    /// Keychain service shared with the identity keys / pairing records.
    static let service = DeviceIdentity.service
    /// The single account holding the shared relay password.
    static let account = "relay-password-v1"

    private let store: KeychainStoring

    init(store: KeychainStoring = KeychainStore(service: service)) {
        self.store = store
    }

    /// The stored relay password, or `nil` when none is set (unconfigured /
    /// local relay) or the stored bytes aren't valid UTF-8. An empty string is
    /// normalized to `nil` so callers never present a present-but-empty
    /// password that a configured relay would reject.
    func load() -> String? {
        guard
            let data = try? store.get(account: Self.account),
            let value = String(data: data, encoding: .utf8),
            !value.isEmpty
        else { return nil }
        return value
    }

    /// Persist `password`, or clear it when `nil`/blank. Trims surrounding
    /// whitespace (a pasted password often drags a trailing space/newline);
    /// a blank result deletes the item rather than storing "".
    func save(_ password: String?) throws {
        let trimmed = password?.trimmingCharacters(in: .whitespacesAndNewlines)
        if let trimmed, !trimmed.isEmpty {
            try store.set(Data(trimmed.utf8), account: Self.account)
        } else {
            try store.delete(account: Self.account)
        }
    }

    /// Remove any stored relay password (part of a full unpair/wipe).
    func delete() throws {
        try store.delete(account: Self.account)
    }
}
