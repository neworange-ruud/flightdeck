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

    private func sampleRecord() -> PairingRecord {
        PairingRecord(
            pairingId: "pair_1",
            peerDeviceId: "desktop_1",
            peerKeyAgreementPublicKeyB64: Data((0..<65).map { UInt8($0) }).base64EncodedString(),
            saltB64: Data("salt-bytes".utf8).base64EncodedString(),
            relayURL: "wss://relay.example/v1",
            lastSentSeq: 3,
            lastReceivedSeq: 9,
            pairedAt: Date(timeIntervalSince1970: 1_752_000_000)
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
        try keychain.set(Data("not json".utf8), account: PairingRecordStore.account)
        let store = PairingRecordStore(store: keychain)
        #expect(throws: PairingRecordError.self) { _ = try store.load() }
    }
}
