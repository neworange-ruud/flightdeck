//
//  SessionSortTests.swift
//  FlightDeckRemoteTests
//
//  Covers `SessionSort.sorted` (PRD §5.2): needs-input, then working, then
//  manual, then idle; ties keep their original relative order.
//

import Testing
@testable import FlightDeckRemote

struct SessionSortTests {

    private func session(_ status: Wire.AgentStatus, name: String) -> Wire.SessionState {
        Wire.SessionState(
            sessionId: Wire.SessionId(name), projectId: Wire.ProjectId("p"),
            name: name, agentType: .claudeCode, status: status,
            git: Wire.GitIndicators(
                branch: "main", added: 0, modified: 0, removed: 0,
                ahead: 0, behind: 0, drift: 0, hasUpstream: true),
            runningTimeSecs: 0, pendingQuestion: nil)
    }

    @Test func needsInputFirstThenWorkingThenManualThenIdle() {
        let sessions = [
            session(.idle, name: "idle-1"),
            session(.manual(label: "hold"), name: "manual-1"),
            session(.needsInput, name: "needs-1"),
            session(.working, name: "working-1"),
        ]
        #expect(SessionSort.sorted(sessions).map(\.name) == ["needs-1", "working-1", "manual-1", "idle-1"])
    }

    @Test func tiesPreserveOriginalRelativeOrder() {
        let sessions = [
            session(.working, name: "working-a"),
            session(.working, name: "working-b"),
            session(.idle, name: "idle-a"),
            session(.idle, name: "idle-b"),
        ]
        #expect(SessionSort.sorted(sessions).map(\.name) == ["working-a", "working-b", "idle-a", "idle-b"])
    }

    @Test func alreadySortedListIsUnchanged() {
        let sessions = [session(.needsInput, name: "n"), session(.idle, name: "i")]
        #expect(SessionSort.sorted(sessions).map(\.name) == ["n", "i"])
    }

    @Test func emptyListStaysEmpty() {
        #expect(SessionSort.sorted([]).isEmpty)
    }
}
