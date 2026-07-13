//
//  PairingStoreTests.swift
//  FlightDeckRemoteTests
//
//  Verifies `PairingStore` loads/persists through its `PairingStateProviding`
//  seam (not through the real `UserDefaults`, so these tests are hermetic
//  and order-independent) and that the DEBUG toggle flips state.
//

import Testing
@testable import FlightDeckRemote

/// In-memory `PairingStateProviding` for hermetic tests — mirrors the
/// `InMemoryKeychainStore` pattern used by `DeviceIdentityTests`.
final class InMemoryPairingStateProvider: PairingStateProviding {
    private var stored: Bool

    init(initial: Bool = false) {
        stored = initial
    }

    func loadIsPaired() -> Bool { stored }
    func saveIsPaired(_ isPaired: Bool) { stored = isPaired }
}

struct PairingStoreTests {

    @Test func loadsInitialStateFromProvider() {
        let provider = InMemoryPairingStateProvider(initial: true)
        let store = PairingStore(storage: provider)
        #expect(store.isPaired == true)
    }

    @Test func defaultsToUnpairedWithFreshProvider() {
        let provider = InMemoryPairingStateProvider()
        let store = PairingStore(storage: provider)
        #expect(store.isPaired == false)
    }

    @Test func mutatingIsPairedPersistsToProvider() {
        let provider = InMemoryPairingStateProvider()
        let store = PairingStore(storage: provider)

        store.isPaired = true
        #expect(provider.loadIsPaired() == true)

        // A second store reading the same provider observes the change.
        let reloaded = PairingStore(storage: provider)
        #expect(reloaded.isPaired == true)
    }

    #if DEBUG
    @Test func debugToggleFlipsIsPaired() {
        let store = PairingStore(storage: InMemoryPairingStateProvider())
        #expect(store.isPaired == false)

        store.debugTogglePaired()
        #expect(store.isPaired == true)

        store.debugTogglePaired()
        #expect(store.isPaired == false)
    }
    #endif
}
