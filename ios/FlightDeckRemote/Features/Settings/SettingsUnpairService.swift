//
//  SettingsUnpairService.swift
//  FlightDeckRemote
//
//  The concrete steps performed when the user unpairs this device from
//  Settings (PRD §5.6/§8: "Unpair this device" → confirm → back to the
//  Pairing screen). Abstracted behind `SettingsUnpairing` — mirroring every
//  other seam in this app (`PairingServicing`, `AppLockSettingsProviding`,
//  `BiometricAuthenticating`) — so `SettingsUnpairCoordinator`'s exact
//  ordering can be unit-tested with a mock, without a live `TransportClient`
//  or real Keychain/Secure-Enclave access (see `SettingsUnpairServiceTests`).
//
//  What's destroyed vs. kept, and why:
//   - `PairingRecord` (Keychain) — deleted. It's this pairing's session
//     state (seq cursors, peer key-agreement key, relay URL); meaningless
//     once unpaired.
//   - `KeyAgreementKeys` (Keychain) — destroyed. It's the E2E bootstrap
//     keypair for *this* pairing's channel; a fresh pairing derives a fresh
//     channel from a fresh key (`KeyAgreementKeys.loadOrCreate`), so keeping
//     the old one around serves no purpose and needlessly retains key
//     material for a channel that no longer exists.
//   - `DeviceIdentity` (Keychain / Secure Enclave) — KEPT, deliberately, and
//     therefore never referenced by this file. It's the phone's stable
//     identity to the relay (REMOTE_PROTOCOL §5.1) — `deviceId` is derived
//     from it. Destroying it on every unpair would mint a new device
//     identity on every re-pair, which nothing in the protocol keys off of
//     today, and would discard the Secure-Enclave-resident key for no
//     security benefit: unpairing revokes the *pairing*, not the phone's
//     identity. It is only ever expected to be destroyed by a full app data
//     reset (uninstall), not by this flow.
//

import Foundation

/// Abstracts the destructive steps of unpairing so `SettingsUnpairCoordinator`
/// can be driven end-to-end with a mock in tests.
protocol SettingsUnpairing {
    /// Stops the live transport connection (if running) so no further
    /// traffic is sent once we're unpairing.
    func stopTransport() async
    /// Deletes the persisted `PairingRecord` (Keychain).
    func deletePairingRecord() throws
    /// Destroys the key-agreement keypair (Keychain). A fresh pairing mints
    /// a fresh one via `KeyAgreementKeys.loadOrCreate`.
    func destroyKeyAgreementKeys() throws
}

/// Production implementation, composing the same primitives
/// `TransportStoreFactory`/`RealPairingService` use elsewhere. `DeviceIdentity`
/// is deliberately NOT touched here — see the file-level doc comment.
struct DefaultSettingsUnpairService: SettingsUnpairing {
    var transportStore: TransportStore
    var pairingRecordStore: PairingRecordStore = PairingRecordStore()

    func stopTransport() async {
        await transportStore.stop()
    }

    func deletePairingRecord() throws {
        try pairingRecordStore.delete()
    }

    func destroyKeyAgreementKeys() throws {
        try KeyAgreementKeys.destroy()
    }
}

/// Runs the unpair sequence in the documented order (PRD §8): stop the
/// transport, delete the pairing record, flip `PairingStore` back to
/// unpaired (which `AppRouter`/`RootView` react to by swapping back to the
/// Pairing screen), then destroy the key-agreement keys.
///
/// Keychain deletes are best-effort (`try?`) — `KeychainStore.delete`
/// already treats a missing item as success, but an unpair should never get
/// "stuck" on a later step because an earlier one hit an unexpected
/// Keychain error; the user's own observable state (`pairingStore.isPaired`)
/// is the one step that must always run.
@MainActor
enum SettingsUnpairCoordinator {
    static func run(service: SettingsUnpairing, pairingStore: PairingStore) async {
        await service.stopTransport()
        try? service.deletePairingRecord()
        pairingStore.unpair()
        try? service.destroyKeyAgreementKeys()
    }
}
