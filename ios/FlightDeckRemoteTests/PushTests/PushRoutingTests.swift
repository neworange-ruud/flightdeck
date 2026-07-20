//
//  PushRoutingTests.swift
//  FlightDeckRemoteTests
//
//  A tapped notification routes to the specific agent by reusing the shared
//  deep-link seam (PRD §5.2/§5.7): `AppRouter.pendingDeepLink` + Projects tab.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@MainActor
struct PushRoutingTests {

    private func routerWithPairing(_ pairingId: String) -> AppRouter {
        let store = PairingStore(instancesStorage: InMemoryPairedInstancesProvider())
        store.add(PairedInstance(
            pairingId: pairingId,
            relayURL: URL(string: "wss://relay.example/\(pairingId)")!))
        return AppRouter(pairingStore: store)
    }

    private func pushUserInfo(pairingId: String?) -> [AnyHashable: Any] {
        PushPayload.userInfo(
            eventId: "e",
            deepLink: Wire.DeepLink(
                projectId: Wire.ProjectId("p1"), sessionId: Wire.SessionId("s1"), itemId: nil),
            pairingId: pairingId)
    }

    @Test func validPayloadSetsPendingDeepLinkAndSwitchesToProjects() {
        let router = AppRouter(pairingStore: PairingStore())
        router.selectedTab = .settings

        let userInfo = PushPayload.userInfo(
            eventId: "e",
            deepLink: Wire.DeepLink(projectId: Wire.ProjectId("p1"), sessionId: Wire.SessionId("s1"), itemId: nil))

        #expect(PushRouting.route(userInfo: userInfo, in: router) == true)
        #expect(router.pendingDeepLink == DeepLink(projectId: "p1", sessionId: "s1"))
        #expect(router.selectedTab == .projects)
    }

    @Test func malformedPayloadDoesNotNavigate() {
        let router = AppRouter(pairingStore: PairingStore())
        router.selectedTab = .shell

        #expect(PushRouting.route(userInfo: ["nonsense": 1], in: router) == false)
        #expect(router.pendingDeepLink == nil)
        #expect(router.selectedTab == .shell) // unchanged
    }

    // MARK: - Multi-pairing pairingId resolution (remote-control-b8d.10)

    @Test func resolvedPairingIdRoutesAndCarriesItOnTheDeepLink() {
        let router = routerWithPairing("pair_ruud_mbp")
        router.selectedTab = .settings

        #expect(PushRouting.route(userInfo: pushUserInfo(pairingId: "pair_ruud_mbp"), in: router) == true)
        #expect(router.pendingDeepLink == DeepLink(projectId: "p1", sessionId: "s1", pairingId: "pair_ruud_mbp"))
        #expect(router.selectedTab == .projects)
    }

    @Test func unresolvedPairingIdIsIgnoredGracefully() {
        // The machine was unpaired between push delivery and tap: the pairingId
        // no longer resolves, so the tap must NOT navigate anywhere.
        let router = routerWithPairing("pair_still_here")
        router.selectedTab = .shell

        #expect(PushRouting.route(userInfo: pushUserInfo(pairingId: "pair_gone"), in: router) == false)
        #expect(router.pendingDeepLink == nil)
        #expect(router.selectedTab == .shell) // unchanged
    }

    @Test func payloadWithoutPairingIdStillRoutes() {
        // A relay push that predates the field (no pairing_id) routes the old,
        // machine-agnostic way rather than being dropped.
        let router = routerWithPairing("pair_a")
        router.selectedTab = .settings

        #expect(PushRouting.route(userInfo: pushUserInfo(pairingId: nil), in: router) == true)
        #expect(router.pendingDeepLink == DeepLink(projectId: "p1", sessionId: "s1"))
        #expect(router.selectedTab == .projects)
    }
}
