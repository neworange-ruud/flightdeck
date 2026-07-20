//
//  PairingStore.swift
//  FlightDeckRemote
//
//  Tracks the set of FlightDeck desktop instances this device is paired with
//  (multi-pairing, remote-control-b8d). The persisted source of truth is
//  `[PairedInstance]` — non-secret display/prefs metadata keyed by
//  `pairingId`, joined to the Keychain-backed `PairingRecord`/
//  `PairingRecordStore` (remote-control-b8d.3) which holds the secrets and
//  cursors. This store is `@Observable` so the transport coordinator
//  (b8d.5), the aggregated feed (b8d.6), the router (b8d.7), push (b8d.10),
//  and settings/unpair (b8d.11) all react to `add`/`remove`/setter calls.
//
//  Transitional bridge (remote-control-b8d.4): `isPaired`, `pairedDevice`,
//  `completePairing(with:)`, `unpair()`, and (DEBUG) `debugTogglePaired()`
//  are the pre-multi-pairing single-device API. They are kept, UNCHANGED in
//  behavior and persisted independently via `PairingStateProviding`, so
//  `PairingView`/`SettingsView`/`SettingsUnpairCoordinator` and their existing
//  tests keep compiling and passing without modification. `AppRouter.route`
//  (remote-control-b8d.7) now reads `hasAnyPairing` — count-based off `list`,
//  OR'd with the legacy `isPaired` flag purely so a device paired before
//  multi-pairing (which set `isPaired` but has no `PairedInstance` yet, until
//  `TransportStoreFactory`'s legacy-migration seed runs) still routes to the
//  main tab container. Both the boolean bridge and the persisted list are
//  otherwise updated independently by their respective call sites (see
//  `RealPairingService.pair`, which appends a `PairedInstance` in addition to
//  the `PairedDevice` handed back for `completePairing(with:)`).
//

import Foundation
import Observation

/// Persistence seam for the single-device `isPaired` bridge. `PairingStore`
/// depends on this protocol rather than `UserDefaults` directly, so:
///  - the real Pairing feature can swap in its own backing later, and
///  - tests can inject an in-memory provider (see `InMemoryPairingStateProvider`
///    in `PairingStoreTests`) for hermetic, order-independent runs.
protocol PairingStateProviding {
    func loadIsPaired() -> Bool
    func saveIsPaired(_ isPaired: Bool)
}

/// `UserDefaults`-backed implementation of the `isPaired` bridge.
struct UserDefaultsPairingStateProvider: PairingStateProviding {
    private let defaults: UserDefaults
    private let key = "agency.neworange.flightdeck.remote.isPaired"

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func loadIsPaired() -> Bool {
        defaults.bool(forKey: key)
    }

    func saveIsPaired(_ isPaired: Bool) {
        defaults.set(isPaired, forKey: key)
    }
}

/// Persistence seam for the `[PairedInstance]` metadata list — the multi-
/// pairing source of truth. Separate from `PairingStateProviding` (the
/// single-device boolean bridge) so each can evolve/be tested independently.
protocol PairedInstancesProviding {
    func loadInstances() -> [PairedInstance]
    func saveInstances(_ instances: [PairedInstance])
}

/// `UserDefaults`-backed JSON persistence for `[PairedInstance]`. Everything
/// in `PairedInstance` is non-secret display/prefs metadata (secrets and
/// cursors stay in the Keychain-backed `PairingRecordStore`), so
/// `UserDefaults` is an appropriate backing — no Keychain access needed here.
struct UserDefaultsPairedInstancesProvider: PairedInstancesProviding {
    private let defaults: UserDefaults
    private let key = "agency.neworange.flightdeck.remote.pairedInstances.v1"

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func loadInstances() -> [PairedInstance] {
        guard let data = defaults.data(forKey: key) else { return [] }
        return (try? JSONDecoder().decode([PairedInstance].self, from: data)) ?? []
    }

    func saveInstances(_ instances: [PairedInstance]) {
        guard let data = try? JSONEncoder().encode(instances) else { return }
        defaults.set(data, forKey: key)
    }
}

/// Tracks every FlightDeck desktop instance this device is currently paired
/// with. `[PairedInstance]` (the `list`) is the multi-pairing source of
/// truth; `isPaired`/`pairedDevice` are a transitional single-device bridge
/// kept for existing call sites (see file-level doc comment).
@Observable
final class PairingStore {
    private let storage: PairingStateProviding
    private let instancesStorage: PairedInstancesProviding

    // MARK: - Multi-pairing source of truth

    /// Every paired instance, in the order they were added (oldest first).
    /// Cap enforcement (~3-4 instances) is deferred to remote-control-b8d.7 —
    /// this store places no limit on `add`.
    private(set) var instances: [PairedInstance]

    /// Read-only view of `instances`, for call sites that want the
    /// "consumer" name matching the issue's API shape (transport coordinator,
    /// feed, router, push, settings).
    var list: [PairedInstance] { instances }

    /// Bridge for code that only needs to know "is this device paired with
    /// anything at all" without caring about the transitional single-device
    /// `isPaired` flag — true once at least one instance has been added, or
    /// (until b8d.7 rewires the router) if the legacy `isPaired` flag is set.
    var hasAnyPairing: Bool { isPaired || !instances.isEmpty }

    /// Whether the multi-pairing hard cap (`PairingLimits.maxPairedInstances`,
    /// the SINGLE shared constant — also referenced by `TransportCoordinator`)
    /// has been reached. `PairingView`/`AddMachineSheet` (remote-control-b8d.7)
    /// check this to block STARTING a new pairing rather than letting the
    /// handshake run and only rejecting after the fact.
    var isAtPairingCap: Bool { instances.count >= PairingLimits.maxPairedInstances }

    /// Records a successful pairing transaction: appends a new
    /// `PairedInstance` (or replaces the existing entry for the same
    /// `pairingId`, making a re-pair against an already-known machine
    /// idempotent rather than duplicating it), then persists the list.
    func add(_ instance: PairedInstance) {
        instances.removeAll { $0.pairingId == instance.pairingId }
        instances.append(instance)
        persistInstances()
    }

    /// Removes the instance for `pairingId` (unpair that one machine). A
    /// missing `pairingId` is a no-op.
    func remove(pairingId: String) {
        instances.removeAll { $0.pairingId == pairingId }
        persistInstances()
    }

    /// Sets (or clears, passing `nil`) the user's override name for
    /// `pairingId` — always wins in `PairedInstance.displayName` (remote-
    /// control-b8d.9's naming UI). No-op if `pairingId` isn't known.
    func setOverrideName(pairingId: String, _ name: String?) {
        updateInstance(pairingId: pairingId) { $0.userOverrideName = name }
    }

    /// Mutes/unmutes push notifications from `pairingId` (remote-control-
    /// b8d.10). No-op if `pairingId` isn't known.
    func setMutePush(pairingId: String, _ mute: Bool) {
        updateInstance(pairingId: pairingId) { $0.mutePush = mute }
    }

    /// Records the machine name most recently reported by the desktop for
    /// `pairingId` (re-sent every connect, remote-control-b8d.1/.9). No-op if
    /// `pairingId` isn't known.
    func setMachineName(pairingId: String, _ name: String?) {
        updateInstance(pairingId: pairingId) { $0.machineNameFromDesktop = name }
    }

    /// Records whether `pairingId` was reachable the last time its connection
    /// was checked (remote-control-b8d.5's coordinator/b8d.6's feed drive
    /// this). No-op if `pairingId` isn't known.
    func setLastKnownOnline(pairingId: String, _ online: Bool) {
        updateInstance(pairingId: pairingId) { $0.lastKnownOnline = online }
    }

    private func updateInstance(pairingId: String, _ mutate: (inout PairedInstance) -> Void) {
        guard let index = instances.firstIndex(where: { $0.pairingId == pairingId }) else { return }
        mutate(&instances[index])
        persistInstances()
    }

    private func persistInstances() {
        instancesStorage.saveInstances(instances)
    }

    // MARK: - Transitional single-device bridge

    /// Legacy single-device paired flag. `AppRouter` no longer reads this
    /// directly (remote-control-b8d.7: it reads `hasAnyPairing`, which still
    /// ORs this in) — kept for `PairingView.pair(with:)`/`SettingsUnpairCoordinator`,
    /// which still flip it via `completePairing(with:)`/`unpair()`.
    var isPaired: Bool {
        didSet {
            guard isPaired != oldValue else { return }
            storage.saveIsPaired(isPaired)
        }
    }

    /// Metadata about the most recently paired Mac, set by
    /// `completePairing(with:)`. Kept for `SettingsView`'s "connected device"
    /// surface until it reads from `list` instead (b8d.7+); NOT persisted
    /// (unlike `isPaired`) — only the paired boolean needs to survive
    /// relaunch for routing, while the durable per-instance metadata now
    /// lives in `list`/`instances`.
    private(set) var pairedDevice: PairedDevice?

    init(
        storage: PairingStateProviding = UserDefaultsPairingStateProvider(),
        instancesStorage: PairedInstancesProviding = UserDefaultsPairedInstancesProvider()
    ) {
        self.storage = storage
        self.instancesStorage = instancesStorage
        self.instances = instancesStorage.loadInstances()
        var initial = storage.loadIsPaired()
        #if DEBUG
        // UI-test hook: pairing state persists across launches (by design),
        // so UI tests pass `-uitest-reset-pairing` to start each scenario
        // from a known unpaired state. Resets the *persisted* value too so a
        // test that toggles pairing can't leak state into later launches or
        // later test runs.
        if ProcessInfo.processInfo.arguments.contains("-uitest-reset-pairing") {
            initial = false
            storage.saveIsPaired(false)
            // Multi-pairing (remote-control-b8d.4+) added a persisted
            // [PairedInstance] list; routing now keys off `hasAnyPairing`
            // (isPaired || !instances.isEmpty), so the reset must clear the
            // instance list too — otherwise a leaked instance from a prior
            // launch keeps the app on the feed and the Pairing screen (with the
            // DEBUG toggle UI tests rely on) never appears.
            self.instances = []
            instancesStorage.saveInstances([])
        }
        // Doc-screenshot / UI-test hook: seed a deterministic multi-machine
        // list so the Settings "Machines" card and the feed's machine chips
        // render the real multi-pairing UI (rename/mute/unpair rows, distinct
        // machine names, an offline machine) without a live relay. Seeding
        // `instances` is the same path production takes at launch
        // (`instancesStorage.loadInstances()`), so it can't hang; the coordinator
        // just reports each seeded machine `.disconnected` (no persisted
        // PairingRecord). Runs AFTER the reset block so it wins over it, and
        // overwrites persisted state so it's hermetic on its own.
        if ProcessInfo.processInfo.arguments.contains("-uitest-fixture-machines") {
            let seeded = Self.uiTestFixtureMachines
            self.instances = seeded
            instancesStorage.saveInstances(seeded)
        }
        #endif
        self.isPaired = initial
    }

    #if DEBUG
    /// Deterministic paired-machine list for `-uitest-fixture-machines` (doc
    /// screenshots + Settings/feed UI tests): two online Macs with distinct
    /// desktop-reported names and one muted, offline Mac — enough to show the
    /// online/offline dot, the per-machine mute bell, and the rename/unpair
    /// affordances. No secrets: the E2E key material lives in `PairingRecord`,
    /// absent here, so each seeded machine simply resolves to `.disconnected`.
    static let uiTestFixtureMachines: [PairedInstance] = [
        PairedInstance(
            pairingId: "uitest-machine-studio",
            machineNameFromDesktop: "Studio",
            relayURL: URL(string: "wss://relay.flightdeck.dev")!,
            lastKnownOnline: true),
        PairedInstance(
            pairingId: "uitest-machine-mbp",
            machineNameFromDesktop: "MacBook Pro",
            relayURL: URL(string: "wss://relay.flightdeck.dev")!,
            lastKnownOnline: true),
        PairedInstance(
            pairingId: "uitest-machine-mini",
            machineNameFromDesktop: "Mac mini",
            relayURL: URL(string: "wss://relay.flightdeck.dev")!,
            mutePush: true,
            lastKnownOnline: false),
    ]
    #endif

    /// Records a successful pairing transaction (see `PairingServicing`) and
    /// flips `isPaired` — `AppRouter`/`RootView` react automatically and
    /// swap the Pairing screen for the main tab container (PRD §5.8).
    ///
    /// This is the transitional single-device bridge: it does NOT itself
    /// append to `list` — the real relay-backed `RealPairingService.pair`
    /// does that directly (via its own `pairingStore.add(_:)` call) since it
    /// alone knows the pairing's `relayURL`.
    func completePairing(with device: PairedDevice) {
        pairedDevice = device
        isPaired = true
    }

    /// Reverses `completePairing(with:)`: clears both the paired flag and
    /// the in-memory device metadata (PRD §5.6/§8 "Unpair this device").
    /// `AppRouter`/`RootView` react to `isPaired` flipping and swap back to
    /// the Pairing screen. Called by `SettingsUnpairCoordinator`
    /// (Features/Settings/) as one step of the full unpair sequence, which
    /// also clears the Keychain-backed `PairingRecord`/`KeyAgreementKeys` —
    /// this method only owns this store's own in-memory/`UserDefaults`
    /// state. Does NOT remove anything from `list`/`instances` — unpairing
    /// one machine out of several is `remove(pairingId:)`'s job
    /// (remote-control-b8d.11).
    func unpair() {
        pairedDevice = nil
        isPaired = false
    }

    #if DEBUG
    /// DEBUG-only manual toggle (PRD navigation task): lets a developer
    /// cross the unpaired/paired boundary in the simulator without a real
    /// pairing flow, and lets UI tests do the same deterministically.
    /// No-op in Release builds — there is no way to reach this from
    /// production UI.
    func debugTogglePaired() {
        isPaired.toggle()
    }
    #endif
}
