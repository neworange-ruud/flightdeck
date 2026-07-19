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
    /// identity + KA key.
    private func makeHarness(pairingIds: [String], cap: Int = 4) throws -> Harness {
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
}
