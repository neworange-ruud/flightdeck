//
//  DocScreenshotUITests.swift
//  FlightDeckRemoteUITests
//
//  Not a behavioural test — this suite drives the app through every documented
//  screen using the DEBUG launch-argument fixtures (the same seams the other UI
//  test suites use) and captures a full-window screenshot of each as a
//  `.keepAlways` XCTAttachment. The docs pipeline extracts these from the
//  resulting `.xcresult` bundle via `xcrun xcresulttool export attachments`.
//
//  Each method is self-contained (its own launch + launch arguments) because
//  different screens need different fixtures; `continueAfterFailure = true` so a
//  hiccup on one screen never blocks capturing the rest.
//

import XCTest

final class DocScreenshotUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = true
    }

    // MARK: - Helpers

    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Capture a full-window screenshot under a stable name.
    private func snap(_ app: XCUIApplication, _ name: String) {
        let shot = app.screenshot()
        let attachment = XCTAttachment(screenshot: shot)
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    /// Launch unpaired, then flip the DEBUG paired toggle so the main tab bar
    /// appears (mirrors the other suites' `launchAndPair`).
    @discardableResult
    private func launchPaired(_ app: XCUIApplication, _ args: [String]) -> Bool {
        app.launchArguments += ["-uitest-reset-pairing"] + args
        app.launch()
        let toggle = element(app, "debug-toggle-paired-button")
        guard toggle.waitForExistence(timeout: 10) else { return false }
        toggle.tap()
        return true
    }

    // MARK: - Screens

    @MainActor
    func test01Pairing() throws {
        let app = XCUIApplication()
        app.launchArguments += ["-uitest-reset-pairing"]
        app.launch()
        XCTAssertTrue(element(app, "PairingView").waitForExistence(timeout: 10))
        snap(app, "01-pairing")
    }

    @MainActor
    func test02ProjectsList() throws {
        let app = XCUIApplication()
        launchPaired(app, ["-uitest-fixture-snapshot", "-uitest-linkstate", "connected:38"])
        XCTAssertTrue(element(app, "ProjectsListView").waitForExistence(timeout: 10))
        XCTAssertTrue(element(app, "project-card-proj_flightdeck").waitForExistence(timeout: 10))
        snap(app, "02-projects-list")
    }

    @MainActor
    func test03SessionsList() throws {
        let app = XCUIApplication()
        launchPaired(app, ["-uitest-fixture-snapshot", "-uitest-linkstate", "connected:38"])
        let card = element(app, "project-card-proj_flightdeck")
        XCTAssertTrue(card.waitForExistence(timeout: 10))
        card.tap()
        XCTAssertTrue(element(app, "SessionsListView").waitForExistence(timeout: 10))
        XCTAssertTrue(element(app, "session-card-sess_fix_login").waitForExistence(timeout: 10))
        snap(app, "03-sessions-list")
    }

    @MainActor
    func test04SessionActions() throws {
        let app = XCUIApplication()
        launchPaired(app, ["-uitest-fixture-snapshot", "-uitest-linkstate", "connected:38"])
        element(app, "project-card-proj_flightdeck").tap()
        XCTAssertTrue(element(app, "SessionsListView").waitForExistence(timeout: 10))
        let actions = element(app, "session-actions-sess_fix_login")
        XCTAssertTrue(actions.waitForExistence(timeout: 10))
        actions.tap()
        XCTAssertTrue(element(app, "SessionActionsSheet").waitForExistence(timeout: 10))
        snap(app, "04-session-actions")
    }

    @MainActor
    func test05GitStatus() throws {
        let app = XCUIApplication()
        launchPaired(app, ["-uitest-fixture-snapshot", "-uitest-fixture-git-status", "-uitest-linkstate", "connected:38"])
        element(app, "project-card-proj_flightdeck").tap()
        XCTAssertTrue(element(app, "SessionsListView").waitForExistence(timeout: 10))
        element(app, "session-actions-sess_fix_login").tap()
        XCTAssertTrue(element(app, "SessionActionsSheet").waitForExistence(timeout: 10))
        let gitRow = element(app, "control-action-git-status")
        XCTAssertTrue(gitRow.waitForExistence(timeout: 10))
        gitRow.tap()
        XCTAssertTrue(element(app, "git-status-view").waitForExistence(timeout: 10))
        snap(app, "05-git-status")
    }

    @MainActor
    func test06NewAgent() throws {
        let app = XCUIApplication()
        launchPaired(app, ["-uitest-fixture-snapshot", "-uitest-linkstate", "connected:38"])
        XCTAssertTrue(element(app, "ProjectsListView").waitForExistence(timeout: 10))
        element(app, "tab-fab-new-agent").tap()
        XCTAssertTrue(element(app, "NewAgentView").waitForExistence(timeout: 10))
        let nameField = element(app, "new-agent-name-field")
        if nameField.waitForExistence(timeout: 3) {
            nameField.tap()
            nameField.typeText("fix signup redirect")
        }
        snap(app, "06-new-agent")
    }

    @MainActor
    func test07AgentChat() throws {
        let app = XCUIApplication()
        launchPaired(app, ["-uitest-fixture-transcript", "-uitest-linkstate", "connected:42"])
        XCTAssertTrue(element(app, "AgentChatView").waitForExistence(timeout: 10))
        snap(app, "07-agent-chat")
    }

    @MainActor
    func test08FocusMode() throws {
        let app = XCUIApplication()
        launchPaired(app, ["-uitest-fixture-transcript", "-uitest-linkstate", "connected:42"])
        XCTAssertTrue(element(app, "AgentChatView").waitForExistence(timeout: 10))
        let enter = element(app, "focus-enter")
        if enter.waitForExistence(timeout: 5) {
            enter.tap()
            XCTAssertTrue(element(app, "focus-mode").waitForExistence(timeout: 10))
            snap(app, "08-focus-mode")
        }
    }

    @MainActor
    func test09Shell() throws {
        let app = XCUIApplication()
        launchPaired(app, ["-uitest-fixture-snapshot", "-uitest-fixture-shell", "-uitest-linkstate", "connected:38"])
        let shellTab = element(app, "tab-shell")
        XCTAssertTrue(shellTab.waitForExistence(timeout: 10))
        shellTab.tap()
        // ShellTabView may auto-select the last/only session, or show a picker.
        let picked = element(app, "shell-session-sess_fix_login")
        if picked.waitForExistence(timeout: 3) {
            picked.tap()
        }
        _ = element(app, "shell-terminal").waitForExistence(timeout: 10)
        snap(app, "09-shell")
    }

    @MainActor
    func test10Feed() throws {
        // The Activity tab was folded into the unified Feed (remote-control-fa8):
        // this now captures the Feed with its attention-first unread rows.
        let app = XCUIApplication()
        launchPaired(app, ["-uitest-fixture-snapshot", "-uitest-fixture-activity", "-uitest-linkstate", "connected:38"])
        let feedTab = element(app, "tab-feed")
        XCTAssertTrue(feedTab.waitForExistence(timeout: 10))
        feedTab.tap()
        XCTAssertTrue(element(app, "FeedView").waitForExistence(timeout: 10))
        snap(app, "10-feed")
    }

    @MainActor
    func test11Settings() throws {
        let app = XCUIApplication()
        launchPaired(app, ["-uitest-fixture-snapshot", "-uitest-reset-applock", "-uitest-linkstate", "connected:38"])
        let settingsTab = element(app, "tab-settings")
        XCTAssertTrue(settingsTab.waitForExistence(timeout: 10))
        settingsTab.tap()
        XCTAssertTrue(element(app, "SettingsView").waitForExistence(timeout: 10))
        snap(app, "11-settings")
    }

    @MainActor
    func test12AppLock() throws {
        let app = XCUIApplication()
        app.launchArguments += ["-uitest-reset-pairing", "-uitest-enable-applock", "-uitest-mock-biometrics"]
        app.launch()
        let lock = element(app, "AppLockView")
        if lock.waitForExistence(timeout: 10) {
            snap(app, "12-app-lock")
        } else if element(app, "applock-unlock-button").waitForExistence(timeout: 5) {
            snap(app, "12-app-lock")
        }
    }
}
