//
//  GitUITests.swift
//  FlightDeckRemoteUITests
//
//  Tier-3 git actions (PRD §5.5/§5.6) against the `-uitest-fixture-snapshot` +
//  `-uitest-fixture-git-status` seams (no live desktop):
//   - the session actions sheet's "Git status" row opens the read-only
//     status screen, rendering branch/base/ahead-behind/drift and the
//     changed-files list from the fixture `Wire.GitStatusDetail`;
//   - git status stays reachable (and its row enabled) even while the link
//     is down — it's a read, not a state change (PRD §8);
//   - pull-base and guarded merge-back show their standard confirmation, are
//     blocked while commands are paused, and — once confirmed — send
//     (observed via the honest in-flight outcome row, PRD §5.8);
//   - merge-back's confirmation surfaces the extra guard note when the
//     fixture status is dirty/drifted.
//

import XCTest

final class GitUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Any of the honest control-outcome rows (in-flight / applied / rejected /
    /// not-delivered). Confirming a send always produces one of these; which
    /// one is nondeterministic in UI tests, where the session row drives the
    /// real (relay-less) `TransportStore` rather than a scripted sender — a
    /// send starts `.sending` (in-flight) and then flips to `.failed` (not
    /// delivered) once the relay-less client can't reach a peer. The
    /// deterministic fact is that confirming sent something.
    private func anyOutcomeRow(_ app: XCUIApplication) -> XCUIElement {
        let predicate = NSPredicate(format: "identifier BEGINSWITH 'control-outcome-'")
        return app.descendants(matching: .any).matching(predicate).firstMatch
    }

    /// Launch unpaired (reset), pair via the DEBUG toggle, seed the fixture
    /// snapshot + git status, and force the link state.
    private func launchPairedWithFixtures(_ app: XCUIApplication,
                                          linkState: String = "connected") {
        app.launchArguments += ["-uitest-reset-pairing", "-uitest-fixture-snapshot",
                                "-uitest-fixture-git-status", "-uitest-linkstate", linkState]
        app.launch()
        let toggle = element(app, "debug-toggle-paired-button")
        XCTAssertTrue(toggle.waitForExistence(timeout: 5), "Expected the DEBUG pairing toggle")
        toggle.tap()
    }

    /// Navigate to the flightdeck project's sessions and open fix-login's
    /// actions sheet from the row ellipsis.
    private func openActionsSheet(_ app: XCUIApplication) {
        element(app, "project-card-proj_flightdeck").tap()
        XCTAssertTrue(element(app, "SessionsListView").waitForExistence(timeout: 5))
        let ellipsis = element(app, "session-actions-sess_fix_login")
        XCTAssertTrue(ellipsis.waitForExistence(timeout: 5))
        ellipsis.tap()
        XCTAssertTrue(element(app, "SessionActionsSheet").waitForExistence(timeout: 5))
    }

    // MARK: - Git status (read, frictionless)

    @MainActor
    func testGitStatusRowOpensReadOnlyStatusScreen() throws {
        let app = XCUIApplication()
        launchPairedWithFixtures(app)
        openActionsSheet(app)

        XCTAssertTrue(element(app, "control-action-git-status").waitForExistence(timeout: 5))
        element(app, "control-action-git-status").tap()

        XCTAssertTrue(element(app, "git-status-view").waitForExistence(timeout: 5))
        XCTAssertEqual(element(app, "git-status-branch").label, "flightdeck/fix-login")
        XCTAssertEqual(element(app, "git-status-base").label, "main")
        XCTAssertEqual(element(app, "git-status-ahead-behind").label, "2 ahead · 1 behind")
        XCTAssertEqual(element(app, "git-status-drift").label, "3 commits behind base")

        // Three changed files in the fixture (modified/added/deleted).
        let fileRows = app.descendants(matching: .any).matching(identifier: "git-file-row")
        XCTAssertEqual(fileRows.count, 3)
    }

    @MainActor
    func testGitStatusRowStaysEnabledAndOpensWhileLinkIsDown() throws {
        let app = XCUIApplication()
        launchPairedWithFixtures(app, linkState: "disconnected")
        openActionsSheet(app)

        XCTAssertTrue(element(app, "control-paused-label").waitForExistence(timeout: 5))
        let gitStatusRow = element(app, "control-action-git-status")
        XCTAssertTrue(gitStatusRow.isEnabled, "Reads stay frictionless even while commands are paused")

        gitStatusRow.tap()
        XCTAssertTrue(element(app, "git-status-view").waitForExistence(timeout: 5))
    }

    // MARK: - Pull base (confirmed)

    @MainActor
    func testPullBaseShowsConfirmationThenSendsOnConfirm() throws {
        let app = XCUIApplication()
        launchPairedWithFixtures(app)
        openActionsSheet(app)

        element(app, "control-action-pull-base").tap()
        XCTAssertTrue(element(app, "ControlConfirmationSheet").waitForExistence(timeout: 5))
        XCTAssertFalse(element(app, "control-confirm-guard-note").exists,
                       "Only merge-back gets the guard note")

        let confirm = element(app, "control-confirm-button")
        XCTAssertEqual(confirm.label, "Pull")
        confirm.tap()

        XCTAssertTrue(anyOutcomeRow(app).waitForExistence(timeout: 8),
                      "Confirming sends the command (an honest outcome row appears, PRD §5.8)")
    }

    @MainActor
    func testPullBaseBlockedWhilePaused() throws {
        let app = XCUIApplication()
        launchPairedWithFixtures(app, linkState: "disconnected")
        openActionsSheet(app)

        XCTAssertFalse(element(app, "control-action-pull-base").isEnabled)
    }

    // MARK: - Guarded merge-back (confirmed)

    @MainActor
    func testMergeBackShowsGuardNoteFromLiveGitStatus() throws {
        let app = XCUIApplication()
        launchPairedWithFixtures(app)
        openActionsSheet(app)

        element(app, "control-action-merge-back").tap()
        XCTAssertTrue(element(app, "ControlConfirmationSheet").waitForExistence(timeout: 5))

        let guardNote = element(app, "control-confirm-guard-note")
        XCTAssertTrue(guardNote.waitForExistence(timeout: 5),
                      "The dirty+drifted fixture status should surface a guard note")
        XCTAssertTrue(guardNote.label.contains("uncommitted change"))
        XCTAssertTrue(guardNote.label.contains("drift"))

        let confirm = element(app, "control-confirm-button")
        XCTAssertEqual(confirm.label, "Merge")
        confirm.tap()

        XCTAssertTrue(anyOutcomeRow(app).waitForExistence(timeout: 8),
                      "Confirming merge-back sends the command (an honest outcome row appears)")
    }

    @MainActor
    func testMergeBackBlockedWhilePaused() throws {
        let app = XCUIApplication()
        launchPairedWithFixtures(app, linkState: "disconnected")
        openActionsSheet(app)

        XCTAssertFalse(element(app, "control-action-merge-back").isEnabled)
    }
}
