//
//  NavigationUITests.swift
//  FlightDeckRemoteUITests
//
//  Exercises the bottom-tab navigation + entry routing (PRD §5.7/§5.8):
//  unpaired shows Pairing; the DEBUG pairing toggle crosses into the main
//  tab container; tapping tabs switches content; the center FAB presents
//  the New-agent sheet; the Activity unread badge is visible then clears.
//

import XCTest

final class NavigationUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    /// Looks up an element by accessibility identifier regardless of its
    /// underlying XCUIElement type (mirrors ComponentGalleryUITests' helper
    /// — SwiftUI containers don't always map to the type a naive
    /// `app.otherElements[...]` query would expect).
    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Launches with pairing state reset (pairing persists across launches
    /// by design — see PairingStore — so every scenario must start from a
    /// known unpaired state regardless of what earlier tests toggled).
    private func launchUnpaired(_ app: XCUIApplication) {
        app.launchArguments += ["-uitest-reset-pairing"]
        app.launch()
    }

    /// Launches unpaired and taps the DEBUG "Toggle Paired" button on the
    /// Pairing screen so the app crosses into the main tab container.
    private func launchAndPair(_ app: XCUIApplication) {
        launchUnpaired(app)
        let toggle = element(app, "debug-toggle-paired-button")
        XCTAssertTrue(toggle.waitForExistence(timeout: 5), "Expected the DEBUG pairing toggle on the Pairing screen")
        toggle.tap()
    }

    @MainActor
    func testUnpairedShowsPairingScreen() throws {
        let app = XCUIApplication()
        launchUnpaired(app)

        XCTAssertTrue(element(app, "PairingView").waitForExistence(timeout: 5))
        XCTAssertFalse(element(app, "MainTabView").exists, "Should not show the tab container while unpaired")
    }

    @MainActor
    func testDebugTogglePairingRevealsTabBarWithFourTabsAndFAB() throws {
        let app = XCUIApplication()
        launchAndPair(app)

        XCTAssertTrue(element(app, "MainTabView").waitForExistence(timeout: 5), "Expected the tab container after toggling paired state")
        XCTAssertTrue(element(app, "tab-projects").exists)
        XCTAssertTrue(element(app, "tab-activity").exists)
        XCTAssertTrue(element(app, "tab-shell").exists)
        XCTAssertTrue(element(app, "tab-settings").exists)
        XCTAssertTrue(element(app, "tab-fab-new-agent").exists)
    }

    @MainActor
    func testTappingTabsSwitchesContent() throws {
        let app = XCUIApplication()
        launchAndPair(app)

        XCTAssertTrue(element(app, "ProjectsListView").waitForExistence(timeout: 5), "Expected Projects to be the default tab")

        element(app, "tab-shell").tap()
        XCTAssertTrue(element(app, "ShellTabView").waitForExistence(timeout: 5))

        element(app, "tab-settings").tap()
        XCTAssertTrue(element(app, "SettingsView").waitForExistence(timeout: 5))

        element(app, "tab-activity").tap()
        XCTAssertTrue(element(app, "ActivityFeedView").waitForExistence(timeout: 5))

        element(app, "tab-projects").tap()
        XCTAssertTrue(element(app, "ProjectsListView").waitForExistence(timeout: 5))
    }

    @MainActor
    func testFABPresentsNewAgentSheet() throws {
        let app = XCUIApplication()
        launchAndPair(app)

        element(app, "tab-fab-new-agent").tap()
        XCTAssertTrue(element(app, "NewAgentView").waitForExistence(timeout: 5))
    }

    @MainActor
    func testActivityBadgeVisibleThenClearsOnSelection() throws {
        let app = XCUIApplication()
        // The badge is driven by real (persisted) events now — seed the
        // deterministic all-unread fixture feed rather than relying on the
        // old stub's hardcoded default of 1 unread.
        app.launchArguments += ["-uitest-fixture-activity"]
        launchAndPair(app)

        XCTAssertTrue(element(app, "tab-activity-unread-badge").waitForExistence(timeout: 5), "Expected the unread badge with unviewed fixture events")

        element(app, "tab-activity").tap()
        XCTAssertFalse(element(app, "tab-activity-unread-badge").waitForExistence(timeout: 2), "Expected the unread badge to clear once Activity is viewed")
    }
}
