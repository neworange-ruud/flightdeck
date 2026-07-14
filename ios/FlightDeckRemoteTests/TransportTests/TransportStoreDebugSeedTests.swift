//
//  TransportStoreDebugSeedTests.swift
//  FlightDeckRemoteTests
//
//  Covers the additive `TransportStore.debugSeed` DEBUG seam (Projects/
//  Sessions fixture rendering): it force-sets `snapshot`/`linkState` directly
//  without touching the real `TransportClient`.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@MainActor
@Suite struct TransportStoreDebugSeedTests {

    private func makeStore() throws -> TransportStore {
        let keychain = InMemoryKeychainStore()
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        let keyAgreement = try KeyAgreementKeys.loadOrCreate(store: keychain)
        let recordStore = PairingRecordStore(store: keychain)
        let client = TransportClient(
            identity: identity, keyAgreement: keyAgreement, recordStore: recordStore,
            connector: ScriptedConnector(channel: ScriptedChannel()))
        return TransportStore(client: client)
    }

    @Test func debugSeedSetsSnapshotAndLinkState() throws {
        let store = try makeStore()
        #expect(store.snapshot == nil)
        #expect(store.linkState == .disconnected)

        let snapshot = Wire.StateSnapshot(serverTimeMs: 1, projects: [])
        store.debugSeed(snapshot: snapshot, linkState: .connected(latencyMs: 5))

        #expect(store.snapshot == snapshot)
        #expect(store.linkState == .connected(latencyMs: 5))
    }

    @Test func debugSeedDefaultsToAConnectedLinkState() throws {
        let store = try makeStore()
        store.debugSeed(snapshot: Wire.StateSnapshot(serverTimeMs: 1, projects: []))

        guard case .connected = store.linkState else {
            Issue.record("expected a connected link state by default, got \(store.linkState)")
            return
        }
    }
}
