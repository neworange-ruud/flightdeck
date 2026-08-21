//
//  PushRouting.swift
//  FlightDeckRemote
//
//  Turns a tapped notification into in-app navigation (PRD §5.2/§5.7: tap
//  deep-links straight to the agent) and provides the APNs environment /
//  device-token helpers the registration glue needs.
//
//  Routing deliberately reuses the exact same seam the `flightdeck-remote://`
//  URL scheme already uses — set `AppRouter.pendingDeepLink` and switch to the
//  Projects tab — rather than duplicating navigation logic
//  (see `AppRouter.handleDeepLink`).
//

import Foundation

/// Routes a tapped notification's `userInfo` into the app.
@MainActor
enum PushRouting {
    /// Parse `userInfo` and, if it carries a valid deep link, navigate to the
    /// agent (Projects tab + `pendingDeepLink`). Returns whether it routed —
    /// mainly for tests. A malformed payload is ignored (no navigation).
    ///
    /// Multi-pairing (remote-control-b8d.10): when the payload names a
    /// `pairingId`, it must still resolve to a currently-paired machine. If it
    /// doesn't — the machine was unpaired between push delivery and tap — the
    /// tap is ignored gracefully (no navigation) rather than deep-linking into
    /// a machine that's gone. A payload with no `pairingId` (a relay push that
    /// predates the field, or the single-pairing path) routes as before.
    @discardableResult
    static func route(userInfo: [AnyHashable: Any], in router: AppRouter) -> Bool {
        guard let payload = PushPayload(userInfo: userInfo) else { return false }
        if let pairingId = payload.pairingId,
           !router.pairingStore.list.contains(where: { $0.pairingId == pairingId }) {
            return false
        }
        router.pendingDeepLink = payload.appDeepLink
        router.selectedTab = .projects
        return true
    }
}

/// APNs environment + device-token helpers.
enum PushEnvironment {
    /// The APNs environment this build targets. DEBUG builds are provisioned
    /// against the sandbox APNs; Release against production (spec §5.5
    /// `environment`). The relay uses this to pick the right APNs host.
    static var current: Wire.ApnsEnvironment {
        #if DEBUG
        return .sandbox
        #else
        return .production
        #endif
    }

    /// Encode a raw APNs device-token `Data` as the lowercase hex string the
    /// relay stores and Apple expects on the `/3/device/<token>` path.
    static func hexToken(from data: Data) -> String {
        data.map { String(format: "%02x", $0) }.joined()
    }
}
