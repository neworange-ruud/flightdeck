//
//  ProjectsSearchTests.swift
//  FlightDeckRemoteTests
//
//  Covers `ProjectsSearch.filter` (PRD §5.2 search affordance): empty/blank
//  query passthrough, case-insensitive substring matching, and no-match.
//

import Testing
@testable import FlightDeckRemote

struct ProjectsSearchTests {

    private func project(_ name: String) -> Wire.ProjectState {
        Wire.ProjectState(
            projectId: Wire.ProjectId("proj_\(name)"),
            name: name,
            rollup: Wire.StatusRollup(
                dot: .idle, summary: "idle · 1 agent", working: 0, idle: 1,
                needsInput: 0, manual: 0, agentCount: 1),
            sessions: [])
    }

    @Test func emptyQueryReturnsAllProjects() {
        let projects = [project("flightdeck"), project("remote-control")]
        #expect(ProjectsSearch.filter(projects: projects, query: "").count == 2)
    }

    @Test func whitespaceOnlyQueryReturnsAllProjects() {
        let projects = [project("flightdeck")]
        #expect(ProjectsSearch.filter(projects: projects, query: "   ").count == 1)
    }

    @Test func filtersCaseInsensitiveSubstring() {
        let projects = [project("FlightDeck"), project("remote-control")]
        let result = ProjectsSearch.filter(projects: projects, query: "flight")
        #expect(result.map(\.name) == ["FlightDeck"])
    }

    @Test func noMatchReturnsEmpty() {
        let projects = [project("flightdeck")]
        #expect(ProjectsSearch.filter(projects: projects, query: "zzz").isEmpty)
    }

    @Test func matchesMidNameSubstring() {
        let projects = [project("remote-control"), project("flightdeck")]
        let result = ProjectsSearch.filter(projects: projects, query: "control")
        #expect(result.map(\.name) == ["remote-control"])
    }
}
