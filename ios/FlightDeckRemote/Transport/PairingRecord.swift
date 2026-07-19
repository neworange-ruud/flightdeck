//
//  PairingRecord.swift
//  FlightDeckRemote
//
//  The persisted state of an established pairing (REMOTE_PROTOCOL §5.2/§6):
//  everything the transport needs to reconnect, re-derive the E2E channel, and
//  resume queued delivery without losing or duplicating events. A single phone
//  can now pair with MULTIPLE FlightDeck instances at once (multi-pairing,
//  remote-control-b8d), so `PairingRecordStore` is a per-`pairingId` collection
//  rather than a single-record accessor.
//
//  Persistence: one JSON blob PER pairing in the Keychain (via `KeychainStoring`),
//  each under its own account `"pairing-record-v1.<pairingId>"`, written with the
//  same `AfterFirstUnlockThisDeviceOnly` accessibility as the identity key so it
//  can be read on a background wake and never syncs off-device. One item per
//  pairing keeps cursor updates isolated (advancing one pairing's seq never
//  rewrites another's) and reads independent. The KA public key and salt are not
//  secrets that must be hidden from the OS, but living beside the keys keeps
//  unpair (`destroy`) a single coherent wipe.
//
//  Migration: builds up to and including v1 stored a single record under the
//  fixed account `"pairing-record-v1"`. On first read after upgrade the legacy
//  item is migrated into the keyed store crash-safely — write the keyed item and
//  verify the readback FIRST, then delete the legacy item — so an interrupted
//  migration never loses a pairing and re-running is idempotent.
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
    /// The E2E bootstrap salt, base64(standard, padded): ALWAYS the
    /// claim-token UTF-8 bytes, on both the QR and manual-code paths (§7.1,
    /// reconciled contract — the QR `pairing_secret` is wire-compat only and
    /// plays no role in derivation). Never transits the relay's E2E plane.
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

/// Keychain-backed CRUD for the per-`pairingId` collection of `PairingRecord`s.
/// Injectable `KeychainStoring` keeps tests hermetic, matching `DeviceIdentity`.
///
/// The primary API is keyed by `pairingId` (`loadAll`, `load(pairingId:)`,
/// `save`, `delete(pairingId:)`, and the `…pairingId:` cursor helpers). The
/// no-argument convenience methods (`load()`, `delete()`, no-`pairingId` cursor
/// helpers) are transitional shims for the single-pairing call sites that have
/// not yet moved to the coordinator (remote-control-b8d.5); they operate on the
/// first stored record. New code should prefer the keyed API.
final class PairingRecordStore {
    /// Keychain service shared with the identity keys.
    static let service = DeviceIdentity.service
    /// Account prefix for the keyed per-pairing items:
    /// `"pairing-record-v1.<pairingId>"`. The trailing dot keeps these items
    /// disjoint from the legacy account and from other services' items.
    static let accountPrefix = "pairing-record-v1."
    /// Legacy account for the single pre-multi-pairing record. Migrated into the
    /// keyed store on first read after upgrade, then removed.
    static let legacyAccount = "pairing-record-v1"

    /// The Keychain account for a given pairing's record.
    static func account(for pairingId: String) -> String {
        accountPrefix + pairingId
    }

    private let store: KeychainStoring
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    init(store: KeychainStoring = KeychainStore(service: service)) {
        self.store = store
    }

    // MARK: - Keyed API

    /// Every stored pairing record, oldest first (`pairedAt` ascending). Runs the
    /// one-time legacy migration first so an upgraded install sees its existing
    /// pairing immediately.
    func loadAll() throws -> [PairingRecord] {
        try migrateLegacyRecordIfNeeded()
        let keyedAccounts = try store.accounts()
            .filter { $0.hasPrefix(Self.accountPrefix) }
        var records: [PairingRecord] = []
        for account in keyedAccounts {
            guard let blob = try store.get(account: account) else { continue }
            records.append(try decode(blob))
        }
        return records.sorted { $0.pairedAt < $1.pairedAt }
    }

    /// The record for `pairingId`, or `nil` if no such pairing is stored.
    func load(pairingId: String) throws -> PairingRecord? {
        try migrateLegacyRecordIfNeeded()
        guard let blob = try store.get(account: Self.account(for: pairingId)) else {
            return nil
        }
        return try decode(blob)
    }

    /// Persist (create or replace) `record`, keyed by its own `pairingId`.
    func save(_ record: PairingRecord) throws {
        let data = try encoder.encode(record)
        try store.set(data, account: Self.account(for: record.pairingId))
    }

    /// Remove the record for `pairingId` (unpair that one machine). Missing
    /// records are a no-op.
    func delete(pairingId: String) throws {
        try store.delete(account: Self.account(for: pairingId))
    }

    // MARK: - Cursor helpers (keyed)

    /// Advance the persisted outbound cursor for `pairingId` to `seq` (no-op if
    /// not newer). Isolated: touches only that pairing's item.
    @discardableResult
    func setLastSentSeq(_ seq: UInt64, pairingId: String) throws -> PairingRecord? {
        guard var record = try load(pairingId: pairingId) else { return nil }
        if seq > record.lastSentSeq {
            record.lastSentSeq = seq
            try save(record)
        }
        return record
    }

    /// Advance the persisted inbound cursor for `pairingId` to `seq` (no-op if
    /// not newer).
    @discardableResult
    func setLastReceivedSeq(_ seq: UInt64, pairingId: String) throws -> PairingRecord? {
        guard var record = try load(pairingId: pairingId) else { return nil }
        if seq > record.lastReceivedSeq {
            record.lastReceivedSeq = seq
            try save(record)
        }
        return record
    }

    /// Force `pairingId`'s outbound cursor back to 0 after the relay rejected our
    /// stream as non-monotonic — it lost its in-memory watermark (restart/
    /// redeploy) while we kept ours (remote-control-bbf). Unlike `setLastSentSeq`
    /// this is NOT monotonic: it deliberately rewinds so the next command
    /// restarts at seq 1, which a fresh relay accepts.
    @discardableResult
    func resetOutboundCursor(pairingId: String) throws -> PairingRecord? {
        guard var record = try load(pairingId: pairingId) else { return nil }
        record.lastSentSeq = 0
        try save(record)
        return record
    }

    /// Force `pairingId`'s inbound cursor to `seq`, rewinding it if necessary,
    /// when the desktop restarts its outbound stream after a relay seq reset.
    /// Unlike `setLastReceivedSeq` this is NOT monotonic — the new epoch
    /// legitimately begins below the old cursor (remote-control-bbf).
    @discardableResult
    func resetInboundCursor(to seq: UInt64, pairingId: String) throws -> PairingRecord? {
        guard var record = try load(pairingId: pairingId) else { return nil }
        record.lastReceivedSeq = seq
        try save(record)
        return record
    }

    // MARK: - Single-record convenience (transitional)

    /// The first stored pairing record, or `nil` if this device is unpaired.
    /// Transitional shim for single-pairing call sites; prefer `loadAll()` /
    /// `load(pairingId:)`.
    func load() throws -> PairingRecord? {
        try loadAll().first
    }

    /// Remove every stored pairing record (full unpair), including any legacy
    /// item. Transitional shim; prefer `delete(pairingId:)`.
    func delete() throws {
        for account in try store.accounts() where account.hasPrefix(Self.accountPrefix) {
            try store.delete(account: account)
        }
        try store.delete(account: Self.legacyAccount)
    }

    @discardableResult
    func setLastSentSeq(_ seq: UInt64) throws -> PairingRecord? {
        guard let pairingId = try loadAll().first?.pairingId else { return nil }
        return try setLastSentSeq(seq, pairingId: pairingId)
    }

    @discardableResult
    func setLastReceivedSeq(_ seq: UInt64) throws -> PairingRecord? {
        guard let pairingId = try loadAll().first?.pairingId else { return nil }
        return try setLastReceivedSeq(seq, pairingId: pairingId)
    }

    @discardableResult
    func resetOutboundCursor() throws -> PairingRecord? {
        guard let pairingId = try loadAll().first?.pairingId else { return nil }
        return try resetOutboundCursor(pairingId: pairingId)
    }

    @discardableResult
    func resetInboundCursor(to seq: UInt64) throws -> PairingRecord? {
        guard let pairingId = try loadAll().first?.pairingId else { return nil }
        return try resetInboundCursor(to: seq, pairingId: pairingId)
    }

    // MARK: - Migration

    /// Crash-safe, idempotent one-time migration of the legacy single record
    /// into the keyed store: write the keyed item and verify the readback FIRST,
    /// then delete the legacy item. If interrupted before the delete, a re-run
    /// simply repeats the (idempotent) copy and completes the delete, so no
    /// pairing is ever lost. A corrupt legacy blob is left in place and surfaced
    /// as `corruptStoredRecord` rather than silently discarded.
    private func migrateLegacyRecordIfNeeded() throws {
        guard let legacyBlob = try store.get(account: Self.legacyAccount) else {
            return
        }
        let record = try decode(legacyBlob)
        let keyedAccount = Self.account(for: record.pairingId)

        // Write the keyed item first and verify it reads back as the same record.
        let encoded = try encoder.encode(record)
        try store.set(encoded, account: keyedAccount)
        guard let readback = try store.get(account: keyedAccount),
              try decode(readback) == record else {
            // Readback failed — leave the legacy item in place for a future retry.
            throw PairingRecordError.corruptStoredRecord
        }

        // Only now that the keyed copy is durable do we drop the legacy item.
        try store.delete(account: Self.legacyAccount)
    }

    // MARK: - Private

    private func decode(_ blob: Data) throws -> PairingRecord {
        do {
            return try decoder.decode(PairingRecord.self, from: blob)
        } catch {
            throw PairingRecordError.corruptStoredRecord
        }
    }
}
