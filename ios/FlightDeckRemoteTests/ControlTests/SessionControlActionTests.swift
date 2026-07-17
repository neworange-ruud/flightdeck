//
//  SessionControlActionTests.swift
//  FlightDeckRemoteTests
//
//  The action → wire-command mapping (all seven control commands), the
//  safe/destructive grouping (PRD §5.6: never mixed), the confirmation copy,
//  and the abandon type-to-confirm gate (exact match only).
//

import XCTest
@testable import FlightDeckRemote

final class SessionControlActionTests: XCTestCase {

    private let sessionId = Wire.SessionId("sess_fix_login")

    // MARK: - Action → CommandBody mapping (all 7)

    func testRestartAgentMapsToRestartAgentCommand() {
        XCTAssertEqual(ControlCommands.restartAgent(sessionId),
                       .restartAgent(sessionId: sessionId))
    }

    func testCloseSessionMapsToCloseSessionCommand() {
        XCTAssertEqual(ControlCommands.closeSession(sessionId),
                       .closeSession(sessionId: sessionId))
    }

    func testSetManualStatusMapsWithLabel() {
        XCTAssertEqual(ControlCommands.setManualStatus(sessionId, label: "reviewing"),
                       .setManualStatus(sessionId: sessionId, label: "reviewing"))
    }

    func testClearManualStatusMaps() {
        XCTAssertEqual(ControlCommands.clearManualStatus(sessionId),
                       .clearManualStatus(sessionId: sessionId))
    }

    func testPullBaseMapsToGitPullBase() {
        XCTAssertEqual(ControlCommands.pullBase(sessionId),
                       .gitPullBase(sessionId: sessionId))
    }

    func testMergeBackMapsToGitMergeBack() {
        XCTAssertEqual(ControlCommands.mergeBack(sessionId),
                       .gitMergeBack(sessionId: sessionId))
    }

    func testAbandonWorktreeMapsWithTypedConfirmName() {
        XCTAssertEqual(ControlCommands.abandonWorktree(sessionId, confirmName: "fix-login"),
                       .gitAbandonWorktree(sessionId: sessionId, confirmName: "fix-login"))
    }

    // MARK: - Grouping (PRD §5.6: safe on top, destructive apart — never mixed)

    func testSafeGroupOrderAndMembership() {
        XCTAssertEqual(SessionControlAction.safeGroup,
                       [.restartAgent, .openShell, .setManualStatus, .pullBase, .mergeBack])
        XCTAssertTrue(SessionControlAction.safeGroup.allSatisfy { !$0.isDestructive })
    }

    func testDestructiveGroupIsApartAndAllDestructive() {
        XCTAssertEqual(SessionControlAction.destructiveGroup, [.closeSession, .abandonWorktree])
        XCTAssertTrue(SessionControlAction.destructiveGroup.allSatisfy(\.isDestructive))
        XCTAssertTrue(Set(SessionControlAction.safeGroup)
            .isDisjoint(with: SessionControlAction.destructiveGroup))
    }

    // MARK: - Confirmation copy

    func testStandardDialogActionsHaveConfirmations() {
        for action in [SessionControlAction.restartAgent, .pullBase, .mergeBack, .closeSession] {
            let conf = action.confirmation(sessionName: "fix-login")
            XCTAssertNotNil(conf, "\(action) should use a standard confirmation dialog")
            XCTAssertFalse(conf!.title.isEmpty)
            XCTAssertFalse(conf!.message.isEmpty)
        }
    }

    func testOwnFlowActionsHaveNoStandardConfirmation() {
        // Manual status uses its sub-sheet; abandon uses type-to-confirm;
        // open shell mounts the terminal directly (no confirmation).
        XCTAssertNil(SessionControlAction.setManualStatus.confirmation(sessionName: "x"))
        XCTAssertNil(SessionControlAction.abandonWorktree.confirmation(sessionName: "x"))
        XCTAssertNil(SessionControlAction.openShell.confirmation(sessionName: "x"))
    }

    func testOnlyCloseSessionDialogIsDestructive() {
        XCTAssertTrue(SessionControlAction.closeSession.confirmation(sessionName: "x")!.isDestructive)
        XCTAssertFalse(SessionControlAction.restartAgent.confirmation(sessionName: "x")!.isDestructive)
        XCTAssertFalse(SessionControlAction.pullBase.confirmation(sessionName: "x")!.isDestructive)
        XCTAssertFalse(SessionControlAction.mergeBack.confirmation(sessionName: "x")!.isDestructive)
    }

    // MARK: - Abandon type-to-confirm gate (exact match only)

    func testAbandonConfirmedOnlyOnExactMatch() {
        XCTAssertTrue(AbandonConfirmLogic.isConfirmed(input: "fix-login", sessionName: "fix-login"))
    }

    func testAbandonRejectsNearMisses() {
        XCTAssertFalse(AbandonConfirmLogic.isConfirmed(input: "", sessionName: "fix-login"))
        XCTAssertFalse(AbandonConfirmLogic.isConfirmed(input: "fix", sessionName: "fix-login"))
        XCTAssertFalse(AbandonConfirmLogic.isConfirmed(input: "Fix-Login", sessionName: "fix-login"))
        XCTAssertFalse(AbandonConfirmLogic.isConfirmed(input: " fix-login", sessionName: "fix-login"))
        XCTAssertFalse(AbandonConfirmLogic.isConfirmed(input: "fix-login ", sessionName: "fix-login"))
        XCTAssertFalse(AbandonConfirmLogic.isConfirmed(input: "fix-login-2", sessionName: "fix-login"))
    }

    func testAbandonNeverConfirmedForEmptySessionName() {
        XCTAssertFalse(AbandonConfirmLogic.isConfirmed(input: "", sessionName: ""))
    }

    // MARK: - Honest in-flight phrasing

    func testInFlightLabels() {
        let s = sessionId
        XCTAssertEqual(ControlActionPhrasing.inFlightLabel(for: .restartAgent(sessionId: s)), "Restarting agent…")
        XCTAssertEqual(ControlActionPhrasing.inFlightLabel(for: .closeSession(sessionId: s)), "Closing session…")
        XCTAssertEqual(ControlActionPhrasing.inFlightLabel(for: .setManualStatus(sessionId: s, label: "x")), "Setting status…")
        XCTAssertEqual(ControlActionPhrasing.inFlightLabel(for: .clearManualStatus(sessionId: s)), "Clearing status…")
        XCTAssertEqual(ControlActionPhrasing.inFlightLabel(for: .gitPullBase(sessionId: s)), "Pulling base…")
        XCTAssertEqual(ControlActionPhrasing.inFlightLabel(for: .gitMergeBack(sessionId: s)), "Merging back…")
        XCTAssertEqual(ControlActionPhrasing.inFlightLabel(for: .gitAbandonWorktree(sessionId: s, confirmName: "x")), "Abandoning worktree…")
        XCTAssertEqual(ControlActionPhrasing.inFlightLabel(for: nil), "Sending…")
    }
}
