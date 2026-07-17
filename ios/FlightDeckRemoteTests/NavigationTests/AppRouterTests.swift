//
//  AppRouterTests.swift
//  FlightDeckRemoteTests
//
//  Verifies AppRouter's entry-flow routing (PRD §5.8: unpaired -> Pairing,
//  paired -> main tab container), that it starts on the Projects tab, and
//  that a valid deep link stores `pendingDeepLink` and switches to the
//  Projects tab while a malformed one is ignored (PRD §5.2/§5.7).
//
//  Uses `InMemoryPairingStateProvider` (PairingStoreTests.swift) so pairing
//  state here never touches the real `UserDefaults`.
//

import Testing
import Foundation
@testable import FlightDeckRemote

struct AppRouterTests {

    private func makeRouter(paired: Bool = false) -> AppRouter {
        let store = PairingStore(storage: InMemoryPairingStateProvider(initial: paired))
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
