//
//  PairedInstanceStoreTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the multi-pairing surface of `PairingStore` (remote-control-
//  b8d.4): `[PairedInstance]` persists across a fresh store instance (the
//  "across launches" acceptance criterion), `add` appends rather than
//  replaces, the display-name precedence (override > desktop > fallback),
//  and that the mute/override/machine-name setters persist. Uses an
//  in-memory `PairedInstancesProviding` (mirroring `InMemoryPairingStateProvider`
//  in `PairingStoreTests`) so these tests never touch the real `UserDefaults`.
//

import Testing
import Foundation
@testable import FlightDeckRemote

/// In-memory `PairedInstancesProviding` for hermetic tests. A fresh
/// `PairingStore(instancesStorage:)` reading the SAME provider instance
/// simulates "a fresh store instance after relaunch" without touching the
/// real `UserDefaults`.
final class InMemoryPairedInstancesProvider: PairedInstancesProviding {
    private var stored: [PairedInstance]

    init(initial: [PairedInstance] = []) {
        stored = initial
    }

    func loadInstances() -> [PairedInstance] { stored }
    func saveInstances(_ instances: [PairedInstance]) { stored = instances }
}

struct PairedInstanceStoreTests {

    private let relayURL = URL(string: "wss://relay.flightdeck.app/v1")!
    private let otherRelayURL = URL(string: "wss://relay2.flightdeck.app/v1")!

    private func makeInstance(
        pairingId: String,
        machineNameFromDesktop: String? = nil,
        userOverrideName: String? = nil,
        relayURL: URL? = nil,
        mutePush: Bool = false,
        pairedAt: Date = Date(),
        lastKnownOnline: Bool = true
    ) -> PairedInstance {
        PairedInstance(
            pairingId: pairingId,
            machineNameFromDesktop: machineNameFromDesktop,
            userOverrideName: userOverrideName,
            relayURL: relayURL ?? self.relayURL,
            mutePush: mutePush,
            pairedAt: pairedAt,
            lastKnownOnline: lastKnownOnline
        )
    }

    // MARK: - list / add

    @Test func startsEmptyWithFreshProvider() {
        let store = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        #expect(store.list.isEmpty)
    }

    @Test func addAppendsRatherThanReplaces() {
        let store = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())

        store.add(makeInstance(pairingId: "pair-1"))
        #expect(store.list.count == 1)

        store.add(makeInstance(pairingId: "pair-2", relayURL: otherRelayURL))
        #expect(store.list.count == 2, "adding a second pairing must not evict the first")
        #expect(store.list.map(\.pairingId) == ["pair-1", "pair-2"], "append order is preserved")
    }

    @Test func addWithSamePairingIdReplacesThatEntryOnly() {
        let store = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        store.add(makeInstance(pairingId: "pair-1", machineNameFromDesktop: "Old Name"))
        store.add(makeInstance(pairingId: "pair-2"))

        store.add(makeInstance(pairingId: "pair-1", machineNameFromDesktop: "New Name"))

        #expect(store.list.count == 2, "re-adding a known pairingId must not duplicate it")
        #expect(store.list.first { $0.pairingId == "pair-1" }?.machineNameFromDesktop == "New Name")
    }

    @Test func removeDropsOnlyTheMatchingPairing() {
        let store = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        store.add(makeInstance(pairingId: "pair-1"))
        store.add(makeInstance(pairingId: "pair-2"))

        store.remove(pairingId: "pair-1")

        #expect(store.list.map(\.pairingId) == ["pair-2"])
    }

    @Test func removeOfUnknownPairingIdIsANoOp() {
        let store = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        store.add(makeInstance(pairingId: "pair-1"))

        store.remove(pairingId: "does-not-exist")

        #expect(store.list.count == 1)
    }

    // MARK: - Persistence across a fresh store instance ("across launches")

    @Test func persistsAcrossFreshStoreInstance() throws {
        let provider = InMemoryPairedInstancesProvider()
        let store = PairingStore(instancesStorage: provider)
        let pairedAt = Date(timeIntervalSince1970: 1_700_000_000)

        store.add(makeInstance(pairingId: "pair-1", machineNameFromDesktop: "Ruud's MacBook Pro", pairedAt: pairedAt))
        store.add(makeInstance(pairingId: "pair-2", relayURL: otherRelayURL))

        // A fresh `PairingStore` reading the same underlying provider is the
        // hermetic stand-in for "the app relaunched".
        let reloaded = PairingStore(instancesStorage: provider)

        #expect(reloaded.list.count == 2)
        let first = try #require(reloaded.list.first { $0.pairingId == "pair-1" })
        #expect(first.machineNameFromDesktop == "Ruud's MacBook Pro")
        #expect(first.relayURL == relayURL)
        #expect(first.pairedAt == pairedAt)
        #expect(reloaded.list.first { $0.pairingId == "pair-2" }?.relayURL == otherRelayURL)
    }

    @Test func persistsThroughRealUserDefaultsProvider() {
        let suiteName = "PairedInstanceStoreTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let provider = UserDefaultsPairedInstancesProvider(defaults: defaults)
        let store = PairingStore(instancesStorage: provider)
        store.add(makeInstance(pairingId: "pair-1", machineNameFromDesktop: "Ruud's MacBook Pro"))

        let reloaded = PairingStore(instancesStorage: UserDefaultsPairedInstancesProvider(defaults: defaults))
        #expect(reloaded.list.map(\.pairingId) == ["pair-1"])
        #expect(reloaded.list.first?.machineNameFromDesktop == "Ruud's MacBook Pro")
    }

    // MARK: - Display-name precedence

    @Test func displayNameFallsBackWhenNeitherNameIsSet() {
        let instance = makeInstance(pairingId: "pair-1")
        #expect(instance.displayName == PairedInstance.fallbackDisplayName)
    }

    @Test func displayNameUsesDesktopNameWhenNoOverride() {
        let instance = makeInstance(pairingId: "pair-1", machineNameFromDesktop: "Ruud's MacBook Pro")
        #expect(instance.displayName == "Ruud's MacBook Pro")
    }

    @Test func displayNameOverrideWinsOverDesktopName() {
        let instance = makeInstance(
            pairingId: "pair-1",
            machineNameFromDesktop: "Ruud's MacBook Pro",
            userOverrideName: "Home Studio Mac"
        )
        #expect(instance.displayName == "Home Studio Mac")
    }

    // MARK: - Setters persist

    @Test func setOverrideNamePersists() {
        let provider = InMemoryPairedInstancesProvider()
        let store = PairingStore(instancesStorage: provider)
        store.add(makeInstance(pairingId: "pair-1"))

        store.setOverrideName(pairingId: "pair-1", "Home Studio Mac")

        #expect(store.list.first?.userOverrideName == "Home Studio Mac")
        #expect(store.list.first?.displayName == "Home Studio Mac")

        let reloaded = PairingStore(instancesStorage: provider)
        #expect(reloaded.list.first?.userOverrideName == "Home Studio Mac")
    }

    @Test func setOverrideNameToNilClearsIt() {
        let store = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        store.add(makeInstance(pairingId: "pair-1", userOverrideName: "Home Studio Mac"))

        store.setOverrideName(pairingId: "pair-1", nil)

        #expect(store.list.first?.userOverrideName == nil)
    }

    @Test func setMutePushPersists() {
        let provider = InMemoryPairedInstancesProvider()
        let store = PairingStore(instancesStorage: provider)
        store.add(makeInstance(pairingId: "pair-1", mutePush: false))

        store.setMutePush(pairingId: "pair-1", true)

        #expect(store.list.first?.mutePush == true)

        let reloaded = PairingStore(instancesStorage: provider)
        #expect(reloaded.list.first?.mutePush == true)
    }

    @Test func setMachineNamePersistsAndUpdatesDisplayNameWhenNoOverride() {
        let provider = InMemoryPairedInstancesProvider()
        let store = PairingStore(instancesStorage: provider)
        store.add(makeInstance(pairingId: "pair-1"))

        store.setMachineName(pairingId: "pair-1", "Ruud's MacBook Pro")

        #expect(store.list.first?.machineNameFromDesktop == "Ruud's MacBook Pro")
        #expect(store.list.first?.displayName == "Ruud's MacBook Pro")

        let reloaded = PairingStore(instancesStorage: provider)
        #expect(reloaded.list.first?.machineNameFromDesktop == "Ruud's MacBook Pro")
    }

    @Test func settersOnUnknownPairingIdAreNoOps() {
        let store = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        store.add(makeInstance(pairingId: "pair-1"))

        store.setOverrideName(pairingId: "does-not-exist", "Nope")
        store.setMutePush(pairingId: "does-not-exist", true)
        store.setMachineName(pairingId: "does-not-exist", "Nope")

        #expect(store.list.count == 1)
        #expect(store.list.first?.userOverrideName == nil)
        #expect(store.list.first?.mutePush == false)
        #expect(store.list.first?.machineNameFromDesktop == nil)
    }

    // MARK: - hasAnyPairing bridge

    @Test func hasAnyPairingIsFalseWhenEmptyAndIsPairedIsFalse() {
        let store = PairingStore(
            storage: InMemoryPairingStateProvider(initial: false),
            instancesStorage: InMemoryPairedInstancesProvider()
        )
        #expect(store.hasAnyPairing == false)
    }

    @Test func hasAnyPairingIsTrueOnceAnInstanceIsAdded() {
        let store = PairingStore(
            storage: InMemoryPairingStateProvider(initial: false),
            instancesStorage: InMemoryPairedInstancesProvider()
        )
        store.add(makeInstance(pairingId: "pair-1"))
        #expect(store.hasAnyPairing == true)
    }

    // MARK: - isAtPairingCap (remote-control-b8d.7 cap enforcement)

    /// Below the shared cap (`PairingLimits.maxPairedInstances`), starting a
    /// new pairing must NOT be blocked.
    @Test func isAtPairingCapIsFalseBelowTheSharedLimit() {
        let store = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        for i in 0..<(PairingLimits.maxPairedInstances - 1) {
            store.add(makeInstance(pairingId: "pair-\(i)"))
        }
        #expect(store.list.count == PairingLimits.maxPairedInstances - 1)
        #expect(store.isAtPairingCap == false)
    }

    /// Exactly at the shared cap, attempting to add ONE MORE (the
    /// `(cap + 1)`th pairing) must be blocked — this is the boundary
    /// `PairingView.pair(with:)` checks before ever starting a new handshake.
    @Test func isAtPairingCapIsTrueAtTheSharedLimit() {
        let store = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        for i in 0..<PairingLimits.maxPairedInstances {
            store.add(makeInstance(pairingId: "pair-\(i)"))
        }
        #expect(store.list.count == PairingLimits.maxPairedInstances)
        #expect(store.isAtPairingCap == true)
    }

    /// Unpairing one machine while at the cap must free up a slot again.
    @Test func isAtPairingCapClearsAfterRemovingOneAtTheLimit() {
        let store = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        for i in 0..<PairingLimits.maxPairedInstances {
            store.add(makeInstance(pairingId: "pair-\(i)"))
        }
        #expect(store.isAtPairingCap == true)

        store.remove(pairingId: "pair-0")

        #expect(store.isAtPairingCap == false)
    }
}
