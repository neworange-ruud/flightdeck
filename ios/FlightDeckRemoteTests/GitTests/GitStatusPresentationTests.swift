//
//  GitStatusPresentationTests.swift
//  FlightDeckRemoteTests
//
//  The read-only git status screen's pure wire → display mapping (PRD §5.5):
//  ahead/behind formatting, drift, the changed-files list, and the
//  empty/clean states.
//

import XCTest
@testable import FlightDeckRemote

final class GitStatusPresentationTests: XCTestCase {

    private let sessionId = Wire.SessionId("sess_fix_login")

    private func detail(
        branch: String? = "flightdeck/fix-login",
        baseBranch: String? = "main",
        hasUpstream: Bool = true,
        ahead: UInt32 = 0,
        behind: UInt32 = 0,
        drift: UInt32 = 0,
        files: [Wire.GitFileChange] = []
    ) -> Wire.GitStatusDetail {
        Wire.GitStatusDetail(sessionId: sessionId, branch: branch, baseBranch: baseBranch,
                             hasUpstream: hasUpstream, ahead: ahead, behind: behind,
                             drift: drift, files: files)
    }

    // MARK: - Branch / base

    func testBranchAndBaseBranchPassThrough() {
        let rows = GitStatusPresentation.present(detail(branch: "flightdeck/fix-login", baseBranch: "main"))
        XCTAssertEqual(rows.branch, "flightdeck/fix-login")
        XCTAssertEqual(rows.baseBranch, "main")
    }

    func testDetachedBranchAndMissingBaseFallBackToPlaceholders() {
        let rows = GitStatusPresentation.present(detail(branch: nil, baseBranch: nil))
        XCTAssertEqual(rows.branch, "(detached HEAD)")
        XCTAssertEqual(rows.baseBranch, "—")
    }

    // MARK: - Ahead / behind

    func testUpToDateWhenNeitherAheadNorBehind() {
        XCTAssertEqual(GitStatusPresentation.aheadBehindText(ahead: 0, behind: 0, hasUpstream: true),
                       "up to date")
    }

    func testNoUpstreamWinsRegardlessOfCounts() {
        XCTAssertEqual(GitStatusPresentation.aheadBehindText(ahead: 3, behind: 2, hasUpstream: false),
                       "no upstream")
    }

    func testAheadOnly() {
        XCTAssertEqual(GitStatusPresentation.aheadBehindText(ahead: 3, behind: 0, hasUpstream: true),
                       "3 ahead")
    }

    func testBehindOnly() {
        XCTAssertEqual(GitStatusPresentation.aheadBehindText(ahead: 0, behind: 2, hasUpstream: true),
                       "2 behind")
    }

    func testAheadAndBehindJoined() {
        XCTAssertEqual(GitStatusPresentation.aheadBehindText(ahead: 3, behind: 2, hasUpstream: true),
                       "3 ahead · 2 behind")
    }

    // MARK: - Drift

    func testNoDriftYieldsNilDriftText() {
        XCTAssertNil(GitStatusPresentation.driftText(0))
    }

    func testSingularDriftText() {
        XCTAssertEqual(GitStatusPresentation.driftText(1), "1 commit behind base")
    }

    func testPluralDriftText() {
        XCTAssertEqual(GitStatusPresentation.driftText(3), "3 commits behind base")
    }

    // MARK: - Empty / clean state

    func testNoFilesIsClean() {
        let rows = GitStatusPresentation.present(detail(files: []))
        XCTAssertTrue(rows.isClean)
        XCTAssertTrue(rows.files.isEmpty)
    }

    func testAnyFileIsNotClean() {
        let rows = GitStatusPresentation.present(detail(files: [
            Wire.GitFileChange(path: "a.swift", status: .modified, addedLines: 1, removedLines: 0),
        ]))
        XCTAssertFalse(rows.isClean)
    }

    // MARK: - Changed files list

    func testFilesMapAllFieldsInOrder() {
        let changes: [Wire.GitFileChange] = [
            Wire.GitFileChange(path: "Sources/Login.swift", status: .modified, addedLines: 12, removedLines: 4),
            Wire.GitFileChange(path: "Sources/New.swift", status: .added, addedLines: 40, removedLines: 0),
            Wire.GitFileChange(path: "Sources/Old.swift", status: .deleted, addedLines: 0, removedLines: 30),
            Wire.GitFileChange(path: "Sources/Renamed.swift", status: .renamed, addedLines: 0, removedLines: 0),
            Wire.GitFileChange(path: "Sources/New2.swift", status: .untracked, addedLines: 5, removedLines: 0),
        ]
        let rows = GitStatusPresentation.present(detail(files: changes))
        XCTAssertEqual(rows.files.map(\.path), changes.map(\.path))
        XCTAssertEqual(rows.files.map(\.status), changes.map(\.status))
        XCTAssertEqual(rows.files.map(\.addedLines), changes.map(\.addedLines))
        XCTAssertEqual(rows.files.map(\.removedLines), changes.map(\.removedLines))
    }

    func testShortLabelsForEveryFileStatus() {
        XCTAssertEqual(GitStatusPresentation.shortLabel(for: .added), "A")
        XCTAssertEqual(GitStatusPresentation.shortLabel(for: .modified), "M")
        XCTAssertEqual(GitStatusPresentation.shortLabel(for: .deleted), "D")
        XCTAssertEqual(GitStatusPresentation.shortLabel(for: .renamed), "R")
        XCTAssertEqual(GitStatusPresentation.shortLabel(for: .untracked), "U")
    }

    // MARK: - Full detail, end to end

    func testFullDetailWithDriftAndFiles() {
        let rows = GitStatusPresentation.present(detail(
            branch: "flightdeck/fix-login", baseBranch: "main", hasUpstream: true,
            ahead: 2, behind: 1, drift: 3,
            files: [Wire.GitFileChange(path: "a.swift", status: .modified, addedLines: 1, removedLines: 1)]))

        XCTAssertEqual(rows.branch, "flightdeck/fix-login")
        XCTAssertEqual(rows.baseBranch, "main")
        XCTAssertEqual(rows.aheadBehindText, "2 ahead · 1 behind")
        XCTAssertEqual(rows.driftText, "3 commits behind base")
        XCTAssertFalse(rows.isClean)
        XCTAssertEqual(rows.files.count, 1)
    }
}
