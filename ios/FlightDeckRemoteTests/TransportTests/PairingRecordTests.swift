//
//  PairingRecordTests.swift
//  FlightDeckRemoteTests
//
//  Persistence round-trip and cursor semantics for `PairingRecord` /
//  `PairingRecordStore` (REMOTE_PROTOCOL §5.2/§6).
//

import Testing
import Foundation
@testable import FlightDeckRemote

@Suite struct PairingRecordTests {

    private func sampleRecord(
        pairingId: String = "pair_1",
        peerDeviceId: String? = "desktop_1",
        lastSentSeq: UInt64 = 3,
        lastReceivedSeq: UInt64 = 9,
        pairedAt: Date = Date(timeIntervalSince1970: 1_752_000_000)
    ) -> PairingRecord {
        PairingRecord(
            pairingId: pairingId,
            peerDeviceId: peerDeviceId,
            peerKeyAgreementPublicKeyB64: Data((0..<65).map { UInt8($0) }).base64EncodedString(),
            saltB64: Data("salt-bytes".utf8).base64EncodedString(),
            relayURL: "wss://relay.example/v1",
            lastSentSeq: lastSentSeq,
            lastReceivedSeq: lastReceivedSeq,
            pairedAt: pairedAt
        )
    }

    @Test func saveLoadRoundTrip() throws {
        let store = PairingRecordStore(store: InMemoryKeychainStore())
        #expect(try store.load() == nil)

        let record = sampleRecord()
        try store.save(record)
        let loaded = try #require(try store.load())
        #expect(loaded == record)
        #expect(loaded.salt == Data("salt-bytes".utf8))
    }

    @Test func deleteRemovesTheRecord() throws {
        let store = PairingRecordStore(store: InMemoryKeychainStore())
        try store.save(sampleRecord())
        try store.delete()
        #expect(try store.load() == nil)
    }

    @Test func cursorsAdvanceOnlyForward() throws {
        let store = PairingRecordStore(store: InMemoryKeychainStore())
        try store.save(sampleRecord()) // lastSent 3, lastReceived 9

        try store.setLastReceivedSeq(9)   // equal → no change
        #expect(try store.load()?.lastReceivedSeq == 9)
        try store.setLastReceivedSeq(12)  // forward
        #expect(try store.load()?.lastReceivedSeq == 12)
        try store.setLastReceivedSeq(11)  // backward → ignored
        #expect(try store.load()?.lastReceivedSeq == 12)

        try store.setLastSentSeq(4)
        #expect(try store.load()?.lastSentSeq == 4)
        try store.setLastSentSeq(2)       // backward → ignored
        #expect(try store.load()?.lastSentSeq == 4)
    }

    @Test func corruptBlobThrows() throws {
        let keychain = InMemoryKeychainStore()
        try keychain.set(Data("not json".utf8), account: PairingRecordStore.account(for: "pair_1"))
        let store = PairingRecordStore(store: keychain)
        #expect(throws: PairingRecordError.self) { _ = try store.load(pairingId: "pair_1") }
    }

    // MARK: - Multi-pairing keyed API

    @Test func multiRecordRoundTripIsIsolatedByPairingId() throws {
        let store = PairingRecordStore(store: InMemoryKeychainStore())
        let a = sampleRecord(pairingId: "pair_a", peerDeviceId: "mac_a",
                             pairedAt: Date(timeIntervalSince1970: 1_752_000_000))
        let b = sampleRecord(pairingId: "pair_b", peerDeviceId: "mac_b",
                             pairedAt: Date(timeIntervalSince1970: 1_752_000_100))
        try store.save(a)
        try store.save(b)

        // loadAll returns both, oldest first.
        let all = try store.loadAll()
        #expect(all.count == 2)
        #expect(all.map(\.pairingId) == ["pair_a", "pair_b"])

        // load(pairingId:) isolates each record.
        #expect(try store.load(pairingId: "pair_a") == a)
        #expect(try store.load(pairingId: "pair_b") == b)
        #expect(try store.load(pairingId: "pair_missing") == nil)
    }

    @Test func keyedCursorUpdateDoesNotTouchOtherPairings() throws {
        let store = PairingRecordStore(store: InMemoryKeychainStore())
        try store.save(sampleRecord(pairingId: "pair_a", lastSentSeq: 3, lastReceivedSeq: 9))
        try store.save(sampleRecord(pairingId: "pair_b", lastSentSeq: 3, lastReceivedSeq: 9))

        try store.setLastSentSeq(7, pairingId: "pair_a")
        try store.setLastReceivedSeq(15, pairingId: "pair_a")

        #expect(try store.load(pairingId: "pair_a")?.lastSentSeq == 7)
        #expect(try store.load(pairingId: "pair_a")?.lastReceivedSeq == 15)
        // pair_b is untouched.
        #expect(try store.load(pairingId: "pair_b")?.lastSentSeq == 3)
        #expect(try store.load(pairingId: "pair_b")?.lastReceivedSeq == 9)

        // Non-monotonic reset rewinds only the targeted pairing.
        try store.resetOutboundCursor(pairingId: "pair_a")
        try store.resetInboundCursor(to: 2, pairingId: "pair_a")
        #expect(try store.load(pairingId: "pair_a")?.lastSentSeq == 0)
        #expect(try store.load(pairingId: "pair_a")?.lastReceivedSeq == 2)
        #expect(try store.load(pairingId: "pair_b")?.lastSentSeq == 3)
    }

    @Test func deleteByPairingIdRemovesOnlyThatRecord() throws {
        let store = PairingRecordStore(store: InMemoryKeychainStore())
        try store.save(sampleRecord(pairingId: "pair_a"))
        try store.save(sampleRecord(pairingId: "pair_b"))

        try store.delete(pairingId: "pair_a")

        #expect(try store.load(pairingId: "pair_a") == nil)
        #expect(try store.load(pairingId: "pair_b") != nil)
        #expect(try store.loadAll().map(\.pairingId) == ["pair_b"])
    }

    // MARK: - Legacy migration

    @Test func legacyRecordIsMigratedIntoKeyedStore() throws {
        let keychain = InMemoryKeychainStore()
        // Seed a legacy single record at the old fixed account.
        let legacy = sampleRecord(pairingId: "pair_legacy")
        let blob = try JSONEncoder().encode(legacy)
        try keychain.set(blob, account: PairingRecordStore.legacyAccount)

        let store = PairingRecordStore(store: keychain)

        // It surfaces via the new keyed API without re-pairing…
        let all = try store.loadAll()
        #expect(all == [legacy])
        #expect(try store.load(pairingId: "pair_legacy") == legacy)

        // …the keyed item now exists…
        #expect(try keychain.get(account: PairingRecordStore.account(for: "pair_legacy")) != nil)
        // …and the legacy account has been cleared.
        #expect(try keychain.get(account: PairingRecordStore.legacyAccount) == nil)
    }

    @Test func migrationIsIdempotentAndSafeToReRun() throws {
        let keychain = InMemoryKeychainStore()
        let legacy = sampleRecord(pairingId: "pair_legacy")
        try keychain.set(try JSONEncoder().encode(legacy), account: PairingRecordStore.legacyAccount)

        let store = PairingRecordStore(store: keychain)
        _ = try store.loadAll()          // first migration
        _ = try store.loadAll()          // re-run must be a no-op

        #expect(try store.loadAll() == [legacy])
        #expect(try keychain.get(account: PairingRecordStore.legacyAccount) == nil)
    }

    @Test func migrationCoexistsWithAnExistingKeyedRecord() throws {
        let keychain = InMemoryKeychainStore()
        let existing = sampleRecord(pairingId: "pair_new",
                                    pairedAt: Date(timeIntervalSince1970: 1_752_000_500))
        let legacy = sampleRecord(pairingId: "pair_legacy",
                                  pairedAt: Date(timeIntervalSince1970: 1_752_000_000))
        let store = PairingRecordStore(store: keychain)
        try store.save(existing)
        try keychain.set(try JSONEncoder().encode(legacy), account: PairingRecordStore.legacyAccount)

        let all = try store.loadAll()
        #expect(all.map(\.pairingId) == ["pair_legacy", "pair_new"]) // oldest first
        #expect(try keychain.get(account: PairingRecordStore.legacyAccount) == nil)
    }
}
