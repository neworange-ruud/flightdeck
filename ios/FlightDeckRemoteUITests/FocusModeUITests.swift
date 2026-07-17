//
//  FocusModeUITests.swift
//  FlightDeckRemoteUITests
//
//  Exercises the eyes-free focus mode (PRD §5.3 3b) via the chat fixture seam:
//  entering focus mode pins the pending permission ask large, and the big
//  Approve / Deny buttons route through the same inline resolution path as the
//  transcript card (a spinner appears while the scripted decision is sending).
//

import XCTest

final class FocusModeUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    private func launchChat(linkState: String) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-uitest-reset-pairing", "-uitest-fixture-transcript",
                                "-uitest-linkstate", linkState]
        app.launch()
        let toggle = element(app, "debug-toggle-paired-button")
        XCTAssertTrue(toggle.waitForExistence(timeout: 5))
        toggle.tap()
        XCTAssertTrue(element(app, "AgentChatView").waitForExistence(timeout: 5))
        return app
    }

    private func enterFocusMode(_ app: XCUIApplication) {
        let enter = element(app, "focus-enter")
        XCTAssertTrue(enter.waitForExistence(timeout: 5),
                      "Focus-mode entry should be offered while a prompt is pending")
        enter.tap()
        XCTAssertTrue(element(app, "focus-mode").waitForExistence(timeout: 5),
                      "Expected the focus-mode screen")
    }

    @MainActor
    func testEnterFocusModePinsPendingCommand() throws {
        let app = launchChat(linkState: "connected:20")
        enterFocusMode(app)
        XCTAssertTrue(element(app, "focus-pending-command").waitForExistence(timeout: 3),
                      "Expected the pending command pinned large")
        XCTAssertTrue(element(app, "focus-approve").exists)
        XCTAssertTrue(element(app, "focus-deny").exists)
    }

    @MainActor
    func testFocusApproveShowsSendingSpinner() throws {
        let app = launchChat(linkState: "connected:20")
        enterFocusMode(app)

        let approve = element(app, "focus-approve")
        XCTAssertTrue(approve.waitForExistence(timeout: 3))
        XCTAssertTrue(approve.isEnabled, "Approve should be live on the current prompt while connected")
        approve.tap()
        XCTAssertTrue(element(app, "working-spinner").waitForExistence(timeout: 3),
                      "Approving should show a spinner while the decision is sending")
    }

    @MainActor
    func testFocusDenyIsLiveWhenConnected() throws {
        let app = launchChat(linkState: "connected:20")
        enterFocusMode(app)

        let deny = element(app, "focus-deny")
        XCTAssertTrue(deny.waitForExistence(timeout: 3))
        XCTAssertTrue(deny.isEnabled)
        deny.tap()
        XCTAssertTrue(element(app, "working-spinner").waitForExistence(timeout: 3),
                      "Denying should show a spinner while the decision is sending")
    }

    // NOTE: the disconnected/paused disabled state for the big Approve/Deny
    // buttons is covered by the pure `ChatViewModel.isPermissionActionable`
    // unit tests (paused ⇒ not actionable) — the SAME gate the transcript
    // card uses (see ChatComposeUITests.testDisconnectedPermissionButtonsDisabled).
    // A disconnected focus-mode UI test isn't added here because the header's
    // focus-mode entry button sits under the top-anchored ReconnectingBanner
    // while the link is down (the same overlay reason the tab bar is hidden in
    // chat), so the entry tap is swallowed — an app-shell layout trait, not a
    // focus-mode behaviour worth a brittle UI test.
}
