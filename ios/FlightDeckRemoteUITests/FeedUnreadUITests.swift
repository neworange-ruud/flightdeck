//
//  FeedUnreadUITests.swift
//  FlightDeckRemoteUITests
//
//  Exercises the unified Feed's Activity-derived value end to end
//  (remote-control-fa8), plus the offline stale banner (PRD §9.2), via the
//  DEBUG seams:
//   - `-uitest-fixture-activity` now seeds the FEED (not a separate Activity
//     tab): one canned row per attention variant (needs-input / finished /
//     error), all initially unread, so the Feed tab shows an unread badge and
//     per-row unread dots (`FeedStore.Fixture` / `ActivityFixtures`);
//   - `-uitest-fixture-snapshot` seeds the snapshot the deep-linked chat/
//     sessions destinations render against;
//   - `-uitest-fixture-snapshot-stale` seeds a cache-seeded (stale) snapshot;
//   - `-uitest-linkstate <state>` forces the link state (ConnectionDebugSeam).
//

import XCTest

final class FeedUnreadUITests: XCTestCase {

    /// The synthetic machine the `-uitest-fixture-activity` feed is attributed
    /// to (mirrors `FeedStore.Fixture.pairingId`); a feed item's id — and its
    /// per-row accessibility identifiers — join it to the project id with the
    /// ASCII unit separator (mirrors `FeedItem.id`).
    private let fixturePairing = "uitest-fixture-machine"
    private func itemId(_ projectId: String) -> String { "\(fixturePairing)\u{1f}\(projectId)" }

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

    // MARK: - Feed unread rows + badge

    @MainActor
    func testFixtureRowsRenderUnreadWithTheFeedBadge() throws {
        let app = XCUIApplication()
        launchAndPair(app, arguments: ["-uitest-fixture-activity", "-uitest-fixture-snapshot"])

        // Badge visible before opening any row (fixture rows are all unread) —
        // it now rides the FEED tab, not the removed Activity tab.
        XCTAssertTrue(element(app, "tab-feed-unread-badge").waitForExistence(timeout: 5),
                      "Expected the Feed unread badge while fixture rows are unopened")

        element(app, "tab-feed").tap()
        XCTAssertTrue(element(app, "FeedView").waitForExistence(timeout: 5))

        // The needs-input row (flightdeck) renders and is marked unread.
        let needsInputDot = element(app, "feed-unread-dot-\(itemId("proj_flightdeck"))")
        XCTAssertTrue(needsInputDot.waitForExistence(timeout: 5), "Expected the needs-input row's unread dot")
        XCTAssertEqual(needsInputDot.label, "Unread")
    }

    @MainActor
    func testOpeningARowMarksItReadAndLeavesOthersUnread() throws {
        let app = XCUIApplication()
        launchAndPair(app, arguments: ["-uitest-fixture-activity", "-uitest-fixture-snapshot"])

        element(app, "tab-feed").tap()
        XCTAssertTrue(element(app, "FeedView").waitForExistence(timeout: 5))

        // The finished row (remote-control) opens the sessions list (calm rows
        // don't deep-link into chat).
        let finishedRow = element(app, "feed-row-\(itemId("proj_remote_control"))")
        XCTAssertTrue(finishedRow.waitForExistence(timeout: 5))
        let finishedDot = element(app, "feed-unread-dot-\(itemId("proj_remote_control"))")
        XCTAssertEqual(finishedDot.label, "Unread")

        finishedRow.tap()
        XCTAssertTrue(element(app, "SessionsListView").waitForExistence(timeout: 5),
                      "Expected a calm row to open the project's sessions list")

        // Back to the feed — that row is now read, the badge still shows (the
        // other two rows remain unread).
        element(app, "sessions-back-to-projects").tap()
        XCTAssertTrue(element(app, "FeedView").waitForExistence(timeout: 5))
        XCTAssertEqual(element(app, "feed-unread-dot-\(itemId("proj_remote_control"))").label, "Read",
                       "Opening a row should mark THAT item read")
        XCTAssertTrue(element(app, "tab-feed-unread-badge").exists,
                      "Two rows remain unread → the Feed badge should persist")
    }

    @MainActor
    func testTappingANeedsInputRowDeepLinksStraightToItsChat() throws {
        let app = XCUIApplication()
        launchAndPair(app, arguments: ["-uitest-fixture-activity", "-uitest-fixture-snapshot"])

        element(app, "tab-feed").tap()
        let needsInputRow = element(app, "feed-row-\(itemId("proj_flightdeck"))")
        XCTAssertTrue(needsInputRow.waitForExistence(timeout: 5))
        needsInputRow.tap()

        // A needs-input/error row deep-links STRAIGHT into that session's chat.
        XCTAssertTrue(element(app, "AgentChatView").waitForExistence(timeout: 5),
                      "Expected a needs-input row to deep-link into the agent chat")
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
