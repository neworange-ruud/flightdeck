//
//  NewAgentFormModelTests.swift
//  FlightDeckRemoteTests
//
//  The New-Agent form (PRD §5.5): live slug/branch preview matching the
//  desktop's rules, launchability validation (deliberate + paused-gated),
//  the built `new_agent` command, and the snapshot-derived defaults.
//

import XCTest
@testable import FlightDeckRemote

@MainActor
final class NewAgentFormModelTests: XCTestCase {

    private let projectId = Wire.ProjectId("proj_flightdeck")

    private func filledModel() -> NewAgentFormModel {
        let model = NewAgentFormModel()
        model.selectedProjectId = projectId
        model.agentType = .claudeCode
        model.name = "Add rate limit"
        model.baseBranch = "main"
        model.firstTask = "Add a rate limiter to the API."
        return model
    }

    // MARK: - Slug preview

    func testBranchPreviewMirrorsDesktopSlugify() {
        let model = NewAgentFormModel()
        model.name = "Add rate limit"
        XCTAssertEqual(model.slug, "add-rate-limit")
        XCTAssertEqual(model.branchPreview, "flightdeck/add-rate-limit")

        model.name = "Fix the Login Bug!"
        XCTAssertEqual(model.branchPreview, "flightdeck/fix-the-login-bug")
    }

    func testBranchPreviewNilWhileNameYieldsEmptySlug() {
        let model = NewAgentFormModel()
        XCTAssertNil(model.branchPreview)
        model.name = "!!!"
        XCTAssertNil(model.branchPreview)
    }

    // MARK: - Launchability

    func testLaunchableOnlyWhenComplete() {
        let model = filledModel()
        XCTAssertTrue(model.isLaunchable(commandsPaused: false))

        model.name = "  !! "
        XCTAssertFalse(model.isLaunchable(commandsPaused: false), "Needs a sluggable name")
        model.name = "Add rate limit"

        model.baseBranch = "   "
        XCTAssertFalse(model.isLaunchable(commandsPaused: false), "Needs a base branch")
        model.baseBranch = "main"

        model.firstTask = "\n  "
        XCTAssertFalse(model.isLaunchable(commandsPaused: false), "Needs a first task")
        model.firstTask = "Do the thing."

        model.selectedProjectId = nil
        XCTAssertFalse(model.isLaunchable(commandsPaused: false), "Needs a project")
    }

    func testNeverLaunchableWhilePaused() {
        let model = filledModel()
        XCTAssertFalse(model.isLaunchable(commandsPaused: true))
    }

    // MARK: - Command body

    func testCommandBodyCarriesSlugAndTrimmedFields() {
        let model = filledModel()
        model.agentType = .codex
        model.baseBranch = " develop "
        model.firstTask = "  Add a rate limiter.  "
        XCTAssertEqual(model.commandBody(),
                       .newAgent(projectId: projectId, agentType: .codex,
                                 name: "add-rate-limit", baseBranch: "develop",
                                 firstTask: "Add a rate limiter."))
    }

    func testCommandBodyNilWhileIncomplete() {
        let model = filledModel()
        model.firstTask = ""
        XCTAssertNil(model.commandBody())
    }

    // MARK: - Defaults from the snapshot

    private func snapshot(sessions: [Wire.SessionState]) -> Wire.StateSnapshot {
        Wire.StateSnapshot(serverTimeMs: 0, projects: [
            Wire.ProjectState(
                projectId: projectId, name: "flightdeck",
                rollup: Wire.StatusRollup(dot: .idle, summary: "", working: 0, idle: 0,
                                          needsInput: 0, manual: 0,
                                          agentCount: UInt32(sessions.count)),
                sessions: sessions),
        ])
    }

    private func session(_ id: String) -> Wire.SessionState {
        Wire.SessionState(
            sessionId: Wire.SessionId(id), projectId: projectId, name: id,
            agentType: .claudeCode, status: .idle,
            git: Wire.GitIndicators(branch: id, added: 0, modified: 0, removed: 0,
                                    ahead: 0, behind: 0, drift: 0, hasUpstream: true),
            runningTimeSecs: 0, pendingQuestion: nil)
    }

    func testDefaultsSelectFirstProjectAndKeepMainWithoutGitStatus() {
        let model = NewAgentFormModel()
        model.applyDefaults(snapshot: snapshot(sessions: [session("s1")]), gitStatus: [:])
        XCTAssertEqual(model.selectedProjectId, projectId)
        XCTAssertEqual(model.baseBranch, "main")
    }

    func testDefaultsAdoptProjectBaseBranchFromGitStatus() {
        let model = NewAgentFormModel()
        let detail = Wire.GitStatusDetail(
            sessionId: Wire.SessionId("s1"), branch: "s1", baseBranch: "develop",
            hasUpstream: true, ahead: 0, behind: 0, drift: 0, files: [])
        model.applyDefaults(snapshot: snapshot(sessions: [session("s1")]),
                            gitStatus: [Wire.SessionId("s1"): detail])
        XCTAssertEqual(model.baseBranch, "develop")
    }

    func testDefaultsKeepExplicitProjectSelection() {
        let model = NewAgentFormModel()
        let other = Wire.ProjectId("proj_other")
        model.selectedProjectId = other
        model.applyDefaults(snapshot: snapshot(sessions: []), gitStatus: [:])
        XCTAssertEqual(model.selectedProjectId, other)
    }
}
