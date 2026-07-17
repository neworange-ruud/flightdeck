//
//  ProjectsNavModelTests.swift
//  FlightDeckRemoteTests
//
//  Sanity check for the Projects tab's typed `NavigationStack` path — the
//  hook point later feature tasks push sessions/chat destinations onto.
//

import Testing
@testable import FlightDeckRemote

struct ProjectsNavModelTests {

    @Test func startsEmptyAndAppendsTypedRoutes() {
        let model = ProjectsNavModel()
        #expect(model.path.isEmpty)

        model.path.append(.sessions(projectId: "proj-1"))
        model.path.append(.chat(projectId: "proj-1", sessionId: "sess-1"))

        #expect(model.path.count == 2)
        #expect(model.path == [
            .sessions(projectId: "proj-1"),
            .chat(projectId: "proj-1", sessionId: "sess-1"),
        ])
    }
}
