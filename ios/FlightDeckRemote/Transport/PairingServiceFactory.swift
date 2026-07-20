//
//  PairingServiceFactory.swift
//  FlightDeckRemote
//
//  The single place that decides which `PairingServicing` the app uses at the
//  composition root, so `PairingView` can default to it without any UI-layer
//  knowledge of the relay transport.
//
//  Switch rules (keeps every existing UI test green):
//   1. Any `-uitest…` launch argument → `MockPairingService`. The UI tests
//      pair by typing the documented code `4729` and assert the deterministic
//      mock's accept/reject behavior, so they must never touch a real relay.
//   2. Env override `FLIGHTDECK_PAIRING = real | mock` → forces that service,
//      so a developer can pin either service from any build.
//   3. Otherwise, in DEBUG: the mock on the *simulator* only (previews, local
//      dev without a paired Mac); the real relay-backed `RealPairingService`
//      on a *physical device*. Release always uses the real service.
//
//  Why the simulator/device split (remote-control-lae): the mock used to be
//  the silent DEBUG default on every build, so a developer testing pairing on
//  a real device against a real relay got a faked handshake that never opened a
//  socket — then the (always-real) transport reconnected against a pairing the
//  relay never knew about and hung on "reconnecting", with zero connections in
//  the relay logs. A physical DEBUG device now takes the real path by default,
//  and whenever the mock IS active we log loudly and surface an on-screen badge
//  (`PairingServiceFactory.isMock` → PairingView) so it can never be mistaken
//  for the real thing.
//

import Foundation

enum PairingServiceFactory {
    /// The default `PairingServicing` for this build/run.
    ///
    /// `pairingStore` (remote-control-b8d.4) is threaded through to
    /// `RealPairingService` so a real pairing appends its `PairedInstance` to
    /// the SAME reactive store the rest of the app (router/feed/transport/
    /// push/settings) observes — pass the app's shared `PairingStore`
    /// instance (e.g. `router.pairingStore`) here, not a fresh one, or the
    /// append won't be visible outside this service.
    static func makeDefault(
        arguments: [String] = ProcessInfo.processInfo.arguments,
        environment: [String: String] = ProcessInfo.processInfo.environment,
        pairingStore: PairingStore = PairingStore()
    ) -> PairingServicing {
        let service = resolve(arguments: arguments, environment: environment, pairingStore: pairingStore)
        if service is MockPairingService,
           !arguments.contains(where: { $0.hasPrefix("-uitest") }) {
            // Loud one-line warning at the composition root: a mocked handshake
            // never opens a WebSocket, so nothing will appear in the relay logs.
            NSLog("⚠️ FlightDeckRemote: PAIRING IS MOCKED (MockPairingService). "
                + "No relay connection will be made. Set FLIGHTDECK_PAIRING=real "
                + "or run on a physical device to use the live relay.")
        }
        return service
    }

    /// Whether the current build/run pairs against the deterministic mock rather
    /// than the live relay. Drives the on-screen "MOCK PAIRING" badge so a mocked
    /// handshake can never be mistaken for a real one (remote-control-lae).
    static func isMock(
        arguments: [String] = ProcessInfo.processInfo.arguments,
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> Bool {
        // `pairingStore` doesn't affect service *selection*, only what a real
        // service is wired to — a scratch instance is fine here.
        resolve(arguments: arguments, environment: environment, pairingStore: PairingStore()) is MockPairingService
    }

    /// Pure selection logic shared by `makeDefault` and `isMock` so both always
    /// agree on which service is active.
    private static func resolve(
        arguments: [String],
        environment: [String: String],
        pairingStore: PairingStore
    ) -> PairingServicing {
        // 1. UI tests rely on the deterministic mock.
        if arguments.contains(where: { $0.hasPrefix("-uitest") }) {
            return MockPairingService()
        }
        // 2. Explicit override.
        switch environment["FLIGHTDECK_PAIRING"] {
        case "real": return RealPairingService(pairingStore: pairingStore)
        case "mock": return MockPairingService()
        default: break
        }
        // 3. Build-configuration default.
        #if DEBUG
        #if targetEnvironment(simulator)
        // Simulator dev default: no paired Mac needed. Surfaced loudly + badged.
        return MockPairingService()
        #else
        // Physical DEBUG device: use the live relay, not a silent fake.
        return RealPairingService(pairingStore: pairingStore)
        #endif
        #else
        return RealPairingService(pairingStore: pairingStore)
        #endif
    }
}
