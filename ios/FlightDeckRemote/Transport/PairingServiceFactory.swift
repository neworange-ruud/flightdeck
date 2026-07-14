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
//      so a developer can exercise the real relay from a DEBUG build (or pin
//      the mock in a Release smoke build).
//   3. Otherwise: `MockPairingService` in DEBUG (developer default), the real
//      relay-backed `RealPairingService` in Release.
//

import Foundation

enum PairingServiceFactory {
    /// The default `PairingServicing` for this build/run.
    static func makeDefault(
        arguments: [String] = ProcessInfo.processInfo.arguments,
        environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> PairingServicing {
        // 1. UI tests rely on the deterministic mock.
        if arguments.contains(where: { $0.hasPrefix("-uitest") }) {
            return MockPairingService()
        }
        // 2. Explicit override.
        switch environment["FLIGHTDECK_PAIRING"] {
        case "real": return RealPairingService()
        case "mock": return MockPairingService()
        default: break
        }
        // 3. Build-configuration default.
        #if DEBUG
        return MockPairingService()
        #else
        return RealPairingService()
        #endif
    }
}
