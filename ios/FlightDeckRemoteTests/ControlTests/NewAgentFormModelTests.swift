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

    // MARK: - Base branch helper

    func testDefaultBaseBranchStaticFromGitStatusElseMain() {
        let project = self.snapshot(sessions: [session("s1")]).projects[0]
        XCTAssertEqual(
            NewAgentFormModel.defaultBaseBranch(project: project, gitStatus: [:]),
            "main")

        let detail = Wire.GitStatusDetail(
            sessionId: Wire.SessionId("s1"), branch: "s1", baseBranch: "develop",
            hasUpstream: true, ahead: 0, behind: 0, drift: 0, files: [])
        XCTAssertEqual(
            NewAgentFormModel.defaultBaseBranch(
                project: project, gitStatus: [Wire.SessionId("s1"): detail]),
            "develop")
    }

    // MARK: - Aggregated (multi-machine) defaults

    private func makeStore() throws -> TransportStore {
        let keychain = InMemoryKeychainStore()
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        let keyAgreement = try KeyAgreementKeys.loadOrCreate(store: keychain)
        let recordStore = PairingRecordStore(store: keychain)
        let client = TransportClient(
            identity: identity, keyAgreement: keyAgreement, recordStore: recordStore,
            connector: ScriptedConnector(channel: ScriptedChannel()))
        return TransportStore(client: client)
    }

    private func option(pairingId: String, machineName: String?, projectId: String,
                        baseBranch: String, store: TransportStore) -> NewAgentProjectOption {
        let pid = Wire.ProjectId(projectId)
        let project = Wire.ProjectState(
            projectId: pid, name: projectId,
            rollup: Wire.StatusRollup(dot: .idle, summary: "", working: 0, idle: 0,
                                      needsInput: 0, manual: 0, agentCount: 0),
            sessions: [])
        return NewAgentProjectOption(
            pairingId: pairingId, machineName: machineName, project: project,
            store: store, defaultBaseBranch: baseBranch)
    }

    func testAggregatedDefaultsSelectFirstOptionAndAdoptItsBase() throws {
        let store = try makeStore()
        let options = [
            option(pairingId: "mac-a", machineName: "Studio", projectId: "proj_a",
                   baseBranch: "develop", store: store),
            option(pairingId: "mac-b", machineName: "MacBook", projectId: "proj_b",
                   baseBranch: "main", store: store),
        ]
        let model = NewAgentFormModel()
        model.applyDefaults(options: options)

        XCTAssertEqual(model.selectedProjectId, Wire.ProjectId("proj_a"))
        XCTAssertEqual(model.selectedPairingId, "mac-a")
        XCTAssertEqual(model.baseBranch, "develop")
    }

    func testAggregatedDefaultsMatchSelectionByBothProjectAndPairing() throws {
        let store = try makeStore()
        // Same project id on two machines — only the pairing distinguishes them.
        let options = [
            option(pairingId: "mac-a", machineName: "Studio", projectId: "proj_shared",
                   baseBranch: "trunk-a", store: store),
            option(pairingId: "mac-b", machineName: "MacBook", projectId: "proj_shared",
                   baseBranch: "trunk-b", store: store),
        ]
        let model = NewAgentFormModel()
        model.selectedProjectId = Wire.ProjectId("proj_shared")
        model.selectedPairingId = "mac-b"
        model.applyDefaults(options: options)

        XCTAssertEqual(model.selectedPairingId, "mac-b")
        XCTAssertEqual(model.baseBranch, "trunk-b",
                       "Must resolve the option for the SELECTED machine, not the first match")
    }

    func testAggregatedDefaultsNoOptionsLeavesSelectionNil() {
        let model = NewAgentFormModel()
        model.applyDefaults(options: [])
        XCTAssertNil(model.selectedProjectId)
        XCTAssertNil(model.selectedPairingId)
    }
}
