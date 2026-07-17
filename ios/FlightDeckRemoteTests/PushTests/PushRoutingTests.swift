//
//  PushRoutingTests.swift
//  FlightDeckRemoteTests
//
//  A tapped notification routes to the specific agent by reusing the shared
//  deep-link seam (PRD §5.2/§5.7): `AppRouter.pendingDeepLink` + Projects tab.
//

import Testing
@testable import FlightDeckRemote

@MainActor
struct PushRoutingTests {

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
}
