//
//  PairingRecord.swift
//  FlightDeckRemote
//
//  The persisted state of an established pairing (REMOTE_PROTOCOL §5.2/§6):
//  everything the transport needs to reconnect, re-derive the E2E channel, and
//  resume queued delivery without losing or duplicating events. In v1 the phone
//  holds at most one pairing (single-Mac UI, §10), so `PairingRecordStore`
//  exposes a single-record accessor — but the record is keyed by `pairingId`
//  so the model already generalizes to the multi-pairing wire format.
//
//  Persistence: one JSON blob in the Keychain (via `KeychainStoring`), written
//  with the same `AfterFirstUnlockThisDeviceOnly` accessibility as the identity
//  key so it can be read on a background wake and never syncs off-device. The
//  KA public key and salt are not secrets that must be hidden from the OS, but
//  living beside the keys keeps unpair (`destroy`) a single coherent wipe.
//

import Foundation

/// The persisted state for one phone ↔ Mac pairing.
struct PairingRecord: Codable, Equatable, Sendable {
    /// The relay-assigned pairing id — all routing is keyed by this (§5.2).
    var pairingId: String
    /// The peer (desktop) device id, if the relay reported one at claim time.
    var peerDeviceId: String?
    /// The desktop's key-agreement public key, base64(standard, padded) X9.63
    /// (`pairing_claimed.peer_key_agreement_public_key`, §5.2). Fed to
    /// `E2EChannel.derive` as the peer public key.
    var peerKeyAgreementPublicKeyB64: String
    /// The E2E bootstrap salt, base64(standard, padded): the decoded QR
    /// `pairing_secret`, or the claim-token UTF-8 bytes for the code path
    /// (§7.1). Never transits the relay.
    var saltB64: String
    /// The relay endpoint this pairing connects to.
    var relayURL: String
    /// Highest outbound (phone→desktop) envelope `seq` durably sent. The next
    /// send uses `lastSentSeq + 1` and persists only after the send succeeds so
    /// the peer's dedup never stalls on a gap (§6.1, mirrors the desktop).
    var lastSentSeq: UInt64
    /// Highest inbound (desktop→phone) envelope `seq` durably handled. Drives
    /// `resume { from_seq }` and cumulative `ack` (§6.2/§6.3).
    var lastReceivedSeq: UInt64
    /// When this pairing was established.
    var pairedAt: Date

    init(
        pairingId: String,
        peerDeviceId: String?,
        peerKeyAgreementPublicKeyB64: String,
        saltB64: String,
        relayURL: String,
        lastSentSeq: UInt64 = 0,
        lastReceivedSeq: UInt64 = 0,
        pairedAt: Date = Date()
    ) {
        self.pairingId = pairingId
        self.peerDeviceId = peerDeviceId
        self.peerKeyAgreementPublicKeyB64 = peerKeyAgreementPublicKeyB64
        self.saltB64 = saltB64
        self.relayURL = relayURL
        self.lastSentSeq = lastSentSeq
        self.lastReceivedSeq = lastReceivedSeq
        self.pairedAt = pairedAt
    }

    /// The decoded E2E bootstrap salt bytes.
    var salt: Data {
        Data(base64Encoded: saltB64) ?? Data()
    }
}

/// Errors from persisting or rehydrating a `PairingRecord`.
enum PairingRecordError: Error, Equatable {
    /// The stored blob could not be decoded as a `PairingRecord`.
    case corruptStoredRecord
}

/// Keychain-backed CRUD for the (single, in v1) `PairingRecord`. Injectable
/// `KeychainStoring` keeps tests hermetic, matching `DeviceIdentity`.
final class PairingRecordStore {
    /// Keychain service shared with the identity keys.
    static let service = DeviceIdentity.service
    /// Keychain account under which the single pairing record JSON is stored.
    static let account = "pairing-record-v1"

    private let store: KeychainStoring
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    init(store: KeychainStoring = KeychainStore(service: service)) {
        self.store = store
    }

    /// The current pairing record, or `nil` if this device is unpaired.
    func load() throws -> PairingRecord? {
        guard let blob = try store.get(account: Self.account) else { return nil }
        do {
            return try decoder.decode(PairingRecord.self, from: blob)
        } catch {
            throw PairingRecordError.corruptStoredRecord
        }
    }

    /// Persist (create or replace) the pairing record.
    func save(_ record: PairingRecord) throws {
        let data = try encoder.encode(record)
        try store.set(data, account: Self.account)
    }

    /// Remove the pairing record (unpair).
    func delete() throws {
        try store.delete(account: Self.account)
    }

    // MARK: - Cursor helpers

    /// Advance the persisted outbound cursor to `seq` (no-op if not newer).
    @discardableResult
    func setLastSentSeq(_ seq: UInt64) throws -> PairingRecord? {
        guard var record = try load() else { return nil }
        if seq > record.lastSentSeq {
            record.lastSentSeq = seq
            try save(record)
        }
        return record
    }

    /// Advance the persisted inbound cursor to `seq` (no-op if not newer).
    @discardableResult
    func setLastReceivedSeq(_ seq: UInt64) throws -> PairingRecord? {
        guard var record = try load() else { return nil }
        if seq > record.lastReceivedSeq {
            record.lastReceivedSeq = seq
            try save(record)
        }
        return record
    }
}
