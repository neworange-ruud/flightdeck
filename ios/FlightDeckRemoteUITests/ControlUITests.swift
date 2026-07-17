//
//  ControlUITests.swift
//  FlightDeckRemoteUITests
//
//  Tier-2 light control (PRD §5.5/§5.6) against the `-uitest-fixture-snapshot`
//  fixture + `-uitest-linkstate connected` (no live desktop):
//   - a session row's ellipsis opens the actions sheet with the safe group on
//     top and the destructive group apart;
//   - Restart agent shows its standard confirmation dialog;
//   - the abandon flow requires typing the exact session name before the
//     destructive button enables;
//   - the FAB presents the real NewAgentView with all §5.5 fields, and the
//     branch preview live-updates with the desktop's slug rules;
//   - a down link disables the actions (commands paused, PRD §8).
//

import XCTest

final class ControlUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Launch unpaired (reset), pair via the DEBUG toggle, seed the fixture
    /// snapshot, and force the link state.
    private func launchPairedWithFixture(_ app: XCUIApplication,
                                         linkState: String = "connected") {
        app.launchArguments += ["-uitest-reset-pairing", "-uitest-fixture-snapshot",
                                "-uitest-linkstate", linkState]
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

    @MainActor
    func testSessionRowOpensActionsSheetWithSafeAndDestructiveGroups() throws {
        let app = XCUIApplication()
        launchPairedWithFixture(app)
        openActionsSheet(app)

        // Safe group on top.
        XCTAssertTrue(element(app, "control-action-restart").waitForExistence(timeout: 5))
        XCTAssertTrue(element(app, "control-action-shell").exists)
        XCTAssertTrue(element(app, "control-action-manual-status").exists)
        XCTAssertTrue(element(app, "control-action-pull-base").exists)
        XCTAssertTrue(element(app, "control-action-merge-back").exists)
        // Destructive group apart.
        XCTAssertTrue(element(app, "control-action-close").exists)
        XCTAssertTrue(element(app, "control-action-abandon").exists)
        // Open shell is now live (the terminal surface exists, PRD §5.4).
        XCTAssertTrue(element(app, "control-action-shell").isEnabled)
    }

    @MainActor
    func testOpenShellPresentsTheSessionShellSurface() throws {
        let app = XCUIApplication()
        launchPairedWithFixture(app)
        openActionsSheet(app)

        element(app, "control-action-shell").tap()
        XCTAssertTrue(element(app, "SessionShellSheet").waitForExistence(timeout: 5),
                      "Open shell mounts the session's terminal surface")
        XCTAssertTrue(element(app, "ShellView").waitForExistence(timeout: 5))
    }

    @MainActor
    func testRestartShowsStandardConfirmationDialog() throws {
        let app = XCUIApplication()
        launchPairedWithFixture(app)
        openActionsSheet(app)

        element(app, "control-action-restart").tap()
        // The standard confirmation: title + consequence + confirm/cancel.
        XCTAssertTrue(element(app, "ControlConfirmationSheet").waitForExistence(timeout: 5),
                      "Expected the restart confirmation sheet")
        let confirm = element(app, "control-confirm-button")
        XCTAssertTrue(confirm.waitForExistence(timeout: 5),
                      "Expected the restart confirmation's confirm button")
        XCTAssertEqual(confirm.label, "Restart")
        XCTAssertTrue(element(app, "control-cancel-button").exists)
        element(app, "control-cancel-button").tap()
    }

    @MainActor
    func testAbandonRequiresTypingTheExactSessionName() throws {
        let app = XCUIApplication()
        launchPairedWithFixture(app)
        openActionsSheet(app)

        element(app, "control-action-abandon").tap()
        XCTAssertTrue(element(app, "AbandonConfirmView").waitForExistence(timeout: 5))

        let confirmButton = element(app, "abandon-confirm-button")
        XCTAssertFalse(confirmButton.isEnabled, "Disabled until the name is typed")

        let field = element(app, "abandon-confirm-field")
        field.tap()
        field.typeText("fix")
        XCTAssertFalse(confirmButton.isEnabled, "A partial name must not enable abandon")

        field.typeText("-login")
        XCTAssertTrue(confirmButton.waitForExistence(timeout: 2))
        XCTAssertTrue(confirmButton.isEnabled, "The exact session name enables abandon")

        element(app, "abandon-keep-button").tap()
        XCTAssertTrue(element(app, "SessionActionsSheet").waitForExistence(timeout: 5))
    }

    @MainActor
    func testFABPresentsNewAgentViewAndSlugPreviewUpdates() throws {
        let app = XCUIApplication()
        launchPairedWithFixture(app)

        element(app, "tab-fab-new-agent").tap()
        XCTAssertTrue(element(app, "NewAgentView").waitForExistence(timeout: 5))

        // All §5.5 fields on the one screen.
        XCTAssertTrue(element(app, "new-agent-type-claude_code").exists)
        XCTAssertTrue(element(app, "new-agent-type-opencode").exists)
        XCTAssertTrue(element(app, "new-agent-type-codex").exists)
        XCTAssertTrue(element(app, "new-agent-name-field").exists)
        XCTAssertTrue(element(app, "new-agent-slug-preview").exists)
        XCTAssertTrue(element(app, "new-agent-base-field").exists)
        XCTAssertTrue(element(app, "new-agent-task-field").exists)
        XCTAssertTrue(element(app, "new-agent-launch").exists)

        // Live slug preview mirrors the desktop's rules.
        let nameField = element(app, "new-agent-name-field")
        nameField.tap()
        nameField.typeText("Add rate limit")
        let preview = element(app, "new-agent-slug-preview")
        XCTAssertTrue(preview.waitForExistence(timeout: 2))
        XCTAssertEqual(preview.label, "flightdeck/add-rate-limit")

        // Launch stays disabled until the first task is filled in too.
        XCTAssertFalse(element(app, "new-agent-launch").isEnabled)
    }

    @MainActor
    func testDisconnectedLinkDisablesActions() throws {
        let app = XCUIApplication()
        launchPairedWithFixture(app, linkState: "disconnected")
        openActionsSheet(app)

        XCTAssertTrue(element(app, "control-paused-label").waitForExistence(timeout: 5),
                      "Expected the honest commands-paused note")
        XCTAssertFalse(element(app, "control-action-restart").isEnabled)
        XCTAssertFalse(element(app, "control-action-pull-base").isEnabled)
        XCTAssertFalse(element(app, "control-action-close").isEnabled)
        XCTAssertFalse(element(app, "control-action-abandon").isEnabled)
    }
}
