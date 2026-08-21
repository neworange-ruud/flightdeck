//
//  PairingUnpairServiceTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the per-machine unpair path (remote-control-b8d.11):
//  `PairingUnpairCoordinator` sequences the five steps scoped to ONE
//  `pairingId` — best-effort relay revoke, delete that pairing's record, remove
//  its `PairedInstance`, stop its client — and only destroys the SHARED
//  key-agreement keys (and flips the router bridge back to onboarding) when the
//  LAST pairing is removed. Driven with a mock `PairingUnpairing` so the
//  ordering + guard are exercised without a live transport or real Keychain,
//  plus one real `DefaultPairingUnpairService` check that record deletion is
//  keyed to the target pairing and revoke is a no-op when nothing is live.
//

import Testing
import Foundation
@testable import FlightDeckRemote

/// Records call order + per-step `pairingId`s, and lets the revoke result be
/// scripted (to simulate the relay being unreachable — a `false` must still let
/// local removal proceed).
@MainActor
final class MockPairingUnpairService: PairingUnpairing {
    private(set) var callOrder: [String] = []
    private(set) var revokedPairingIds: [String] = []
    private(set) var deletedRecordPairingIds: [String] = []
    private(set) var stoppedPairingIds: [String] = []
    private(set) var destroySharedKeysCallCount = 0

    /// What `revoke` returns — `false` simulates "relay unreachable".
    var revokeResult = true
    /// If set, `deletePairingRecord` throws it (best-effort — removal proceeds).
    var deleteRecordError: Error?

    func revoke(pairingId: String) async -> Bool {
        callOrder.append("revoke")
        revokedPairingIds.append(pairingId)
        return revokeResult
    }

    func stopTransport(pairingId: String) async {
        callOrder.append("stopTransport")
        stoppedPairingIds.append(pairingId)
    }

    func deletePairingRecord(pairingId: String) throws {
        callOrder.append("deletePairingRecord")
        deletedRecordPairingIds.append(pairingId)
        if let deleteRecordError { throw deleteRecordError }
    }

    func destroySharedKeyAgreementKeys() throws {
        callOrder.append("destroySharedKeyAgreementKeys")
        destroySharedKeysCallCount += 1
    }
}

private struct StubUnpairError: Error {}

@MainActor
struct PairingUnpairServiceTests {

    private let relayURL = URL(string: "wss://relay.flightdeck.app/v1")!

    private func makeStore(pairingIds: [String]) -> PairingStore {
        let instances = pairingIds.enumerated().map { index, id in
            PairedInstance(
                pairingId: id,
                relayURL: relayURL,
                pairedAt: Date(timeIntervalSince1970: TimeInterval(1_000 + index)))
        }
        return PairingStore(
            storage: InMemoryPairingStateProvider(initial: true),
            instancesStorage: InMemoryPairedInstancesProvider(initial: instances))
    }

    // MARK: - Unpair one of several

    @Test func unpairingOneOfSeveralRemovesOnlyThatInstanceAndRetainsSharedKeys() async {
        let store = makeStore(pairingIds: ["p1", "p2"])
        let service = MockPairingUnpairService()

        await PairingUnpairCoordinator.run(pairingId: "p1", service: service, pairingStore: store)

        // Only p1 is gone; p2 remains.
        #expect(store.list.map(\.pairingId) == ["p2"])
        // Every destructive step targeted exactly p1.
        #expect(service.revokedPairingIds == ["p1"])
        #expect(service.deletedRecordPairingIds == ["p1"])
        #expect(service.stoppedPairingIds == ["p1"])
        // Shared keys RETAINED — another pairing still needs them.
        #expect(service.destroySharedKeysCallCount == 0)
        // Router bridge untouched — still paired with p2.
        #expect(store.isPaired == true)
        #expect(store.hasAnyPairing == true)
    }

    @Test func unpairingOneSendsRevokeBeforeTearingDownTheClient() async {
        let store = makeStore(pairingIds: ["p1", "p2"])
        let service = MockPairingUnpairService()

        await PairingUnpairCoordinator.run(pairingId: "p1", service: service, pairingStore: store)

        // Revoke must go out FIRST (while the client is still live), then the
        // record delete, then the client teardown; no shared-key destroy.
        #expect(service.callOrder == ["revoke", "deletePairingRecord", "stopTransport"])
    }

    // MARK: - Unpair the last machine

    @Test func unpairingLastMachineDestroysSharedKeysAndReturnsToOnboarding() async {
        let store = makeStore(pairingIds: ["p1"])
        let service = MockPairingUnpairService()

        await PairingUnpairCoordinator.run(pairingId: "p1", service: service, pairingStore: store)

        #expect(store.list.isEmpty)
        // Shared keys destroyed exactly once — this was the last pairing.
        #expect(service.destroySharedKeysCallCount == 1)
        // Legacy router bridge flipped so `hasAnyPairing` is false → onboarding.
        #expect(store.isPaired == false)
        #expect(store.hasAnyPairing == false)
        // Destroy happens AFTER the per-pairing teardown.
        #expect(service.callOrder == [
            "revoke", "deletePairingRecord", "stopTransport", "destroySharedKeyAgreementKeys"
        ])
    }

    // MARK: - Best-effort / idempotent revoke

    @Test func relayUnreachableStillRemovesLocally() async {
        let store = makeStore(pairingIds: ["p1", "p2"])
        let service = MockPairingUnpairService()
        service.revokeResult = false // relay unreachable / no live session

        await PairingUnpairCoordinator.run(pairingId: "p1", service: service, pairingStore: store)

        // A failed revoke must NOT block local removal (§5.8 idempotent).
        #expect(service.revokedPairingIds == ["p1"])
        #expect(store.list.map(\.pairingId) == ["p2"])
        #expect(service.deletedRecordPairingIds == ["p1"])
        #expect(service.stoppedPairingIds == ["p1"])
    }

    @Test func keychainDeleteThrowStillRemovesInstance() async {
        let store = makeStore(pairingIds: ["p1", "p2"])
        let service = MockPairingUnpairService()
        service.deleteRecordError = StubUnpairError()

        await PairingUnpairCoordinator.run(pairingId: "p1", service: service, pairingStore: store)

        // Best-effort record delete threw, yet the observable removal still ran.
        #expect(store.list.map(\.pairingId) == ["p2"])
        #expect(service.stoppedPairingIds == ["p1"])
    }

    @Test func unknownPairingIdIsANoOpForOthers() async {
        let store = makeStore(pairingIds: ["p1", "p2"])
        let service = MockPairingUnpairService()

        // Revoke idempotency: an id not in the store still runs the (harmless)
        // steps and never disturbs the known pairings.
        await PairingUnpairCoordinator.run(pairingId: "ghost", service: service, pairingStore: store)

        #expect(store.list.map(\.pairingId) == ["p1", "p2"])
        #expect(service.destroySharedKeysCallCount == 0)
        #expect(store.isPaired == true)
    }

    // MARK: - Real service: keyed record deletion + no-op revoke without a client

    @Test func defaultServiceDeletesOnlyTargetRecordAndRevokeIsNoOpWhenNotLive() async throws {
        let keychain = InMemoryKeychainStore()
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        let keyAgreement = try KeyAgreementKeys.loadOrCreate(store: keychain)
        let recordStore = PairingRecordStore(store: keychain)

        func record(_ id: String, at seconds: TimeInterval) -> PairingRecord {
            PairingRecord(
                pairingId: id,
                peerDeviceId: nil,
                peerKeyAgreementPublicKeyB64: "",
                saltB64: "",
                relayURL: "wss://relay.example/\(id)",
                pairedAt: Date(timeIntervalSince1970: seconds))
        }
        try recordStore.save(record("p1", at: 1))
        try recordStore.save(record("p2", at: 2))

        // A coordinator with NO active handles — so `client(for:)` is nil and
        // revoke is a best-effort no-op returning false.
        let coordinator = TransportCoordinator(
            identity: identity,
            keyAgreement: keyAgreement,
            recordStore: recordStore,
            connectorFactory: { ScriptedConnector(channel: ScriptedChannel()) })

        let service = DefaultPairingUnpairService(coordinator: coordinator, pairingRecordStore: recordStore)

        let sent = await service.revoke(pairingId: "p1")
        #expect(sent == false, "no live client → revoke sends nothing but never blocks removal")

        try service.deletePairingRecord(pairingId: "p1")

        // Only p1's record was removed; p2's survives (keyed deletion, b8d.3).
        #expect(try recordStore.load(pairingId: "p1") == nil)
        #expect(try recordStore.load(pairingId: "p2") != nil)
    }
}
