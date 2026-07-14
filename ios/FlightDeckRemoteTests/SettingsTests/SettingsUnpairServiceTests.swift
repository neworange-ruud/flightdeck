//
//  SettingsUnpairServiceTests.swift
//  FlightDeckRemoteTests
//
//  Verifies `SettingsUnpairCoordinator` runs the documented unpair sequence
//  (PRD §8) in order — stop transport, delete the pairing record, flip
//  `PairingStore` back to unpaired, destroy the key-agreement keys — using
//  a mock `SettingsUnpairing` so the ordering is exercised without a live
//  `TransportClient` or real Keychain access (mirrors `InMemoryKeychainStore`
//  / `InMemoryPairingStateProvider`'s hermetic-test pattern elsewhere).
//

import Testing
@testable import FlightDeckRemote

/// Records call order and lets each step assert on `pairingStore`'s state at
/// the moment it fires, so the test can verify the router-flip step
/// (`pairingStore.unpair()`) happens exactly where the coordinator promises:
/// after the pairing record delete, before the key-agreement key destroy.
@MainActor
final class MockSettingsUnpairService: SettingsUnpairing {
    private(set) var callOrder: [String] = []
    var onStopTransport: (() -> Void)?
    var onDeletePairingRecord: (() -> Void)?
    var onDestroyKeyAgreementKeys: (() -> Void)?

    func stopTransport() async {
        callOrder.append("stopTransport")
        onStopTransport?()
    }

    func deletePairingRecord() throws {
        callOrder.append("deletePairingRecord")
        onDeletePairingRecord?()
    }

    func destroyKeyAgreementKeys() throws {
        callOrder.append("destroyKeyAgreementKeys")
        onDestroyKeyAgreementKeys?()
    }
}

private struct StubError: Error {}

@MainActor
struct SettingsUnpairServiceTests {

    @Test func runsStepsInDocumentedOrder() async {
        let pairingStore = PairingStore(storage: InMemoryPairingStateProvider(initial: true))
        pairingStore.completePairing(
            with: PairedDevice(pairingId: "p1", peerName: "Ruud's MacBook Pro", pairedAt: .now)
        )
        let service = MockSettingsUnpairService()

        await SettingsUnpairCoordinator.run(service: service, pairingStore: pairingStore)

        #expect(service.callOrder == ["stopTransport", "deletePairingRecord", "destroyKeyAgreementKeys"])
    }

    @Test func flipsPairingStoreAfterDeletingRecordButBeforeDestroyingKeys() async {
        let pairingStore = PairingStore(storage: InMemoryPairingStateProvider(initial: true))
        pairingStore.completePairing(
            with: PairedDevice(pairingId: "p1", peerName: "Ruud's MacBook Pro", pairedAt: .now)
        )
        let service = MockSettingsUnpairService()

        var isPairedWhenRecordDeleted: Bool?
        var isPairedWhenKeysDestroyed: Bool?
        service.onDeletePairingRecord = { isPairedWhenRecordDeleted = pairingStore.isPaired }
        service.onDestroyKeyAgreementKeys = { isPairedWhenKeysDestroyed = pairingStore.isPaired }

        await SettingsUnpairCoordinator.run(service: service, pairingStore: pairingStore)

        #expect(isPairedWhenRecordDeleted == true, "The router flip should not have happened yet when the pairing record is deleted")
        #expect(isPairedWhenKeysDestroyed == false, "The router flip must happen before the key-agreement keys are destroyed")
    }

    @Test func clearsIsPairedAndPairedDevice() async {
        let pairingStore = PairingStore(storage: InMemoryPairingStateProvider(initial: true))
        pairingStore.completePairing(
            with: PairedDevice(pairingId: "p1", peerName: "Ruud's MacBook Pro", pairedAt: .now)
        )
        let service = MockSettingsUnpairService()

        await SettingsUnpairCoordinator.run(service: service, pairingStore: pairingStore)

        #expect(pairingStore.isPaired == false)
        #expect(pairingStore.pairedDevice == nil)
    }

    @Test func stillFlipsRouterWhenKeychainDeletesThrow() async {
        // Keychain deletes are best-effort — an unpair should never get
        // "stuck" mid-sequence because an earlier delete failed.
        final class ThrowingService: SettingsUnpairing {
            func stopTransport() async {}
            func deletePairingRecord() throws { throw StubError() }
            func destroyKeyAgreementKeys() throws { throw StubError() }
        }

        let pairingStore = PairingStore(storage: InMemoryPairingStateProvider(initial: true))
        pairingStore.completePairing(
            with: PairedDevice(pairingId: "p1", peerName: "Ruud's MacBook Pro", pairedAt: .now)
        )

        await SettingsUnpairCoordinator.run(service: ThrowingService(), pairingStore: pairingStore)

        #expect(pairingStore.isPaired == false)
    }
}
