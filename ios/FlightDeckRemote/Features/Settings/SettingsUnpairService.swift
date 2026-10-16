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

// MARK: - Per-pairing unpair (multi-pairing, remote-control-b8d.11)

/// Abstracts the per-machine unpair steps so `PairingUnpairCoordinator` can be
/// driven end-to-end with a mock — the multi-pairing counterpart to
/// `SettingsUnpairing`, but every step is scoped to ONE `pairingId` and the
/// shared-key destruction is a SEPARATE step the coordinator only invokes when
/// the last pairing is being removed. `@MainActor` because the production
/// implementation drives the `@MainActor` `TransportCoordinator`.
@MainActor
protocol PairingUnpairing {
    /// Best-effort phone→relay `revoke` for `pairingId` via THAT pairing's live
    /// client (spec §5.8). Never throws and never blocks local removal — returns
    /// whether a frame was actually sent (diagnostic only); a `false` (relay
    /// unreachable) still proceeds to local removal.
    @discardableResult
    func revoke(pairingId: String) async -> Bool
    /// Stop + dispose ONLY this pairing's `TransportClient`, leaving every other
    /// live client untouched.
    func stopTransport(pairingId: String) async
    /// Delete ONLY this pairing's Keychain `PairingRecord`.
    func deletePairingRecord(pairingId: String) throws
    /// Destroy the SHARED per-device key-agreement keypair. Invoked by the
    /// coordinator ONLY when the last pairing is removed — never on a
    /// unpair-one-of-many. The per-device `DeviceIdentity` (Secure Enclave) is
    /// deliberately never destroyed here (see `DefaultSettingsUnpairService`'s
    /// file-level note): unpair revokes pairings, not the phone's identity.
    func destroySharedKeyAgreementKeys() throws
}

/// Production per-pairing unpair, composed over the live `TransportCoordinator`
/// (for the revoke + client teardown) and the keyed `PairingRecordStore`.
@MainActor
struct DefaultPairingUnpairService: PairingUnpairing {
    let coordinator: TransportCoordinator
    var pairingRecordStore: PairingRecordStore = PairingRecordStore()

    @discardableResult
    func revoke(pairingId: String) async -> Bool {
        // Relay always; plus the Mac over the direct link for a direct-mode
        // pairing (spec §5.10) — see `TransportStore.revokePairing`.
        await coordinator.revoke(pairingId: pairingId)
    }

    func stopTransport(pairingId: String) async {
        await coordinator.stop(pairingId: pairingId)
    }

    func deletePairingRecord(pairingId: String) throws {
        try pairingRecordStore.delete(pairingId: pairingId)
    }

    func destroySharedKeyAgreementKeys() throws {
        try KeyAgreementKeys.destroy()
    }
}

/// Runs the unpair-one-machine sequence (remote-control-b8d.11) in a fixed
/// order that keeps the OTHER pairings — and the phone's shared device keys —
/// working:
///
///   1. Best-effort relay `revoke` FIRST, while this pairing's client is still
///      live (so the desktop learns it's unpaired). If the relay is unreachable
///      this is a no-op — removal never blocks on the network (§5.8 idempotent).
///   2. Delete only this pairing's Keychain record (best-effort).
///   3. Remove only its `PairedInstance` — `@Observable`, so the router, feed,
///      push, and the transport coordinator's `reconcile` all react.
///   4. Stop + dispose only its `TransportClient`.
///   5. ONLY if this was the LAST pairing (`list.isEmpty` after step 3): destroy
///      the shared key-agreement keypair AND flip the legacy `isPaired` bridge
///      so `AppRouter` (routing on `hasAnyPairing`) returns to onboarding. While
///      any other pairing remains, the shared keys are RETAINED untouched.
@MainActor
enum PairingUnpairCoordinator {
    static func run(
        pairingId: String,
        service: PairingUnpairing,
        pairingStore: PairingStore
    ) async {
        await service.revoke(pairingId: pairingId)      // 1
        try? service.deletePairingRecord(pairingId: pairingId)  // 2
        pairingStore.remove(pairingId: pairingId)       // 3
        await service.stopTransport(pairingId: pairingId)       // 4
        if pairingStore.list.isEmpty {                  // 5 — last pairing only
            try? service.destroySharedKeyAgreementKeys()
            pairingStore.unpair()
        }
    }
}
