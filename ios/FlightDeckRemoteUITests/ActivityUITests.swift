//
//  ActivityUITests.swift
//  FlightDeckRemoteUITests
//
//  Exercises the Activity feed (PRD §5.7) and the offline stale banner
//  (PRD §9.2) end to end via the DEBUG seams:
//   - `-uitest-fixture-activity` seeds a canned, all-unread event feed
//     (`ActivityFixtures`) into an in-memory `ActivityStore`;
//   - `-uitest-fixture-snapshot` seeds the snapshot the deep-link translator
//     validates taps against (same fixture the Projects/Sessions tests use);
//   - `-uitest-fixture-snapshot-stale` seeds a cache-seeded (stale) snapshot;
//   - `-uitest-linkstate <state>` forces the link state (ConnectionDebugSeam).
//

import XCTest

final class ActivityUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Launches unpaired with the given extra arguments, then taps the DEBUG
    /// "Toggle Paired" button to reach `MainTabView`.
    private func launchAndPair(_ app: XCUIApplication, arguments: [String]) {
        app.launchArguments += ["-uitest-reset-pairing"] + arguments
        app.launch()

        let toggle = element(app, "debug-toggle-paired-button")
        XCTAssertTrue(toggle.waitForExistence(timeout: 5), "Expected the DEBUG pairing toggle on the Pairing screen")
        toggle.tap()
        XCTAssertTrue(element(app, "MainTabView").waitForExistence(timeout: 5))
    }

    // MARK: - Feed

    @MainActor
    func testFixtureEventsRenderAsCellsAndBadgeClearsOnView() throws {
        let app = XCUIApplication()
        launchAndPair(app, arguments: ["-uitest-fixture-activity", "-uitest-fixture-snapshot"])

        // Unread badge visible before viewing (fixture events are all unread).
        XCTAssertTrue(element(app, "tab-activity-unread-badge").waitForExistence(timeout: 5),
                      "Expected the unread badge while the fixture events are unviewed")

        element(app, "tab-activity").tap()
        XCTAssertTrue(element(app, "ActivityFeedView").waitForExistence(timeout: 5))

        // All three variants render (needs-input / finished / error).
        XCTAssertTrue(element(app, "activity-cell-fx-evt-needs-input").waitForExistence(timeout: 5))
        XCTAssertTrue(element(app, "activity-cell-fx-evt-finished").exists)
        XCTAssertTrue(element(app, "activity-cell-fx-evt-error").exists)

        // Viewing the tab clears the badge (PRD §5.7).
        XCTAssertFalse(element(app, "tab-activity-unread-badge").waitForExistence(timeout: 2),
                       "Expected the unread badge to clear once Activity is viewed")
    }

    @MainActor
    func testTappingACellDeepLinksToTheAgentChat() throws {
        let app = XCUIApplication()
        launchAndPair(app, arguments: ["-uitest-fixture-activity", "-uitest-fixture-snapshot"])

        element(app, "tab-activity").tap()
        let cell = element(app, "activity-cell-fx-evt-needs-input")
        XCTAssertTrue(cell.waitForExistence(timeout: 5))
        cell.tap()

        // The tap reuses the deep-link path: Projects tab + [.sessions, .chat] push.
        XCTAssertTrue(element(app, "AgentChatView").waitForExistence(timeout: 5),
                      "Expected tapping an activity cell to land on the agent chat")
    }

    @MainActor
    func testTappingADeadSessionCellShowsANoteAndStaysPut() throws {
        let app = XCUIApplication()
        launchAndPair(app, arguments: ["-uitest-fixture-activity", "-uitest-fixture-snapshot"])

        element(app, "tab-activity").tap()
        let cell = element(app, "activity-cell-fx-evt-dead-session")
        XCTAssertTrue(cell.waitForExistence(timeout: 5))
        cell.tap()

        XCTAssertTrue(element(app, "activity-dead-session-note").waitForExistence(timeout: 3),
                      "Expected the 'session no longer active' note")
        XCTAssertTrue(element(app, "ActivityFeedView").exists, "Should stay on the Activity feed")
        XCTAssertFalse(element(app, "AgentChatView").exists)
    }

    // MARK: - Stale banner (PRD §9.2)

    @MainActor
    func testStaleBannerShowsWhileCacheSeededAndDisconnected() throws {
        let app = XCUIApplication()
        launchAndPair(app, arguments: ["-uitest-fixture-snapshot-stale", "-uitest-linkstate", "disconnected"])

        XCTAssertTrue(element(app, "stale-banner").waitForExistence(timeout: 5),
                      "Expected the stale banner while cache-seeded and disconnected")
        // The cached content itself still renders (read-only last-known state).
        XCTAssertTrue(element(app, "project-card-proj_flightdeck").waitForExistence(timeout: 5),
                      "Expected the cached snapshot's projects to render")
    }

    @MainActor
    func testStaleBannerHiddenWhileConnected() throws {
        let app = XCUIApplication()
        launchAndPair(app, arguments: ["-uitest-fixture-snapshot-stale", "-uitest-linkstate", "connected:20"])

        XCTAssertFalse(element(app, "stale-banner").waitForExistence(timeout: 2),
                       "Expected no stale banner while the link is fully connected")
    }
}
