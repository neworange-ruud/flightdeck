//
//  ChatTranscriptUITests.swift
//  FlightDeckRemoteUITests
//
//  Exercises the cleaned agent-chat transcript (PRD §5.3 3a) with the DEBUG
//  `-uitest-fixture-transcript` seam: the screen renders prose + activity
//  pills, tapping a pill expands its detail, the inline permission card shows
//  with disabled Allow/Deny buttons + voice hint, the Agent·Shell surface
//  switcher is present with Shell disabled, and the compose bar is present.
//

import XCTest

final class ChatTranscriptUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Launch paired (reset first), with the transcript fixture, which
    /// auto-pushes the fixture chat route onto the Projects stack.
    private func launchChatFixture() -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-uitest-reset-pairing", "-uitest-fixture-transcript"]
        app.launch()

        // Cross into the paired tab container.
        let toggle = element(app, "debug-toggle-paired-button")
        XCTAssertTrue(toggle.waitForExistence(timeout: 5), "Expected the DEBUG pairing toggle")
        toggle.tap()
        return app
    }

    @MainActor
    func testTranscriptRendersAndPillExpands() throws {
        let app = launchChatFixture()

        XCTAssertTrue(element(app, "AgentChatView").waitForExistence(timeout: 5),
                      "Expected the chat screen to auto-open under the fixture arg")

        // Prose renders.
        XCTAssertTrue(element(app, "prose-user").waitForExistence(timeout: 3))
        XCTAssertTrue(element(app, "prose-agent").exists)

        // An activity pill renders (fixture position 2 is the edit pill).
        let pill = element(app, "pill-2")
        XCTAssertTrue(pill.waitForExistence(timeout: 3), "Expected an activity pill")

        // The screen scrolls to the pending permission prompt on entry, so the
        // early edit pill may be above the fold — reveal it before tapping.
        var attempts = 0
        while !pill.isHittable && attempts < 4 {
            app.swipeDown()
            attempts += 1
        }

        // Detail is collapsed initially, expands on tap.
        XCTAssertFalse(element(app, "pill-detail-2").exists, "Detail should start collapsed")
        pill.tap()
        XCTAssertTrue(element(app, "pill-detail-2").waitForExistence(timeout: 3),
                      "Tapping the pill should expand its detail")
    }

    @MainActor
    func testPermissionCardChromeAndDisabledButtons() throws {
        let app = launchChatFixture()
        XCTAssertTrue(element(app, "AgentChatView").waitForExistence(timeout: 5))

        XCTAssertTrue(element(app, "permission-prompt").waitForExistence(timeout: 3),
                      "Expected the inline permission card")
        let allow = element(app, "permission-allow")
        let deny = element(app, "permission-deny")
        XCTAssertTrue(allow.exists)
        XCTAssertTrue(deny.exists)
        // Buttons are rendered but disabled this task (chat-permission wires them).
        XCTAssertFalse(allow.isEnabled, "Allow should be disabled this task")
        XCTAssertFalse(deny.isEnabled, "Deny should be disabled this task")
        XCTAssertTrue(element(app, "permission-voice-hint").exists,
                      "Expected the voice hint line")
    }

    @MainActor
    func testSurfaceSwitcherAndComposeBarPresent() throws {
        let app = launchChatFixture()
        XCTAssertTrue(element(app, "AgentChatView").waitForExistence(timeout: 5))

        XCTAssertTrue(element(app, "surface-switcher").exists, "Expected the Agent·Shell switcher")
        XCTAssertTrue(element(app, "surface-agent").exists)
        let shell = element(app, "surface-shell")
        XCTAssertTrue(shell.exists)
        XCTAssertFalse(shell.isEnabled, "Shell segment should be disabled (soon)")

        XCTAssertTrue(element(app, "chat-compose-bar").exists, "Expected the inert compose bar")
    }
}
