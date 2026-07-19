//
//  TransportStoreFactory.swift
//  FlightDeckRemote
//
//  Composes the app's single `TransportStore` (PRD §5.2/§5.7: every Projects
//  tab screen binds to it). Mirrors `PairingServiceFactory`'s shape:
//  production wiring by default, reusing the exact same
//  DeviceIdentity/KeyAgreementKeys/PairingRecordStore/URLSessionWebSocketConnection
//  composition `RealPairingService` already uses elsewhere in Transport/, plus
//  a DEBUG seam so the Projects/Sessions screens render deterministically in
//  UI tests without a live relay or paired Mac.
//
//  Building a real `TransportClient` here is always safe even before the
//  device has ever paired: `TransportClient.start()` loads the persisted
//  `PairingRecord`, finds none, and simply reports `.disconnected` — it never
//  blocks, throws, or opens a socket speculatively (see
//  `TransportClient.runSupervisor`). Actually establishing/managing the live
//  connection (retry UI, connection-status surface, etc.) is a later
//  Connection feature's job; this factory only supplies the store's
//  dependencies so the screens have something to bind to today.
//
//  `.debugSeed` (TransportStore.swift) is the additive DEBUG seam this
//  factory uses to seed the `-uitest-fixture-snapshot` fixture
//  (Features/Monitor/DebugFixtures.swift).
//
//  Offline cache (PRD §9.2): this factory also owns the app's single
//  `SnapshotCache`, wiring it into the store (so live updates persist a
//  debounced last-known-state) and seeding the store from any previously
//  cached state via `TransportStore.seedFromCache` *before* `start()` is ever
//  called (`MainTabView` calls `start()` later, in its own `.task`). Real
//  on-disk cache loading is skipped for any `-uitest*` launch (mirrors
//  `-uitest-reset-pairing`'s hermeticity goal — a UI test must never depend
//  on whatever a previous simulator run happened to leave on disk); the
//  dedicated `-uitest-fixture-snapshot-stale` arg seeds a known stale fixture
//  instead, for the `StaleBanner` UI tests.
//

import Foundation

@MainActor
enum TransportStoreFactory {
    /// Build the app's `TransportStore`. Seeds the DEBUG fixture snapshot
    /// (`Wire.StateSnapshot.uiTestFixture`) when launched with
    /// `-uitest-fixture-snapshot`, so previews and UI tests never depend on a
    /// live desktop.
    static func makeDefault(arguments: [String] = ProcessInfo.processInfo.arguments) -> TransportStore {
        let cache = SnapshotCache(fileURL: SnapshotCache.defaultFileURL())
        let store = TransportStore(client: makeClient(), cache: cache)
        #if DEBUG
        let isUITestLaunch = arguments.contains { $0.hasPrefix("-uitest") }
        if arguments.contains("-uitest-fixture-snapshot") {
            store.debugSeed(snapshot: .uiTestFixture)
        } else if arguments.contains("-uitest-fixture-snapshot-stale") {
            store.seedFromCache(SnapshotCache.CachedState(snapshot: .uiTestFixture, transcripts: [], cachedAtMs: 0))
        } else if !isUITestLaunch, let cached = cache.load() {
            store.seedFromCache(cached)
        }
        #endif
        return store
    }

    /// Build the app's multi-pairing `TransportCoordinator` (remote-control-b8d.5):
    /// one `TransportClient` + `TransportStore` per `PairedInstance`, all sharing
    /// the phone's single `DeviceIdentity` + `KeyAgreementKeys` (Secure Enclave /
    /// KA keys are per-device, never per-pairing) and one keyed
    /// `PairingRecordStore`. Each client owns a fresh `URLSessionWebSocketConnection`
    /// and a per-pairing `SnapshotCache` file.
    ///
    /// Legacy bridge: a device paired before multi-pairing has a `PairingRecord`
    /// but no `PairedInstance` — synthesized here so the coordinator connects it
    /// like any other. The DEBUG `-uitest-fixture-snapshot[-stale]` seams seed the
    /// (recordless) transitional `primaryStore` so the Projects/Sessions screens
    /// render deterministically without a live desktop, matching the pre-multi-
    /// pairing `makeDefault` behavior. Real on-disk caches are skipped for any
    /// `-uitest*` launch (hermeticity).
    static func makeCoordinator(
        pairingStore: PairingStore,
        arguments: [String] = ProcessInfo.processInfo.arguments
    ) -> TransportCoordinator {
        let recordStore = PairingRecordStore()
        let isUITestLaunch = arguments.contains { $0.hasPrefix("-uitest") }
        let coordinator = TransportCoordinator(
            identity: loadIdentity(),
            keyAgreement: loadKeyAgreement(),
            recordStore: recordStore,
            connectorFactory: { URLSessionWebSocketConnection() },
            cacheFactory: { pairingId in
                isUITestLaunch
                    ? nil
                    : SnapshotCache(fileURL: SnapshotCache.fileURL(forPairingId: pairingId))
            }
        )
        if !isUITestLaunch {
            seedLegacyInstanceIfNeeded(into: pairingStore, recordStore: recordStore)
        }
        coordinator.installInitialInstances(pairingStore.list)
        #if DEBUG
        if arguments.contains("-uitest-fixture-snapshot") {
            coordinator.primaryStore.debugSeed(snapshot: .uiTestFixture)
        } else if arguments.contains("-uitest-fixture-snapshot-stale") {
            coordinator.primaryStore.seedFromCache(
                SnapshotCache.CachedState(snapshot: .uiTestFixture, transcripts: [], cachedAtMs: 0))
        }
        #endif
        return coordinator
    }

    /// One-time legacy migration: if the device is paired (a `PairingRecord`
    /// exists) but has no `PairedInstance` metadata yet (paired before
    /// remote-control-b8d.4), synthesize the instance from the record so the
    /// coordinator treats it as a first-class pairing. Idempotent — a no-op once
    /// an instance list exists.
    private static func seedLegacyInstanceIfNeeded(
        into pairingStore: PairingStore,
        recordStore: PairingRecordStore
    ) {
        guard pairingStore.list.isEmpty,
              let records = try? recordStore.loadAll(), !records.isEmpty else { return }
        for record in records {
            guard let url = URL(string: record.relayURL) else { continue }
            pairingStore.add(PairedInstance(
                pairingId: record.pairingId,
                relayURL: url,
                pairedAt: record.pairedAt
            ))
        }
    }

    private static func makeClient() -> TransportClient {
        TransportClient(
            identity: loadIdentity(),
            keyAgreement: loadKeyAgreement(),
            recordStore: PairingRecordStore(),
            connector: URLSessionWebSocketConnection()
        )
    }

    private static func loadIdentity() -> DeviceIdentity {
        if let identity = try? DeviceIdentity.loadOrCreate(store: KeychainStore(service: DeviceIdentity.service)) {
            return identity
        }
        // The real Keychain failed (should not happen in practice — e.g. a
        // corrupted stored blob). Fall back to a fresh, in-memory-only
        // identity so the app never crashes at launch; nothing persists, so
        // the phone will simply need to (re-)pair.
        return try! DeviceIdentity.loadOrCreate(store: InMemoryFallbackKeychainStore())
    }

    private static func loadKeyAgreement() -> KeyAgreementKeys {
        if let keys = try? KeyAgreementKeys.loadOrCreate(store: KeychainStore(service: KeyAgreementKeys.service)) {
            return keys
        }
        return try! KeyAgreementKeys.loadOrCreate(store: InMemoryFallbackKeychainStore())
    }
}

/// Minimal in-memory `KeychainStoring` used only as the last-resort fallback
/// above. Starting empty, `loadOrCreate` always takes the fresh-key path, so
/// this can't fail the way a corrupted real Keychain blob could.
private final class InMemoryFallbackKeychainStore: KeychainStoring {
    private var storage: [String: Data] = [:]
    func get(account: String) throws -> Data? { storage[account] }
    func set(_ data: Data, account: String) throws { storage[account] = data }
    func delete(account: String) throws { storage.removeValue(forKey: account) }
    func accounts() throws -> [String] { Array(storage.keys) }
}
