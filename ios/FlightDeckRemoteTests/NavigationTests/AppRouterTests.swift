//
//  AppRouterTests.swift
//  FlightDeckRemoteTests
//
//  Verifies AppRouter's entry-flow routing (PRD §5.8, extended for multi-
//  pairing remote-control-b8d.7: ZERO paired instances -> Pairing/onboarding,
//  ONE OR MORE -> main tab container / feed), that it starts on the Projects
//  tab, and that a valid deep link stores `pendingDeepLink` and switches to
//  the Projects tab while a malformed one is ignored (PRD §5.2/§5.7).
//
//  Uses `InMemoryPairingStateProvider` (PairingStoreTests.swift) and
//  `InMemoryPairedInstancesProvider` (PairedInstanceStoreTests.swift) so
//  pairing state here never touches the real `UserDefaults`.
//

import Testing
import Foundation
@testable import FlightDeckRemote

struct AppRouterTests {

    private let relayURL = URL(string: "wss://relay.flightdeck.app/v1")!

    private func makeRouter(
        paired: Bool = false,
        instancesStorage: PairedInstancesProviding = InMemoryPairedInstancesProvider()
    ) -> AppRouter {
        let store = PairingStore(
            storage: InMemoryPairingStateProvider(initial: paired),
            instancesStorage: instancesStorage
        )
        return AppRouter(pairingStore: store)
    }

    @Test func routesToPairingWhenUnpaired() {
        let router = makeRouter(paired: false)
        #expect(router.route == .pairing)
    }

    @Test func routesToMainWhenPaired() {
        let router = makeRouter(paired: true)
        #expect(router.route == .main)
    }

    @Test func routeTracksLivePairingStateChanges() {
        let router = makeRouter(paired: false)
        #expect(router.route == .pairing)

        router.pairingStore.isPaired = true
        #expect(router.route == .main)
    }

    /// remote-control-b8d.7 acceptance criterion: "0 pairings shows
    /// onboarding; >=1 shows the feed" — driven by the count-based
    /// `[PairedInstance]` list, NOT the legacy single-device `isPaired` flag.
    @Test func routesToPairingWithZeroPairedInstancesAndLegacyIsPairedFalse() {
        let router = makeRouter(paired: false)
        #expect(router.pairingStore.list.isEmpty)
        #expect(router.route == .pairing)
    }

    /// The router must react to `[PairedInstance]` on its own — a real
    /// pairing (`RealPairingService.pair`) only calls `PairingStore.add`, not
    /// the legacy `completePairing(with:)`/`isPaired` setter, when appending a
    /// second-or-later machine (remote-control-b8d.4). Routing must not
    /// silently depend on the legacy flag also being flipped.
    @Test func routesToMainAssoonAsOneInstanceIsAddedEvenIfLegacyIsPairedStaysFalse() {
        let router = makeRouter(paired: false)
        #expect(router.route == .pairing)

        router.pairingStore.add(PairedInstance(pairingId: "pair-1", relayURL: relayURL))

        #expect(router.pairingStore.isPaired == false, "legacy flag is untouched by add(_:)")
        #expect(router.route == .main, "one paired instance is enough to route to the feed")
    }

    /// A second (or third/fourth) paired instance keeps routing to the feed —
    /// multi-pairing is additive, never a reason to bounce back to onboarding.
    @Test func routesToMainWithMultiplePairedInstances() {
        let router = makeRouter(paired: false)
        router.pairingStore.add(PairedInstance(pairingId: "pair-1", relayURL: relayURL))
        router.pairingStore.add(PairedInstance(pairingId: "pair-2", relayURL: relayURL))

        #expect(router.route == .main)
    }

    /// Removing every paired instance (all machines unpaired) routes back to
    /// onboarding, mirroring the "0 pairings -> onboarding" criterion in the
    /// other direction.
    @Test func routesBackToPairingWhenLastInstanceIsRemoved() {
        let router = makeRouter(paired: false)
        router.pairingStore.add(PairedInstance(pairingId: "pair-1", relayURL: relayURL))
        #expect(router.route == .main)

        router.pairingStore.remove(pairingId: "pair-1")

        #expect(router.route == .pairing)
    }

    @Test func startsOnProjectsTab() {
        let router = makeRouter(paired: true)
        #expect(router.selectedTab == .projects)
    }

    @Test func validDeepLinkSetsPendingLinkAndSwitchesToProjectsTab() throws {
        let router = makeRouter(paired: true)
        router.selectedTab = .settings

        let url = try #require(URL(string: "flightdeck-remote://agent/proj-1/sess-42"))
        let handled = router.handleDeepLink(url: url)

        #expect(handled == true)
        #expect(router.pendingDeepLink == DeepLink(projectId: "proj-1", sessionId: "sess-42"))
        #expect(router.selectedTab == .projects)
    }

    @Test func malformedDeepLinkIsIgnoredAndLeavesStateUnchanged() throws {
        let router = makeRouter(paired: true)
        router.selectedTab = .shell

        let url = try #require(URL(string: "https://agent/proj-1/sess-42"))
        let handled = router.handleDeepLink(url: url)

        #expect(handled == false)
        #expect(router.pendingDeepLink == nil)
        #expect(router.selectedTab == .shell)
    }
}
