//
//  TransportCoordinator.swift
//  FlightDeckRemote
//
//  Owns the fan-out of live transports for multi-pairing (remote-control-b8d.5):
//  up to `cap` independent `TransportClient`s, ONE per `PairedInstance` from the
//  `PairingStore`, each connecting to its own `relayURL` with its own
//  `PairingRecord`, E2E channel, reconnect supervisor, and `TransportStore`.
//
//  Why a coordinator rather than a smarter client: `TransportClient` stays
//  deliberately single-pairing (it already is — one record/channel/supervisor).
//  The coordinator just instantiates it N times and manages the *set* — which
//  is where the multi-pairing lifecycle lives:
//
//    - Foreground → `startAll()`: every paired machine gets a live socket
//      (bounded by `cap`, PRD: all instances live while foregrounded, APNs push
//      takes over on background).
//    - Background → `stopAll()`: every supervisor is cancelled and every
//      `URLSessionWebSocketTask` is closed (via `TransportStore.stop` →
//      `TransportClient.stop`). No lingering sockets, no leaked tasks.
//    - Runtime add/remove (`reconcile(with:)`): pairing a new machine spins up
//      *only* its client; unpairing one stops+disposes *only* that client,
//      leaving the others untouched.
//
//  Shared keys: every client is handed the phone's ONE `DeviceIdentity`
//  (Secure Enclave signing key) and ONE `KeyAgreementKeys` (software KA key).
//  Per-pairing device keys are never minted — each pairing derives its own E2E
//  channel from its own salt inside its `TransportClient` (see
//  `TransportClient.deriveChannel`).
//
//  Concurrency: this type is `@MainActor` and `@Observable`. All mutable state
//  (`handles`, `isForeground`) is touched only on the main actor; the actual
//  socket/cursor work happens inside each `TransportClient` actor. Start/stop
//  are `async` because they await the per-store lifecycle, which awaits the
//  actor client — giving deterministic teardown (a returned `stopAll()` means
//  every socket is closed).
//
//  Transitional bridge (b8d.5 → b8d.12): today the Projects/Activity/Shell/
//  Settings tabs bind to a single `TransportStore`. Until b8d.12 parameterizes
//  those detail views by `pairingId` (resolving their store from this
//  coordinator via `store(for:)`), `primaryStore` exposes the first active
//  instance's store so `MainTabView` keeps working unchanged.
//

import Foundation
import Observation

@MainActor
@Observable
final class TransportCoordinator {

    /// One live transport for one paired machine: the actor `client`, the
    /// `@Observable` `store` the UI binds to, and the `instance` metadata it was
    /// built from (kept so `reconcile` can detect membership + preserve order).
    struct ClientHandle: Identifiable {
        let pairingId: String
        var instance: PairedInstance
        let client: TransportClient
        let store: TransportStore

        var id: String { pairingId }
    }

    // MARK: - Configuration

    /// Hard fan-out cap (PRD: ~3–4 paired instances). `reconcile` bounds the
    /// live set to this many even if `PairingStore` somehow holds more; full
    /// cap *enforcement at the add site* is remote-control-b8d.7's job.
    let cap: Int

    // MARK: - Observable state

    /// The live handles, ordered to match the reconciled instance order
    /// (oldest first). `private(set)` — mutated only through `reconcile`.
    private(set) var handles: [ClientHandle] = []

    // MARK: - Dependencies (shared across every client)

    private let identity: DeviceIdentity
    private let keyAgreement: KeyAgreementKeys
    private let recordStore: PairingRecordStore
    /// A fresh `WebSocketConnecting` per client, so each pairing owns an
    /// independent socket. A closure (not a shared instance) keeps the clients
    /// from contending on one connector.
    private let connectorFactory: @Sendable () -> any WebSocketConnecting
    /// Per-pairing offline cache (or `nil` to disable persistence — tests pass
    /// a no-op so nothing touches disk). `@MainActor` because `SnapshotCache`
    /// is main-actor isolated and this is invoked from `makeHandle`.
    private let cacheFactory: @MainActor @Sendable (String) -> SnapshotCache?
    private let clientConfig: TransportClient.Config
    private let now: @Sendable () -> Int64

    /// Whether the app is currently foregrounded. Drives whether a newly-added
    /// client starts immediately (`reconcile`) or waits for the next foreground.
    private var isForeground = false

    /// A never-connecting store handed to `primaryStore` when there are zero
    /// paired instances, so the transitional single-store consumers always have
    /// a non-`nil` store to bind to (it binds to a bogus pairingId, finds no
    /// record, and reports `.disconnected` — exactly like the pre-multi-pairing
    /// `TransportStoreFactory.makeDefault` did for an unpaired device).
    let fallbackStore: TransportStore

    // MARK: - Init

    init(
        identity: DeviceIdentity,
        keyAgreement: KeyAgreementKeys,
        recordStore: PairingRecordStore,
        connectorFactory: @escaping @Sendable () -> any WebSocketConnecting,
        cacheFactory: @escaping @MainActor @Sendable (String) -> SnapshotCache? = { _ in nil },
        cap: Int = 4,
        clientConfig: TransportClient.Config = TransportClient.Config(),
        now: @escaping @Sendable () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }
    ) {
        self.identity = identity
        self.keyAgreement = keyAgreement
        self.recordStore = recordStore
        self.connectorFactory = connectorFactory
        self.cacheFactory = cacheFactory
        self.cap = cap
        self.clientConfig = clientConfig
        self.now = now
        // A recordless store (pairingId that can never match a stored record):
        // its client's supervisor loads nothing and stays disconnected.
        self.fallbackStore = TransportStore(
            client: TransportClient(
                identity: identity,
                keyAgreement: keyAgreement,
                recordStore: recordStore,
                pairingId: "\u{0}",
                connector: NeverConnectingConnector(),
                config: clientConfig,
                now: now
            ),
            cache: nil,
            now: now
        )
    }

    // MARK: - Lookup (downstream API: b8d.6 feed, b8d.12 detail views)

    /// The pairing ids of every live instance, oldest first.
    var activePairingIds: [String] { handles.map(\.pairingId) }

    /// Every live per-instance store, oldest first — the aggregated feed
    /// (remote-control-b8d.6) folds across these.
    var stores: [TransportStore] { handles.map(\.store) }

    /// The `TransportStore` for `pairingId`, or `nil` if not currently paired/
    /// active. Detail views (remote-control-b8d.12) resolve their store this way.
    func store(for pairingId: String) -> TransportStore? {
        handles.first { $0.pairingId == pairingId }?.store
    }

    /// The `TransportClient` for `pairingId` (per-machine push registration,
    /// remote-control-b8d.10), or `nil` if not active.
    func client(for pairingId: String) -> TransportClient? {
        handles.first { $0.pairingId == pairingId }?.client
    }

    /// Transitional single-store bridge: the first active instance's store, or
    /// the recordless `fallbackStore` when nothing is paired. Replaced by
    /// per-`pairingId` resolution in remote-control-b8d.12.
    var primaryStore: TransportStore { handles.first?.store ?? fallbackStore }

    // MARK: - Initial install (synchronous, pre-foreground)

    /// Build (but do not start) one handle per instance, at init — synchronous so
    /// `primaryStore` resolves to the real first store immediately, before the
    /// view captures it. Nothing is started here (the app isn't foregrounded
    /// yet); `startAll()` / `setForeground(true)` from the `scenePhase` observer
    /// connects them. Precondition: no handles exist yet (call once, from the
    /// factory). Runtime changes afterwards go through `reconcile(with:)`.
    func installInitialInstances(_ instances: [PairedInstance]) {
        guard handles.isEmpty else { return }
        for instance in instances.prefix(cap) {
            handles.append(makeHandle(for: instance))
        }
    }

    // MARK: - Membership (runtime add/remove)

    /// Reconcile the live client set to exactly the (cap-bounded) `instances`:
    /// stop+dispose clients no longer present, spin up clients newly present
    /// (started immediately iff the app is foregrounded), and reorder to match.
    /// Untouched instances keep their existing live client — a reconcile after
    /// one machine is added/removed never disturbs the others.
    func reconcile(with instances: [PairedInstance]) async {
        let capped = Array(instances.prefix(cap))
        let desired = Set(capped.map(\.pairingId))

        // Stop + drop handles for pairings that are gone (unpair one machine).
        let stale = handles.filter { !desired.contains($0.pairingId) }
        for handle in stale {
            await handle.store.stop()
        }
        handles.removeAll { !desired.contains($0.pairingId) }

        // Spin up handles for newly-added pairings.
        for instance in capped where !handles.contains(where: { $0.pairingId == instance.pairingId }) {
            let handle = makeHandle(for: instance)
            handles.append(handle)
            if isForeground {
                await handle.store.start()
            }
        }

        // Refresh retained handles' instance metadata + preserve instance order.
        let order = Dictionary(uniqueKeysWithValues: capped.enumerated().map { ($1.pairingId, $0) })
        let byId = Dictionary(uniqueKeysWithValues: capped.map { ($0.pairingId, $0) })
        for index in handles.indices {
            if let updated = byId[handles[index].pairingId] {
                handles[index].instance = updated
            }
        }
        handles.sort { (order[$0.pairingId] ?? .max) < (order[$1.pairingId] ?? .max) }
    }

    // MARK: - Lifecycle (scenePhase-driven)

    /// Foreground/background transition (called from the `scenePhase` observer).
    /// Foreground starts every client; background stops every client and closes
    /// every socket. Idempotent per state.
    func setForeground(_ active: Bool) async {
        guard active != isForeground else { return }
        isForeground = active
        if active {
            await startAll()
        } else {
            await stopAll()
        }
    }

    /// Start every client's supervisor (connect all). Idempotent — each
    /// `TransportStore.start()` is a no-op if already started.
    func startAll() async {
        isForeground = true
        for handle in handles {
            await handle.store.start()
        }
    }

    /// Stop every client: cancel supervisors, close every `URLSessionWebSocketTask`,
    /// and tear down each store's event bridge. On return, no socket lingers.
    func stopAll() async {
        isForeground = false
        for handle in handles {
            await handle.store.stop()
        }
    }

    /// Start only the client for `pairingId` (no-op if not active).
    func start(pairingId: String) async {
        await store(for: pairingId)?.start()
    }

    /// Stop only the client for `pairingId` — cancels its supervisor and closes
    /// its socket, leaving the others live (no-op if not active).
    func stop(pairingId: String) async {
        await store(for: pairingId)?.stop()
    }

    // MARK: - Building a client

    private func makeHandle(for instance: PairedInstance) -> ClientHandle {
        let client = TransportClient(
            identity: identity,
            keyAgreement: keyAgreement,
            recordStore: recordStore,
            pairingId: instance.pairingId,
            connector: connectorFactory(),
            config: clientConfig,
            now: now
        )
        let cache = cacheFactory(instance.pairingId)
        let store = TransportStore(client: client, cache: cache, now: now)
        // Offline last-known-state (PRD §9.2): seed from this pairing's own
        // cache BEFORE it is ever started, so a backgrounded/offline machine
        // still shows its last feed. Never races a live snapshot (the store
        // isn't started yet).
        if let cached = cache?.load() {
            store.seedFromCache(cached)
        }
        return ClientHandle(
            pairingId: instance.pairingId,
            instance: instance,
            client: client,
            store: store
        )
    }
}

/// The connector for the recordless `fallbackStore`. The fallback's supervisor
/// finds no record and returns before ever connecting, so this is never
/// invoked; it throws if asked, and — crucially — does NOT draw from the
/// injected `connectorFactory`, so a test's connector count maps 1:1 to real
/// paired instances.
private struct NeverConnectingConnector: WebSocketConnecting {
    func connect(to url: URL) async throws -> any WebSocketChannel {
        throw RelayConnectionError.closed
    }
}
