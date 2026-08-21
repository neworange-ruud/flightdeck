//
//  RollupModelTests.swift
//  FlightDeckRemoteTests
//
//  Covers every precedence branch of the client roll-up (PRD §4:
//  needs-input > working > manual > idle; manual counts shown but never
//  outranking), the dot -> design-token color mapping, the plain-language
//  summary patterns, and the desktop-rollup-preferred path.
//

import Testing
import SwiftUI
@testable import FlightDeckRemote

struct RollupModelTests {

    // MARK: - Helpers

    private func session(
        _ status: Wire.AgentStatus,
        name: String = "fix-login"
    ) -> Wire.SessionState {
        Wire.SessionState(
            sessionId: Wire.SessionId("sess_\(name)"),
            projectId: Wire.ProjectId("proj_flightdeck"),
            name: name,
            agentType: .claudeCode,
            status: status,
            git: Wire.GitIndicators(
                branch: "flightdeck/\(name)", added: 0, modified: 0,
                removed: 0, ahead: 0, behind: 0, drift: 0, hasUpstream: true),
            runningTimeSecs: 0,
            pendingQuestion: nil)
    }

    // MARK: - Dot precedence (every branch)

    @Test func needsInputOutranksEverything() {
        let vm = RollupModel.rollup(sessions: [
            session(.working), session(.needsInput),
            session(.manual(label: "hold")), session(.idle),
        ])
        #expect(vm.dot == .needsInput)
    }

    @Test func workingOutranksManualAndIdle() {
        let vm = RollupModel.rollup(sessions: [
            session(.idle), session(.manual(label: "hold")), session(.working),
        ])
        #expect(vm.dot == .working)
    }

    @Test func manualOutranksOnlyIdle() {
        let vm = RollupModel.rollup(sessions: [
            session(.idle), session(.manual(label: "reviewing by hand")),
        ])
        #expect(vm.dot == .manual)
    }

    @Test func allIdleIsIdle() {
        let vm = RollupModel.rollup(sessions: [session(.idle), session(.idle)])
        #expect(vm.dot == .idle)
    }

    @Test func emptyProjectIsIdle() {
        let vm = RollupModel.rollup(sessions: [])
        #expect(vm.dot == .idle)
        #expect(vm.agentCount == 0)
        #expect(vm.summary == "no agents")
    }

    @Test func manualNeverOutranksNeedsInputOrWorking() {
        #expect(RollupModel.dot(needsInput: 1, working: 0, manual: 5) == .needsInput)
        #expect(RollupModel.dot(needsInput: 0, working: 1, manual: 5) == .working)
        #expect(RollupModel.dot(needsInput: 0, working: 0, manual: 5) == .manual)
        #expect(RollupModel.dot(needsInput: 0, working: 0, manual: 0) == .idle)
    }

    // MARK: - Counts

    @Test func countsEveryState() {
        let vm = RollupModel.rollup(sessions: [
            session(.needsInput), session(.working), session(.working),
            session(.manual(label: "hold")), session(.idle), session(.idle),
        ])
        #expect(vm.needsInput == 1)
        #expect(vm.working == 2)
        #expect(vm.manual == 1)
        #expect(vm.idle == 2)
        #expect(vm.agentCount == 6)
    }

    // MARK: - Summary formatting (PRD §4 patterns)

    @Test func summaryMatchesPRDMixedExample() {
        // The PRD/fixture example: "1 needs input · 1 working · 3 agents".
        let vm = RollupModel.rollup(sessions: [
            session(.needsInput), session(.working), session(.idle),
        ])
        #expect(vm.summary == "1 needs input · 1 working · 3 agents")
    }

    @Test func summaryAllIdlePlural() {
        let vm = RollupModel.rollup(sessions: [session(.idle), session(.idle)])
        #expect(vm.summary == "idle · 2 agents")
    }

    @Test func summaryAllIdleSingular() {
        let vm = RollupModel.rollup(sessions: [session(.idle)])
        #expect(vm.summary == "idle · 1 agent")
    }

    @Test func summarySingleSessionFormsDropRedundantCount() {
        #expect(RollupModel.rollup(sessions: [session(.working)]).summary
                == "1 working")
        #expect(RollupModel.rollup(sessions: [session(.needsInput)]).summary
                == "1 needs input")
        #expect(RollupModel.rollup(sessions: [session(.manual(label: "hold"))]).summary
                == "1 manual")
    }

    @Test func summaryIncludesManualCountWithoutOutranking() {
        let vm = RollupModel.rollup(sessions: [
            session(.working), session(.manual(label: "hold")),
        ])
        #expect(vm.dot == .working)
        #expect(vm.summary == "1 working · 1 manual · 2 agents")
    }

    @Test func summaryOmitsZeroSegments() {
        let vm = RollupModel.rollup(sessions: [
            session(.working), session(.working), session(.idle),
        ])
        #expect(vm.summary == "2 working · 3 agents")
    }

    @Test func summaryNeedsInputWithManual() {
        let vm = RollupModel.rollup(sessions: [
            session(.needsInput), session(.manual(label: "hold")), session(.idle),
        ])
        #expect(vm.dot == .needsInput)
        #expect(vm.summary == "1 needs input · 1 manual · 3 agents")
    }

    // MARK: - Dot color mapping (design tokens)

    @Test func dotColorsMapToThemeTokens() {
        #expect(RollupModel.color(for: .needsInput) == Theme.statusNeedsInput)
        #expect(RollupModel.color(for: .working) == Theme.statusWorking)
        #expect(RollupModel.color(for: .manual) == Theme.statusManual)
        #expect(RollupModel.color(for: .idle) == Theme.statusIdle)

        let vm = RollupModel.rollup(sessions: [session(.needsInput)])
        #expect(vm.dotColor == Theme.statusNeedsInput)
    }

    // MARK: - Desktop rollup preferred (summary hints used verbatim)

    @Test func desktopProvidedRollupIsUsedVerbatim() {
        // The desktop can say things the client cannot derive, e.g.
        // "1 done, ready to push" (PRD §4). It must pass through untouched
        // even when locally-computable counts would phrase it differently.
        let desktopRollup = Wire.StatusRollup(
            dot: .idle, summary: "1 done, ready to push",
            working: 0, idle: 1, needsInput: 0, manual: 0, agentCount: 1)
        let project = Wire.ProjectState(
            projectId: Wire.ProjectId("proj_flightdeck"),
            name: "flightdeck",
            rollup: desktopRollup,
            sessions: [session(.idle)])

        let vm = RollupModel.viewModel(for: project)
        #expect(vm.summary == "1 done, ready to push")
        #expect(vm.dot == .idle)
        #expect(vm.dotColor == Theme.statusIdle)

        // The pure-init path maps every count through.
        let direct = ProjectRollupViewModel(rollup: desktopRollup)
        #expect(direct == vm)
        #expect(direct.idle == 1)
        #expect(direct.agentCount == 1)
    }

    @Test func localRollupReproducesDesktopSummaryPattern() {
        // Local fallback should agree with the desktop's summary phrasing
        // for the count-based pattern (the snapshot fixture's rollup).
        let vm = ProjectRollupViewModel(
            dot: RollupModel.dot(needsInput: 1, working: 1, manual: 0),
            summary: RollupModel.summary(
                needsInput: 1, working: 1, manual: 0, agentCount: 3),
            needsInput: 1, working: 1, manual: 0, idle: 1, agentCount: 3)
        #expect(vm.dot == .needsInput)
        #expect(vm.summary == "1 needs input · 1 working · 3 agents")
    }
}
