//
//  CommandRunnerTests.swift
//  FlightDeckRemoteTests
//
//  The shared control-command helper: phase transitions (spinner → applied /
//  rejected-verbatim / not-delivered-retry), the §5.8 retry command-id
//  semantics, and the paused gate (nothing sent blind).
//

import XCTest
@testable import FlightDeckRemote

@MainActor
final class CommandRunnerTests: XCTestCase {

    private let sessionId = Wire.SessionId("sess_fix_login")

    /// Spin the main actor until `condition` holds (the runner's observation
    /// loop re-enters via `Task { @MainActor in … }`).
    private func waitUntil(_ condition: @autoclosure () -> Bool) async {
        for _ in 0..<200 {
            if condition() { return }
            await Task.yield()
        }
    }

    // MARK: - Pure phase mapping

    func testPhaseMappingFromDeliveryStates() {
        XCTAssertEqual(ControlActionPhase.from(delivery: .sending, ackMessage: nil), .inFlight)
        XCTAssertEqual(ControlActionPhase.from(delivery: .delivered(.applied), ackMessage: nil),
                       .succeeded(detail: nil))
        // `accepted` is a success: for new_agent it means creation started.
        XCTAssertEqual(ControlActionPhase.from(delivery: .delivered(.accepted),
                                               ackMessage: "Stopping agent… tap close again once it's idle"),
                       .succeeded(detail: "Stopping agent… tap close again once it's idle"))
        XCTAssertEqual(ControlActionPhase.from(delivery: .delivered(.duplicate), ackMessage: nil),
                       .succeeded(detail: nil))
        // Rejection carries the desktop's exact reason, verbatim.
        XCTAssertEqual(ControlActionPhase.from(delivery: .delivered(.rejected),
                                               ackMessage: "confirm name did not match"),
                       .rejected(reason: "confirm name did not match"))
        XCTAssertEqual(ControlActionPhase.from(delivery: .delivered(.rejected), ackMessage: nil),
                       .rejected(reason: "rejected by desktop"))
        // Desktop attempted-and-failed: observed negative → retry mints a new id.
        XCTAssertEqual(ControlActionPhase.from(delivery: .delivered(.failed),
                                               ackMessage: "merge conflict in src/lib.rs"),
                       .failed(reason: "merge conflict in src/lib.rs", retryReusesId: false))
        // Transport-level failure: never saw an ack → retry reuses the id.
        XCTAssertEqual(ControlActionPhase.from(delivery: .failed(reason: "timed out"), ackMessage: nil),
                       .failed(reason: "timed out", retryReusesId: true))
    }

    // MARK: - Run lifecycle

    func testRunSendsAndTracksThroughApplied() async {
        let sender = ScriptedControlCommandSender()
        let runner = CommandRunner(sender: sender)

        XCTAssertTrue(runner.run(ControlCommands.restartAgent(sessionId)))
        XCTAssertEqual(runner.phase, .inFlight)
        XCTAssertEqual(sender.sends.count, 1)
        XCTAssertEqual(sender.sends[0].body, .restartAgent(sessionId: sessionId))

        sender.resolve(sender.sends[0].commandId, with: .delivered(.applied))
        await waitUntil(runner.phase == .succeeded(detail: nil))
        XCTAssertEqual(runner.phase, .succeeded(detail: nil))
    }

    func testRejectedShowsVerbatimAckReason() async {
        let sender = ScriptedControlCommandSender()
        let runner = CommandRunner(sender: sender)

        runner.run(ControlCommands.mergeBack(sessionId))
        sender.resolve(sender.sends[0].commandId, with: .delivered(.rejected),
                       ackMessage: "worktree has uncommitted changes")
        await waitUntil(runner.phase == .rejected(reason: "worktree has uncommitted changes"))
        XCTAssertEqual(runner.phase, .rejected(reason: "worktree has uncommitted changes"))
    }

    func testSecondRunRefusedWhileInFlight() {
        let sender = ScriptedControlCommandSender()
        let runner = CommandRunner(sender: sender)

        XCTAssertTrue(runner.run(ControlCommands.pullBase(sessionId)))
        XCTAssertFalse(runner.run(ControlCommands.mergeBack(sessionId)),
                       "One action in flight at a time")
        XCTAssertEqual(sender.sends.count, 1)
    }

    func testResetClearsOutcome() async {
        let sender = ScriptedControlCommandSender()
        let runner = CommandRunner(sender: sender)
        runner.run(ControlCommands.pullBase(sessionId))
        sender.resolve(sender.sends[0].commandId, with: .delivered(.applied))
        await waitUntil(runner.phase == .succeeded(detail: nil))

        runner.reset()
        XCTAssertEqual(runner.phase, .idle)
        XCTAssertNil(runner.currentBody)
    }

    // MARK: - Retry id semantics (§5.8)

    func testTransportFailureRetryReusesOriginalCommandId() async {
        let sender = ScriptedControlCommandSender()
        let runner = CommandRunner(sender: sender)

        runner.run(ControlCommands.closeSession(sessionId))
        let originalId = sender.sends[0].commandId
        sender.resolve(originalId, with: .failed(reason: "timed out"))
        await waitUntil(runner.phase == .failed(reason: "timed out", retryReusesId: true))

        XCTAssertTrue(runner.retry())
        XCTAssertEqual(sender.sends.count, 2)
        XCTAssertEqual(sender.sends[1].commandId, originalId,
                       "Never-acked failure retries dedup-safely under the same id")
        XCTAssertEqual(sender.sends[1].body, .closeSession(sessionId: sessionId))
        XCTAssertEqual(runner.phase, .inFlight)
    }

    func testDesktopFailedOutcomeRetryMintsNewCommandId() async {
        let sender = ScriptedControlCommandSender()
        let runner = CommandRunner(sender: sender)

        runner.run(ControlCommands.pullBase(sessionId))
        let originalId = sender.sends[0].commandId
        sender.resolve(originalId, with: .delivered(.failed), ackMessage: "merge conflict")
        await waitUntil(runner.phase == .failed(reason: "merge conflict", retryReusesId: false))

        XCTAssertTrue(runner.retry())
        XCTAssertEqual(sender.sends.count, 2)
        XCTAssertNotEqual(sender.sends[1].commandId, originalId,
                          "An observed desktop negative retries as a fresh attempt")
    }

    func testRetryRefusedUnlessFailed() {
        let sender = ScriptedControlCommandSender()
        let runner = CommandRunner(sender: sender)
        XCTAssertFalse(runner.retry(), "Nothing to retry while idle")
        runner.run(ControlCommands.restartAgent(sessionId))
        XCTAssertFalse(runner.retry(), "Nothing to retry while in flight")
    }

    // MARK: - Paused gating (PRD §8: nothing sent blind)

    func testRunRefusedWhilePaused() {
        let sender = ScriptedControlCommandSender()
        var paused = true
        let runner = CommandRunner(sender: sender, isPaused: { paused })

        XCTAssertFalse(runner.run(ControlCommands.restartAgent(sessionId)))
        XCTAssertEqual(runner.phase, .idle)
        XCTAssertTrue(sender.sends.isEmpty, "Nothing goes out while the link is down")

        paused = false
        XCTAssertTrue(runner.run(ControlCommands.restartAgent(sessionId)))
        XCTAssertEqual(sender.sends.count, 1)
    }

    func testRetryRefusedWhilePaused() async {
        let sender = ScriptedControlCommandSender()
        var paused = false
        let runner = CommandRunner(sender: sender, isPaused: { paused })

        runner.run(ControlCommands.closeSession(sessionId))
        sender.resolve(sender.sends[0].commandId, with: .failed(reason: "link down"))
        await waitUntil(runner.phase == .failed(reason: "link down", retryReusesId: true))

        paused = true
        XCTAssertFalse(runner.retry())
        XCTAssertEqual(sender.sends.count, 1)
    }

    // MARK: - Late-arriving verbatim ack message

    func testAckMessageArrivingAfterDeliveryUpdatesReason() async {
        // The client emits the delivery outcome before the store folds the
        // ack's message onto the handle — the runner must pick up the
        // verbatim reason when it lands a beat later.
        let sender = ScriptedControlCommandSender()
        let runner = CommandRunner(sender: sender)

        runner.run(ControlCommands.mergeBack(sessionId))
        let id = sender.sends[0].commandId
        sender.resolve(id, with: .delivered(.rejected)) // no message yet
        await waitUntil(runner.phase == .rejected(reason: "rejected by desktop"))

        sender.handles[0].ackMessage = "base branch moved; pull base first"
        await waitUntil(runner.phase == .rejected(reason: "base branch moved; pull base first"))
        XCTAssertEqual(runner.phase, .rejected(reason: "base branch moved; pull base first"))
    }
}
