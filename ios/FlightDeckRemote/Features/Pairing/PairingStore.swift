//
//  PairingStore.swift
//  FlightDeckRemote
//
//  Tracks whether this device is paired with a FlightDeck desktop instance
//  (PRD §9: "persists until explicitly unpaired"). Persistence is currently
//  backed by `UserDefaults` via `PairingStateProviding` — a protocol seam so
//  the real Pairing feature task can swap in its actual backing (Keychain /
//  relay-confirmed state) without touching call sites or `AppRouter`.
//

import Foundation
import Observation

/// Persistence seam for pairing state. `PairingStore` depends on this
/// protocol rather than `UserDefaults` directly, so:
///  - the real Pairing feature can swap in its own backing later, and
///  - tests can inject an in-memory provider (see `InMemoryPairingStateProvider`
///    in `PairingStoreTests`) for hermetic, order-independent runs.
protocol PairingStateProviding {
    func loadIsPaired() -> Bool
    func saveIsPaired(_ isPaired: Bool)
}

/// `UserDefaults`-backed implementation used until the real pairing feature
/// (Keychain-backed device identity + relay confirmation) lands.
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

/// Tracks whether this device currently has an active pairing with a Mac.
/// `AppRouter` reads `isPaired` to decide between the Pairing screen and the
/// main tab container (PRD §5.8 entry flow).
@Observable
final class PairingStore {
    private let storage: PairingStateProviding

    var isPaired: Bool {
        didSet {
            guard isPaired != oldValue else { return }
            storage.saveIsPaired(isPaired)
        }
    }

    /// Metadata about the currently paired Mac, set by `completePairing(with:)`
    /// when a real pairing transaction (`PairingServicing`) succeeds. Unlike
    /// `isPaired`, this is not persisted through `PairingStateProviding` —
    /// only the paired boolean needs to survive relaunch for routing;
    /// persisting/rehydrating the device metadata itself is a follow-up for
    /// whichever task builds the Settings "connected device" surface
    /// (PRD §5.6) and the Keychain-backed secret storage.
    private(set) var pairedDevice: PairedDevice?

    init(storage: PairingStateProviding = UserDefaultsPairingStateProvider()) {
        self.storage = storage
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
        }
        #endif
        self.isPaired = initial
    }

    /// Records a successful pairing transaction (see `PairingServicing`) and
    /// flips `isPaired` — `AppRouter`/`RootView` react automatically and
    /// swap the Pairing screen for the main tab container (PRD §5.8).
    func completePairing(with device: PairedDevice) {
        pairedDevice = device
        isPaired = true
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
