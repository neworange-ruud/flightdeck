//
//  AppRouter.swift
//  FlightDeckRemote
//
//  Top-level entry-flow routing (PRD §5.8): unpaired devices land on the
//  Pairing screen; paired devices land on the main tab container. This is
//  the app's single observable source of navigation truth:
//   - `route` — pairing vs. main, derived from `pairingStore.isPaired`.
//   - `selectedTab` — which bottom tab is showing (PRD §5.7).
//   - `pendingDeepLink` — the last `flightdeck-remote://` URL parsed via
//     `handleDeepLink(url:)` (PRD §5.2/§5.7: notifications deep-link
//     straight to the agent). Landing on `.projects` and proving the parse
//     is this task's job; actually pushing into the session's chat via
//     `ProjectsNavModel.path` is a later task's job.
//
//  `RootView` reads `route` to choose between `PairingView` and
//  `MainTabView`, and wires `onOpenURL` to `handleDeepLink(url:)`.
//

import Foundation
import Observation

/// The two top-level destinations the app can be in.
enum AppRoute: Equatable {
    case pairing
    case main
}

/// Owns app-wide navigation state: entry-flow routing, the selected bottom
/// tab, and the deep-link seam.
@Observable
final class AppRouter {
    var pairingStore: PairingStore
    var selectedTab: AppTab = .projects
    var pendingDeepLink: DeepLink?

    init(pairingStore: PairingStore) {
        self.pairingStore = pairingStore
    }

    /// Chooses the root screen based on pairing state.
    var route: AppRoute {
        pairingStore.isPaired ? .main : .pairing
    }

    /// Parses a `flightdeck-remote://` URL (see `DeepLink`). On success,
    /// stores it on `pendingDeepLink` and switches to the Projects tab so
    /// the user lands where the deep link is heading; malformed/unknown
    /// URLs are ignored entirely (no state change).
    ///
    /// Returns whether the URL was recognized, mainly for tests.
    @discardableResult
    func handleDeepLink(url: URL) -> Bool {
        guard let link = DeepLink(url: url) else { return false }
        pendingDeepLink = link
        selectedTab = .projects
        return true
    }
}
