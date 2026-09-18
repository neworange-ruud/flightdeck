//
//  TransportStoreFactoryTests.swift
//  FlightDeckRemoteTests
//
//  Covers launch-time reconciliation between UserDefaults pairing metadata and
//  the Keychain records the transport actually needs to connect.
//

import Foundation
import Testing
@testable import FlightDeckRemote

@MainActor
@Suite struct TransportStoreFactoryTests {
    private func record(_ pairingId: String, pairedAt: Date = Date()) -> PairingRecord {
        PairingRecord(
            pairingId: pairingId,
            peerDeviceId: "desktop_\(pairingId)",
            peerKeyAgreementPublicKeyB64: Data(repeating: 1, count: 65).base64EncodedString(),
            saltB64: Data("salt".utf8).base64EncodedString(),
            relayURL: "wss://relay.example/ws",
            pairedAt: pairedAt
        )
    }

    private func instance(_ pairingId: String, name: String? = nil) -> PairedInstance {
        PairedInstance(
            pairingId: pairingId,
            machineNameFromDesktop: name,
            relayURL: URL(string: "wss://relay.example/ws")!
        )
    }

    @Test func orphanedMetadataReturnsAppToPairing() {
        let pairingStore = PairingStore(
            storage: InMemoryPairingStateProvider(initial: true),
            instancesStorage: InMemoryPairedInstancesProvider(initial: [instance("orphan")])
        )

        TransportStoreFactory.reconcilePersistedPairings(
            in: pairingStore,
            recordStore: PairingRecordStore(store: InMemoryKeychainStore())
        )

        #expect(pairingStore.list.isEmpty)
        #expect(!pairingStore.isPaired)
        #expect(!pairingStore.hasAnyPairing)
    }

    @Test func validRecordsPreserveMetadataAndSeedMissingInstances() throws {
        let keychain = InMemoryKeychainStore()
        let recordStore = PairingRecordStore(store: keychain)
        let existing = record("existing", pairedAt: Date(timeIntervalSince1970: 10))
        let missing = record("missing", pairedAt: Date(timeIntervalSince1970: 20))
        try recordStore.save(existing)
        try recordStore.save(missing)
        let pairingStore = PairingStore(
            storage: InMemoryPairingStateProvider(initial: true),
            instancesStorage: InMemoryPairedInstancesProvider(initial: [
                instance("existing", name: "Keep this name"),
                instance("orphan")
            ])
        )

        TransportStoreFactory.reconcilePersistedPairings(
            in: pairingStore,
            recordStore: recordStore
        )

        #expect(pairingStore.list.map(\.pairingId) == ["existing", "missing"])
        #expect(pairingStore.list.first?.machineNameFromDesktop == "Keep this name")
        #expect(pairingStore.list.last?.pairedAt == missing.pairedAt)
        #expect(pairingStore.hasAnyPairing)
    }

    @Test func keychainReadFailureDoesNotClearMetadata() throws {
        let keychain = InMemoryKeychainStore()
        try keychain.set(
            Data("not json".utf8),
            account: PairingRecordStore.account(for: "pair_a")
        )
        let pairingStore = PairingStore(
            storage: InMemoryPairingStateProvider(initial: true),
            instancesStorage: InMemoryPairedInstancesProvider(initial: [instance("pair_a")])
        )

        TransportStoreFactory.reconcilePersistedPairings(
            in: pairingStore,
            recordStore: PairingRecordStore(store: keychain)
        )

        #expect(pairingStore.list.map(\.pairingId) == ["pair_a"])
        #expect(pairingStore.isPaired)
    }
}
