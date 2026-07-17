//
//  ProjectsAggregateRollupTests.swift
//  FlightDeckRemoteTests
//
//  Covers `RollupModel.aggregateSubtitle` (PRD §5.2 header subtitle): the
//  "1 project needs you · 2 working" pattern, singular/plural phrasing, the
//  all-idle form, and the empty-projects form.
//

import Testing
import Foundation
@testable import FlightDeckRemote

struct ProjectsAggregateRollupTests {

    private func project(dot: Wire.RollupDot) -> Wire.ProjectState {
        Wire.ProjectState(
            projectId: Wire.ProjectId(UUID().uuidString),
            name: "proj",
            rollup: Wire.StatusRollup(
                dot: dot, summary: "x", working: 0, idle: 0,
                needsInput: 0, manual: 0, agentCount: 1),
            sessions: [])
    }

    @Test func noProjectsYet() {
        #expect(RollupModel.aggregateSubtitle(projects: []) == "No projects yet")
    }

    @Test func matchesPRDMixedExample() {
        // The PRD example: "1 project needs you · 2 working".
        let projects = [project(dot: .needsInput), project(dot: .working), project(dot: .working)]
        #expect(RollupModel.aggregateSubtitle(projects: projects) == "1 project needs you · 2 working")
    }

    @Test func allIdlePlural() {
        let projects = [project(dot: .idle), project(dot: .idle)]
        #expect(RollupModel.aggregateSubtitle(projects: projects) == "2 projects · idle")
    }

    @Test func allIdleSingular() {
        #expect(RollupModel.aggregateSubtitle(projects: [project(dot: .idle)]) == "1 project · idle")
    }

    @Test func pluralNeedsInputAndManualOmitsWorking() {
        let projects = [project(dot: .needsInput), project(dot: .needsInput), project(dot: .manual)]
        #expect(RollupModel.aggregateSubtitle(projects: projects) == "2 projects need you · 1 manual")
    }

    @Test func singleNeedsInputProjectIsSingular() {
        #expect(RollupModel.aggregateSubtitle(projects: [project(dot: .needsInput)]) == "1 project needs you")
    }
}
