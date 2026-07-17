//
//  GitActionsConfirmationGatingTests.swift
//  FlightDeckRemoteTests
//
//  Tier-3 git actions (PRD §5.5/§5.6): pull-base and guarded merge-back are
//  higher-stakes and confirmation-gated, and — like every control command —
//  refuse to send while commands are paused (PRD §8: nothing sent blind).
//  These exercise the exact command-construction + send path
//  (`ControlCommands` → `CommandRunner` → `ControlCommandSending`) that
//  `SessionActionsSheet` uses, plus the merge-back guard note's wiring
//  end-to-end with a live `Wire.GitStatusDetail`. Abandon-worktree's own
//  type-to-confirm gate is `AbandonConfirmLogic` (Features/Control,
//  exercised in `SessionControlActionTests`) — re-verified here only insofar
//  as it also maps through `ControlCommands.abandonWorktree`.
//

import XCTest
@testable import FlightDeckRemote

@MainActor
final class GitActionsConfirmationGatingTests: XCTestCase {

    private let sessionId = Wire.SessionId("sess_fix_login")

    // MARK: - Command construction (the tap → wire-command mapping)

    func testPullBaseConfirmedSendsGitPullBaseCommand() {
        let sender = ScriptedControlCommandSender()
        let runner = CommandRunner(sender: sender)

        XCTAssertTrue(runner.run(ControlCommands.pullBase(sessionId)))
        XCTAssertEqual(sender.sends.count, 1)
        XCTAssertEqual(sender.sends[0].body, .gitPullBase(sessionId: sessionId))
        XCTAssertEqual(runner.phase, .inFlight)
    }

    func testMergeBackConfirmedSendsGitMergeBackCommand() {
        let sender = ScriptedControlCommandSender()
        let runner = CommandRunner(sender: sender)

        XCTAssertTrue(runner.run(ControlCommands.mergeBack(sessionId)))
        XCTAssertEqual(sender.sends.count, 1)
        XCTAssertEqual(sender.sends[0].body, .gitMergeBack(sessionId: sessionId))
        XCTAssertEqual(runner.phase, .inFlight)
    }

    func testAbandonConfirmedOnlyAfterExactTypedMatchThenSendsConfirmName() {
        // The gate: exact match only (no near-miss enables the send).
        XCTAssertFalse(AbandonConfirmLogic.isConfirmed(input: "fix", sessionName: "fix-login"))
        XCTAssertTrue(AbandonConfirmLogic.isConfirmed(input: "fix-login", sessionName: "fix-login"))

        let sender = ScriptedControlCommandSender()
        let runner = CommandRunner(sender: sender)
        XCTAssertTrue(runner.run(ControlCommands.abandonWorktree(sessionId, confirmName: "fix-login")))
        XCTAssertEqual(sender.sends[0].body,
                       .gitAbandonWorktree(sessionId: sessionId, confirmName: "fix-login"))
    }

    // MARK: - Commands-paused gate (PRD §8: nothing sent blind)

    func testPullBaseRefusedWhilePaused() {
        let sender = ScriptedControlCommandSender()
        let runner = CommandRunner(sender: sender, isPaused: { true })

        XCTAssertFalse(runner.run(ControlCommands.pullBase(sessionId)))
        XCTAssertTrue(sender.sends.isEmpty, "Nothing goes out while the link is down")
        XCTAssertEqual(runner.phase, .idle)
    }

    func testMergeBackRefusedWhilePaused() {
        let sender = ScriptedControlCommandSender()
        let runner = CommandRunner(sender: sender, isPaused: { true })

        XCTAssertFalse(runner.run(ControlCommands.mergeBack(sessionId)))
        XCTAssertTrue(sender.sends.isEmpty)
        XCTAssertEqual(runner.phase, .idle)
    }

    func testPullAndMergeSendOnceLinkIsBackUp() {
        let sender = ScriptedControlCommandSender()
        var paused = true
        let runner = CommandRunner(sender: sender, isPaused: { paused })

        XCTAssertFalse(runner.run(ControlCommands.pullBase(sessionId)))
        paused = false
        XCTAssertTrue(runner.run(ControlCommands.pullBase(sessionId)))
        XCTAssertEqual(sender.sends.count, 1)
    }

    // MARK: - Standard confirmation copy exists for both (never sent unconfirmed)

    func testPullBaseAndMergeBackHaveNonDestructiveStandardConfirmations() {
        for action in [SessionControlAction.pullBase, .mergeBack] {
            let conf = action.confirmation(sessionName: "fix-login")
            XCTAssertNotNil(conf)
            XCTAssertFalse(conf!.isDestructive, "Pull/merge are higher-stakes but not destructive")
            XCTAssertFalse(conf!.message.isEmpty)
        }
    }

    // MARK: - Guarded merge-back: guard note wiring (SessionActionsSheet's exact computation)

    func testMergeBackGuardNoteIsNilWithNoKnownGitStatus() {
        XCTAssertNil(GitMergeGuardText.build(from: nil))
    }

    func testMergeBackGuardNoteSurfacesDirtyAndDriftFromLiveStatus() {
        let detail = Wire.GitStatusDetail(
            sessionId: sessionId, branch: "flightdeck/fix-login", baseBranch: "main",
            hasUpstream: true, ahead: 0, behind: 0, drift: 2,
            files: [Wire.GitFileChange(path: "a.swift", status: .modified, addedLines: 1, removedLines: 0)])
        let note = GitMergeGuardText.build(from: detail)
        XCTAssertEqual(note, "Heads up: 1 uncommitted change and 2 commits of drift from base. The merge may conflict.")
    }

    func testPullBaseNeverGetsAGuardNote() {
        // `SessionActionsSheet` only computes the guard note for `.mergeBack`;
        // pull-base's confirmation is the standard copy alone.
        let dirtyDetail = Wire.GitStatusDetail(
            sessionId: sessionId, branch: "b", baseBranch: "main", hasUpstream: true,
            ahead: 0, behind: 0, drift: 5,
            files: [Wire.GitFileChange(path: "a.swift", status: .modified, addedLines: 1, removedLines: 0)])
        // Simulate the sheet's own gating logic directly (mirrors the ternary
        // in `SessionActionsSheet`'s `.confirm` case).
        func guardNote(for action: SessionControlAction, detail: Wire.GitStatusDetail?) -> String? {
            action == .mergeBack ? GitMergeGuardText.build(from: detail) : nil
        }
        XCTAssertNil(guardNote(for: .pullBase, detail: dirtyDetail))
        XCTAssertNotNil(guardNote(for: .mergeBack, detail: dirtyDetail))
    }
}
