//
//  GitMergeGuardTextTests.swift
//  FlightDeckRemoteTests
//
//  The "guarded" merge-back's extra warning line (PRD §5.5): built purely
//  from the session's latest known `Wire.GitStatusDetail` — nil status, a
//  clean/no-drift status, dirty-only, drift-only, and both together.
//

import XCTest
@testable import FlightDeckRemote

final class GitMergeGuardTextTests: XCTestCase {

    private let sessionId = Wire.SessionId("sess_fix_login")

    private func detail(drift: UInt32, files: [Wire.GitFileChange]) -> Wire.GitStatusDetail {
        Wire.GitStatusDetail(sessionId: sessionId, branch: "flightdeck/fix-login", baseBranch: "main",
                             hasUpstream: true, ahead: 0, behind: 0, drift: drift, files: files)
    }

    func testNoStatusYieldsNoGuardNote() {
        XCTAssertNil(GitMergeGuardText.build(from: nil))
    }

    func testCleanNoDriftYieldsNoGuardNote() {
        XCTAssertNil(GitMergeGuardText.build(from: detail(drift: 0, files: [])))
    }

    func testDirtyOnlyMentionsUncommittedChangeCountSingular() {
        let note = GitMergeGuardText.build(from: detail(drift: 0, files: [
            Wire.GitFileChange(path: "a.swift", status: .modified, addedLines: 1, removedLines: 0),
        ]))
        XCTAssertEqual(note, "Heads up: 1 uncommitted change. The merge may conflict.")
    }

    func testDirtyOnlyMentionsUncommittedChangeCountPlural() {
        let note = GitMergeGuardText.build(from: detail(drift: 0, files: [
            Wire.GitFileChange(path: "a.swift", status: .modified, addedLines: 1, removedLines: 0),
            Wire.GitFileChange(path: "b.swift", status: .added, addedLines: 5, removedLines: 0),
        ]))
        XCTAssertEqual(note, "Heads up: 2 uncommitted changes. The merge may conflict.")
    }

    func testDriftOnlyMentionsDriftCountSingular() {
        let note = GitMergeGuardText.build(from: detail(drift: 1, files: []))
        XCTAssertEqual(note, "Heads up: 1 commit of drift from base. The merge may conflict.")
    }

    func testDriftOnlyMentionsDriftCountPlural() {
        let note = GitMergeGuardText.build(from: detail(drift: 3, files: []))
        XCTAssertEqual(note, "Heads up: 3 commits of drift from base. The merge may conflict.")
    }

    func testDirtyAndDriftJoinsBothClauses() {
        let note = GitMergeGuardText.build(from: detail(drift: 2, files: [
            Wire.GitFileChange(path: "a.swift", status: .modified, addedLines: 1, removedLines: 0),
        ]))
        XCTAssertEqual(note, "Heads up: 1 uncommitted change and 2 commits of drift from base. The merge may conflict.")
    }
}
