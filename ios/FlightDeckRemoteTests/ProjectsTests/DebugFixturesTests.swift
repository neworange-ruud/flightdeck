//
//  DebugFixturesTests.swift
//  FlightDeckRemoteTests
//
//  Sanity checks for the `-uitest-fixture-snapshot` fixture
//  (`Wire.StateSnapshot.uiTestFixture`): stable project ids, representative
//  status coverage (a needs-input session carries a pending question), and
//  every session has the fields the row rendering depends on.
//

#if DEBUG
import Testing
@testable import FlightDeckRemote

struct DebugFixturesTests {

    @Test func fixtureHasThreeProjectsWithExpectedIds() {
        let snapshot = Wire.StateSnapshot.uiTestFixture
        let ids = Set(snapshot.projects.map(\.projectId.rawValue))
        #expect(ids == Set([
            Wire.StateSnapshot.FixtureIds.flightdeck,
            Wire.StateSnapshot.FixtureIds.remoteControl,
            Wire.StateSnapshot.FixtureIds.marketingSite,
        ]))
    }

    @Test func flightdeckProjectNeedsInputAndHasPendingQuestion() {
        let snapshot = Wire.StateSnapshot.uiTestFixture
        let flightdeck = snapshot.projects.first {
            $0.projectId.rawValue == Wire.StateSnapshot.FixtureIds.flightdeck
        }
        #expect(flightdeck?.rollup.dot == .needsInput)
        let needsInputSession = flightdeck?.sessions.first { $0.status == .needsInput }
        #expect(needsInputSession?.pendingQuestion?.isEmpty == false)
    }

    @Test func everySessionHasAllRowFieldsPopulated() {
        for project in Wire.StateSnapshot.uiTestFixture.projects {
            #expect(!project.name.isEmpty)
            for session in project.sessions {
                #expect(!session.name.isEmpty)
                #expect(session.projectId == project.projectId)
            }
        }
    }

    @Test func rollupsAreConsistentWithTheirSessions() {
        for project in Wire.StateSnapshot.uiTestFixture.projects {
            #expect(project.rollup.agentCount == UInt32(project.sessions.count))
        }
    }
}
#endif
