//
//  TransportCoordinatorTests.swift
//  FlightDeckRemoteTests
//
//  Covers the multi-pairing `TransportCoordinator` (remote-control-b8d.5):
//   - spinning N clients from N `PairedInstance`s, each resolvable by pairingId;
//   - `reconcile` adds/removes only the affected client, leaving others intact;
//   - `setForeground(true)` connects all, `setForeground(false)` tears all down
//     (supervisors cancelled, every scripted socket closed, links `.disconnected`);
//   - all clients reuse the phone's ONE device identity + KA key (both clients'
//     auth signatures verify against the same device key, and both decrypt their
//     own desktop's E2E snapshot — proving the shared key-agreement key derived
//     each per-pairing channel);
//   - the fan-out is bounded by `cap`.
//

import Testing
import Foundation
import CryptoKit
@testable import FlightDeckRemote

@MainActor
@Suite struct TransportCoordinatorTests {

    // MARK: - Test connector: one scripted channel per client, creation order

    /// Hands out a fresh `ScriptedChannel` (wrapped in a `ScriptedConnector`) on
    /// every `connector()` call. `TransportCoordinator` calls the factory once
    /// per client at handle-creation time, so `channels[i]` is client `i`'s
    /// socket. The recordless fallback store uses its own never-connecting
    /// connector, so it does NOT perturb this 1:1 mapping.
    final class ChannelBook: @unchecked Sendable {
        private let lock = NSLock()
        private var _channels: [ScriptedChannel] = []

        func connector() -> any WebSocketConnecting {
            let channel = ScriptedChannel()
            lock.lock(); _channels.append(channel); lock.unlock()
            return ScriptedConnector(channel: channel)
        }

        var channels: [ScriptedChannel] {
            lock.lock(); defer { lock.unlock() }; return _channels
        }
    }

    private struct Harness {
        let coordinator: TransportCoordinator
        let book: ChannelBook
        let peers: [DesktopPeer]
        let instances: [PairedInstance]
        let identityPublicKeyB64: String
    }

    /// Build a coordinator over an in-memory keychain with one saved
    /// `PairingRecord` + `PairedInstance` per id, all sharing the phone's single
    /// identity + KA key. `pairingStore`, when supplied, arms the machine-name
    /// write-back (remote-control-b8d.9) — omitted by every OTHER test in this
    /// file so their assertions are unaffected.
    private func makeHarness(pairingIds: [String], cap: Int = 4, pairingStore: PairingStore? = nil) throws -> Harness {
        let keychain = InMemoryKeychainStore()
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        let keyAgreement = try KeyAgreementKeys.loadOrCreate(store: keychain)
        let recordStore = PairingRecordStore(store: keychain)

        var peers: [DesktopPeer] = []
        var instances: [PairedInstance] = []
        for (index, pairingId) in pairingIds.enumerated() {
            let (peer, _) = try TransportFixtures.makePeer(
                keychain: keychain,
                pairingId: pairingId,
                salt: Data("salt-\(pairingId)-000000000000".utf8),
                relayURL: "wss://relay.example/\(pairingId)"
            )
            try recordStore.save(peer.record)
            peers.append(peer)
            instances.append(PairedInstance(
                pairingId: pairingId,
                relayURL: URL(string: peer.record.relayURL)!,
                pairedAt: Date(timeIntervalSince1970: TimeInterval(1_000 + index))
            ))
        }

        let book = ChannelBook()
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        let coordinator = TransportCoordinator(
            identity: identity,
            keyAgreement: keyAgreement,
            recordStore: recordStore,
            connectorFactory: { book.connector() },
            cap: cap,
            clientConfig: config,
            pairingStore: pairingStore,
            now: { 1_752_000_100_000 }
        )
        return Harness(
            coordinator: coordinator, book: book, peers: peers,
            instances: instances, identityPublicKeyB64: identity.publicKeyBase64
        )
    }

    private func handshake(_ channel: ScriptedChannel, client: TransportClient) async {
        await channel.push(.helloOk(protocolVersion: 1, serverTimeMs: 1, connectionId: "c"))
        await channel.push(.authChallenge(nonce: TransportFixtures.nonceB64(), serverTimeMs: 1))
        await channel.push(.authOk(pairingIds: [Wire.PairingId(client.pairingId ?? "")]))
        _ = await waitUntil { if case .connected = await client.currentLinkState() { return true }; return false }
    }

    private func verifySignature(
        _ frame: Wire.RelayFrame,
        identityPublicKeyB64: String,
        nonceB64: String = TransportFixtures.nonceB64()
    ) -> Bool {
        guard case let .authResponse(_, signature, _) = frame,
              let pub = Data(base64Encoded: identityPublicKeyB64),
              let sig = Data(base64Encoded: signature),
              let nonce = Data(base64Encoded: nonceB64),
              let key = try? P256.Signing.PublicKey(x963Representation: pub),
              let ecdsa = try? P256.Signing.ECDSASignature(rawRepresentation: sig)
        else { return false }
        return key.isValidSignature(ecdsa, for: nonce)
    }

    // MARK: - N clients from N instances

    @Test func spinsOneClientPerInstanceResolvableByPairingId() throws {
        let h = try makeHarness(pairingIds: ["pair_a", "pair_b", "pair_c"])
        h.coordinator.installInitialInstances(h.instances)

        #expect(h.coordinator.handles.count == 3)
        #expect(h.coordinator.activePairingIds == ["pair_a", "pair_b", "pair_c"])
        #expect(h.coordinator.stores.count == 3)

        // Each pairing resolves to its own store + client.
        #expect(h.coordinator.store(for: "pair_b") != nil)
        #expect(h.coordinator.client(for: "pair_c") != nil)
        #expect(h.coordinator.store(for: "unknown") == nil)

        // Distinct store objects per pairing (not one shared store).
        let sA = try #require(h.coordinator.store(for: "pair_a"))
        let sB = try #require(h.coordinator.store(for: "pair_b"))
        #expect(sA !== sB)
        // And the client bound the right pairing.
        #expect(h.coordinator.client(for: "pair_b")?.pairingId == "pair_b")
    }

    @Test func primaryStoreIsFirstInstanceOrFallbackWhenEmpty() throws {
        let empty = try makeHarness(pairingIds: [])
        // No instances installed → primaryStore is the recordless fallback.
        #expect(empty.coordinator.primaryStore === empty.coordinator.fallbackStore)

        let h = try makeHarness(pairingIds: ["pair_a", "pair_b"])
        h.coordinator.installInitialInstances(h.instances)
        #expect(h.coordinator.primaryStore === h.coordinator.store(for: "pair_a"))
    }

    // MARK: - Runtime add / remove

    @Test func reconcileAddsAndRemovesOnlyTheAffectedClient() async throws {
        let h = try makeHarness(pairingIds: ["pair_a", "pair_b"])
        await h.coordinator.reconcile(with: h.instances)
        #expect(h.coordinator.activePairingIds == ["pair_a", "pair_b"])

        let storeBBefore = try #require(h.coordinator.store(for: "pair_b"))

        // Remove pair_a only.
        await h.coordinator.reconcile(with: [h.instances[1]])
        #expect(h.coordinator.activePairingIds == ["pair_b"])
        #expect(h.coordinator.store(for: "pair_a") == nil)
        // pair_b's live handle is untouched (same store object).
        #expect(h.coordinator.store(for: "pair_b") === storeBBefore)

        // Add pair_a back → a fresh handle, pair_b still the same object.
        await h.coordinator.reconcile(with: h.instances)
        #expect(Set(h.coordinator.activePairingIds) == ["pair_a", "pair_b"])
        #expect(h.coordinator.store(for: "pair_b") === storeBBefore)
        #expect(h.coordinator.store(for: "pair_a") !== storeBBefore)
    }

    // MARK: - Foreground connect-all / background teardown

    @Test func foregroundConnectsAllBackgroundTearsAllDown() async throws {
        let h = try makeHarness(pairingIds: ["pair_a", "pair_b"])
        h.coordinator.installInitialInstances(h.instances)

        await h.coordinator.setForeground(true)

        // Each client connects on its own socket.
        for (index, instance) in h.instances.enumerated() {
            let client = try #require(h.coordinator.client(for: instance.pairingId))
            await handshake(h.book.channels[index], client: client)
            let live = await client.currentLinkState()
            #expect({ if case .connected = live { return true }; return false }())
        }
        #expect(h.book.channels.count == 2)

        // Background → tear everything down.
        await h.coordinator.setForeground(false)

        for (index, instance) in h.instances.enumerated() {
            // No lingering socket.
            #expect(await h.book.channels[index].isClosed())
            // Link reported disconnected.
            let client = try #require(h.coordinator.client(for: instance.pairingId))
            #expect(await client.currentLinkState() == .disconnected)
        }
    }

    @Test func stopOnePairingLeavesOthersConnected() async throws {
        let h = try makeHarness(pairingIds: ["pair_a", "pair_b"])
        h.coordinator.installInitialInstances(h.instances)
        await h.coordinator.setForeground(true)
        for (index, instance) in h.instances.enumerated() {
            await handshake(h.book.channels[index], client: try #require(h.coordinator.client(for: instance.pairingId)))
        }

        await h.coordinator.stop(pairingId: "pair_a")

        #expect(await h.book.channels[0].isClosed())
        #expect(await h.coordinator.client(for: "pair_a")?.currentLinkState() == .disconnected)
        // pair_b untouched.
        #expect(!(await h.book.channels[1].isClosed()))
        let liveB = await h.coordinator.client(for: "pair_b")!.currentLinkState()
        #expect({ if case .connected = liveB { return true }; return false }())
    }

    // MARK: - Shared device keys across clients

    @Test func allClientsReuseSharedDeviceIdentityAndKeyAgreementKeys() async throws {
        let h = try makeHarness(pairingIds: ["pair_a", "pair_b"])
        h.coordinator.installInitialInstances(h.instances)
        await h.coordinator.setForeground(true)

        for (index, instance) in h.instances.enumerated() {
            await handshake(h.book.channels[index], client: try #require(h.coordinator.client(for: instance.pairingId)))
        }

        // Shared identity: every client's auth_response verifies against the ONE
        // device public key.
        for index in h.instances.indices {
            let sent = await h.book.channels[index].sentFrames()
            let authResp = try #require(sent.first { if case .authResponse = $0 { return true }; return false })
            #expect(verifySignature(authResp, identityPublicKeyB64: h.identityPublicKeyB64))
        }

        // Shared KA key: each desktop peer seals a snapshot under its OWN pairing
        // channel; that its client opens+folds it proves the client derived the
        // right per-pairing E2E channel from the shared key-agreement key.
        for index in h.instances.indices {
            let snap = Wire.StateSnapshot(serverTimeMs: Int64(index + 1), projects: [])
            try await h.book.channels[index].push(h.peers[index].envelopeFrame(.snapshot(snap), seq: 1))
        }
        for instance in h.instances {
            let store = try #require(h.coordinator.store(for: instance.pairingId))
            _ = await waitUntilMain { store.snapshot != nil }
            #expect(store.snapshot != nil)
        }
    }

    // MARK: - Cap

    @Test func fanOutIsBoundedByCap() throws {
        let h = try makeHarness(pairingIds: ["p1", "p2", "p3", "p4", "p5"], cap: 3)
        h.coordinator.installInitialInstances(h.instances)
        #expect(h.coordinator.handles.count == 3)
        #expect(h.coordinator.activePairingIds == ["p1", "p2", "p3"])
    }

    /// remote-control-b8d.7: the cap is a SINGLE shared constant
    /// (`PairingLimits.maxPairedInstances`) — `TransportCoordinator`'s default
    /// `cap` must read it rather than hardcoding its own literal, so this and
    /// `PairingStore.isAtPairingCap` can never drift out of sync.
    @Test func defaultCapReadsTheSharedPairingLimit() throws {
        let keychain = InMemoryKeychainStore()
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        let keyAgreement = try KeyAgreementKeys.loadOrCreate(store: keychain)
        let recordStore = PairingRecordStore(store: keychain)
        let book = ChannelBook()

        let coordinator = TransportCoordinator(
            identity: identity,
            keyAgreement: keyAgreement,
            recordStore: recordStore,
            connectorFactory: { book.connector() }
        )

        #expect(coordinator.cap == PairingLimits.maxPairedInstances)
    }

    // MARK: - Machine-name write-back into PairingStore (remote-control-b8d.9)

    @Test func desktopAnnouncedNameWritesBackIntoPairingStoreAsTheDefault() async throws {
        let pairingStore = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        let h = try makeHarness(pairingIds: ["pair_a"], pairingStore: pairingStore)
        pairingStore.add(h.instances[0])
        h.coordinator.installInitialInstances(h.instances)
        await h.coordinator.setForeground(true)
        let client = try #require(h.coordinator.client(for: "pair_a"))
        await handshake(h.book.channels[0], client: client)

        try await h.book.channels[0].push(.machineName(
            pairingId: Wire.PairingId("pair_a"), machineName: "Ruud's MacBook Pro"))

        _ = await waitUntilMain {
            pairingStore.list.first { $0.pairingId == "pair_a" }?.machineNameFromDesktop == "Ruud's MacBook Pro"
        }
        let instance = try #require(pairingStore.list.first { $0.pairingId == "pair_a" })
        #expect(instance.machineNameFromDesktop == "Ruud's MacBook Pro")
        #expect(instance.displayName == "Ruud's MacBook Pro")
    }

    @Test func reconnectWithANewDesktopNameUpdatesTheDefaultAgain() async throws {
        let pairingStore = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        let h = try makeHarness(pairingIds: ["pair_a"], pairingStore: pairingStore)
        pairingStore.add(h.instances[0])
        h.coordinator.installInitialInstances(h.instances)
        await h.coordinator.setForeground(true)
        let client = try #require(h.coordinator.client(for: "pair_a"))
        await handshake(h.book.channels[0], client: client)

        try await h.book.channels[0].push(.machineName(pairingId: Wire.PairingId("pair_a"), machineName: "Old Name"))
        _ = await waitUntilMain {
            pairingStore.list.first { $0.pairingId == "pair_a" }?.machineNameFromDesktop == "Old Name"
        }

        // The Mac was renamed and re-announces on its next connect (§5.7)
        // simulated here on the SAME live session for brevity — the client
        // handles it identically whether it arrives on this session or a
        // fresh one after a real reconnect.
        try await h.book.channels[0].push(.machineName(pairingId: Wire.PairingId("pair_a"), machineName: "New Name"))
        _ = await waitUntilMain {
            pairingStore.list.first { $0.pairingId == "pair_a" }?.machineNameFromDesktop == "New Name"
        }

        #expect(pairingStore.list.first { $0.pairingId == "pair_a" }?.displayName == "New Name")
    }

    @Test func userOverridePersistsAndWinsEvenAfterADesktopRename() async throws {
        let pairingStore = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        let h = try makeHarness(pairingIds: ["pair_a"], pairingStore: pairingStore)
        pairingStore.add(h.instances[0])
        pairingStore.setOverrideName(pairingId: "pair_a", "Home Studio Mac")
        h.coordinator.installInitialInstances(h.instances)
        await h.coordinator.setForeground(true)
        let client = try #require(h.coordinator.client(for: "pair_a"))
        await handshake(h.book.channels[0], client: client)

        try await h.book.channels[0].push(.machineName(
            pairingId: Wire.PairingId("pair_a"), machineName: "Ruud's MacBook Pro"))
        // The desktop default DOES still update underneath (it's a separate
        // field) — wait for that write, then assert the override still wins.
        _ = await waitUntilMain {
            pairingStore.list.first { $0.pairingId == "pair_a" }?.machineNameFromDesktop == "Ruud's MacBook Pro"
        }

        let instance = try #require(pairingStore.list.first { $0.pairingId == "pair_a" })
        #expect(instance.userOverrideName == "Home Studio Mac")
        #expect(instance.displayName == "Home Studio Mac", "a user override must always win over the desktop name")

        // Clearing the override falls back to the (already-updated) desktop name.
        pairingStore.setOverrideName(pairingId: "pair_a", nil)
        #expect(pairingStore.list.first { $0.pairingId == "pair_a" }?.displayName == "Ruud's MacBook Pro")
    }

    @Test func withNoPairingStoreSuppliedMachineNameIsNeverWrittenBack() async throws {
        // Every other test in this file omits `pairingStore:` — this just
        // makes the "opt-in, no-op otherwise" contract explicit and exercises
        // it against a live machine_name frame instead of only by omission.
        let h = try makeHarness(pairingIds: ["pair_a"]) // no pairingStore
        h.coordinator.installInitialInstances(h.instances)
        await h.coordinator.setForeground(true)
        let client = try #require(h.coordinator.client(for: "pair_a"))
        await handshake(h.book.channels[0], client: client)

        try await h.book.channels[0].push(.machineName(pairingId: Wire.PairingId("pair_a"), machineName: "Ruud's MacBook Pro"))
        _ = await waitUntilMain { h.coordinator.store(for: "pair_a")?.machineName == "Ruud's MacBook Pro" }

        // The per-instance store still folds it locally...
        #expect(h.coordinator.store(for: "pair_a")?.machineName == "Ruud's MacBook Pro")
        // ...but with no `PairingStore` to write back into, nothing crashes
        // and there's simply nowhere for it to persist (verified indirectly:
        // the coordinator's own `handles[0].instance` snapshot, refreshed only
        // by `reconcile`, is untouched).
        #expect(h.coordinator.handles.first?.instance.machineNameFromDesktop == nil)
    }

    // MARK: - Per-machine push mute (remote-control-b8d.10)

    private func registerCount(_ frames: [Wire.RelayFrame]) -> Int {
        frames.filter { if case .registerPushToken = $0 { return true }; return false }.count
    }

    private func unregisterCount(_ frames: [Wire.RelayFrame]) -> Int {
        frames.filter { if case .unregisterPushToken = $0 { return true }; return false }.count
    }

    @Test func registerPushTokenRegistersEveryLiveMachineWithItsOwnToken() async throws {
        let h = try makeHarness(pairingIds: ["pair_a", "pair_b"])
        h.coordinator.installInitialInstances(h.instances)
        await h.coordinator.setForeground(true)
        for (index, instance) in h.instances.enumerated() {
            await handshake(h.book.channels[index], client: try #require(h.coordinator.client(for: instance.pairingId)))
        }

        h.coordinator.registerPushToken("tok_hex", environment: .sandbox)

        // Both machines register their OWN token against their OWN pairingId.
        for index in h.instances.indices {
            _ = await waitUntil { await self.registerCount(h.book.channels[index].sentFrames()) == 1 }
        }
        for (index, instance) in h.instances.enumerated() {
            let frames = await h.book.channels[index].sentFrames()
            let reg = try #require(frames.first { if case .registerPushToken = $0 { return true }; return false })
            guard case let .registerPushToken(pairingId, token, env) = reg else { return }
            #expect(pairingId.rawValue == instance.pairingId)
            #expect(token == "tok_hex")
            #expect(env == .sandbox)
        }
    }

    @Test func mutingOneMachineDeregistersOnlyItAndLeavesOthersRegistered() async throws {
        let pairingStore = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        let h = try makeHarness(pairingIds: ["pair_a", "pair_b"], pairingStore: pairingStore)
        for instance in h.instances { pairingStore.add(instance) }
        h.coordinator.installInitialInstances(h.instances)
        await h.coordinator.setForeground(true)
        for (index, instance) in h.instances.enumerated() {
            await handshake(h.book.channels[index], client: try #require(h.coordinator.client(for: instance.pairingId)))
        }

        // Both registered.
        h.coordinator.registerPushToken("tok_hex", environment: .sandbox)
        for index in h.instances.indices {
            _ = await waitUntil { await self.registerCount(h.book.channels[index].sentFrames()) == 1 }
        }
        #expect(await registerCount(h.book.channels[0].sentFrames()) == 1)
        #expect(await registerCount(h.book.channels[1].sentFrames()) == 1)

        // Mute pair_a only — the mute flips in the store and reconcile applies it.
        pairingStore.setMutePush(pairingId: "pair_a", true)
        await h.coordinator.reconcile(with: pairingStore.list)

        // pair_a (channel 0) deregisters exactly once; pair_b (channel 1) never does.
        let aDeregistered = await waitUntil { await self.unregisterCount(h.book.channels[0].sentFrames()) == 1 }
        #expect(aDeregistered)
        // Give pair_b a chance to (wrongly) deregister before asserting it didn't.
        _ = await waitUntil { await self.unregisterCount(h.book.channels[1].sentFrames()) > 0 }
        #expect(await unregisterCount(h.book.channels[1].sentFrames()) == 0)
        #expect(await registerCount(h.book.channels[1].sentFrames()) == 1)

        // Unmuting pair_a re-registers only it (register count 1 → 2), pair_b unchanged.
        pairingStore.setMutePush(pairingId: "pair_a", false)
        await h.coordinator.reconcile(with: pairingStore.list)
        let aReRegistered = await waitUntil { await self.registerCount(h.book.channels[0].sentFrames()) == 2 }
        #expect(aReRegistered)
        #expect(await registerCount(h.book.channels[1].sentFrames()) == 1)
    }

    @Test func aMachineAddedAfterTheTokenArrivedIsRegisteredToo() async throws {
        let h = try makeHarness(pairingIds: ["pair_a", "pair_b"])
        // Start with only pair_a live and registered.
        await h.coordinator.reconcile(with: [h.instances[0]])
        await h.coordinator.setForeground(true)
        await handshake(h.book.channels[0], client: try #require(h.coordinator.client(for: "pair_a")))
        h.coordinator.registerPushToken("tok_hex", environment: .sandbox)
        _ = await waitUntil { await self.registerCount(h.book.channels[0].sentFrames()) == 1 }

        // Now add pair_b — the coordinator remembered the token and registers it
        // once its socket goes live, without a fresh token refresh.
        await h.coordinator.reconcile(with: h.instances)
        await handshake(h.book.channels[1], client: try #require(h.coordinator.client(for: "pair_b")))
        let bRegistered = await waitUntil { await self.registerCount(h.book.channels[1].sentFrames()) == 1 }
        #expect(bRegistered)
    }
}
