//
//  ProjectsNavModelTests.swift
//  FlightDeckRemoteTests
//
//  Sanity check for the Projects tab's typed `NavigationStack` path — the
//  hook point later feature tasks push sessions/chat destinations onto. Also
//  covers `ProjectsRoute`'s `pairingId` (remote-control-b8d.12): the Feed tab
//  reuses this same route type, pinning each pushed destination to the
//  machine it was opened for.
//

import Testing
@testable import FlightDeckRemote

struct ProjectsNavModelTests {

    @Test func startsEmptyAndAppendsTypedRoutes() {
        let model = ProjectsNavModel()
        #expect(model.path.isEmpty)

        model.path.append(.sessions(projectId: "proj-1", pairingId: nil))
        model.path.append(.chat(projectId: "proj-1", sessionId: "sess-1", pairingId: nil))

        #expect(model.path.count == 2)
        #expect(model.path == [
            .sessions(projectId: "proj-1", pairingId: nil),
            .chat(projectId: "proj-1", sessionId: "sess-1", pairingId: nil),
        ])
    }

    /// Two routes with identical `projectId`/`sessionId` but different
    /// `pairingId`s are distinct values (derived `Hashable`/`Equatable`) —
    /// exactly the property `MainTabView.detailStore(for:)` depends on:
    /// which machine a pushed destination binds to is baked into the route
    /// value itself, not read from elsewhere.
    @Test func routesWithDifferentPairingIdsAreNotEqual() {
        let onMachineA = ProjectsRoute.sessions(projectId: "proj-1", pairingId: "pair_a")
        let onMachineB = ProjectsRoute.sessions(projectId: "proj-1", pairingId: "pair_b")
        #expect(onMachineA != onMachineB)

        let chatOnMachineA = ProjectsRoute.chat(projectId: "proj-1", sessionId: "sess-1", pairingId: "pair_a")
        let chatOnMachineB = ProjectsRoute.chat(projectId: "proj-1", sessionId: "sess-1", pairingId: "pair_b")
        #expect(chatOnMachineA != chatOnMachineB)
    }
}
