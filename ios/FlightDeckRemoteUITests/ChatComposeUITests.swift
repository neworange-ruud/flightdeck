//
//  ChatComposeUITests.swift
//  FlightDeckRemoteUITests
//
//  Exercises the Tier-1 killer loop end to end with the fixture transcript and
//  a forced link state (`-uitest-linkstate`): with the link up, typing + Send
//  appends an optimistic pending message and the permission Allow/Deny buttons
//  are live (tapping Allow shows a spinner); with the link down, Send is
//  disabled behind the "paused — reconnecting" label and the permission buttons
//  are disabled (PRD §5.3 / §5.8 / §8).
//

import XCTest

final class ChatComposeUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Launch paired, with the fixture transcript and a forced link state; the
    /// fixture arg auto-pushes the chat route.
    private func launchChat(linkState: String) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-uitest-reset-pairing", "-uitest-fixture-transcript",
                                "-uitest-linkstate", linkState]
        app.launch()
        let toggle = element(app, "debug-toggle-paired-button")
        XCTAssertTrue(toggle.waitForExistence(timeout: 5), "Expected the DEBUG pairing toggle")
        toggle.tap()
        XCTAssertTrue(element(app, "AgentChatView").waitForExistence(timeout: 5),
                      "Expected the chat screen to auto-open")
        return app
    }

    @MainActor
    func testConnectedTypeAndSendAppendsPendingMessage() throws {
        let app = launchChat(linkState: "connected:20")

        let field = element(app, "compose-field")
        XCTAssertTrue(field.waitForExistence(timeout: 3))
        // A quick tap on the push-to-talk mic focuses the field (the v1
        // keyboard-dictation fallback) — a deterministic focus path for SwiftUI
        // text fields, whose direct taps can race the keyboard. Fall back to a
        // direct tap.
        element(app, "compose-hold-to-talk").tap()
        if !app.keyboards.firstMatch.waitForExistence(timeout: 3) {
            field.tap()
            _ = app.keyboards.firstMatch.waitForExistence(timeout: 3)
        }
        field.typeText("ship it after the rebuild")

        let send = element(app, "compose-send")
        XCTAssertTrue(send.isEnabled, "Send should be enabled while connected with text")
        send.tap()

        // The optimistic user message appears, marked "Sending…" (the scripted
        // sender leaves it pending so the marker is observable).
        XCTAssertTrue(element(app, "prose-user-sending").waitForExistence(timeout: 3),
                      "Expected an optimistic pending message after Send")
    }

    @MainActor
    func testConnectedPermissionButtonsEnabledAndAllowShowsSpinner() throws {
        let app = launchChat(linkState: "connected:20")

        XCTAssertTrue(element(app, "permission-prompt").waitForExistence(timeout: 3))
        let allow = element(app, "permission-allow")
        let deny = element(app, "permission-deny")
        XCTAssertTrue(allow.isEnabled, "Allow should be enabled while connected on the current prompt")
        XCTAssertTrue(deny.isEnabled, "Deny should be enabled while connected on the current prompt")

        allow.tap()
        // The decision is in flight → a spinner appears on the card.
        XCTAssertTrue(element(app, "working-spinner").waitForExistence(timeout: 3),
                      "Tapping Allow should show a spinner while the decision is sending")
    }

    @MainActor
    func testDisconnectedSendDisabledAndPausedLabelShown() throws {
        let app = launchChat(linkState: "disconnected")

        XCTAssertTrue(element(app, "compose-paused-label").waitForExistence(timeout: 3),
                      "Expected the 'paused — reconnecting' label while disconnected")
        let send = element(app, "compose-send")
        XCTAssertTrue(send.exists)
        XCTAssertFalse(send.isEnabled, "Send should be disabled while the link is down")
    }

    @MainActor
    func testDisconnectedPermissionButtonsDisabled() throws {
        let app = launchChat(linkState: "disconnected")

        XCTAssertTrue(element(app, "permission-prompt").waitForExistence(timeout: 3))
        XCTAssertFalse(element(app, "permission-allow").isEnabled,
                       "Allow should be disabled while the link is down")
        XCTAssertFalse(element(app, "permission-deny").isEnabled,
                       "Deny should be disabled while the link is down")
    }

    // MARK: - Voice compose (hold-to-talk, edit-before-send — PRD §7)

    /// Launch with the scripted-transcript dictation seam so hold-to-talk yields
    /// a deterministic transcript (real STT is unavailable in the simulator).
    private func launchChatWithDictation(transcript: String) -> XCUIApplication {
        let app = XCUIApplication()
        app.launchArguments += ["-uitest-reset-pairing", "-uitest-fixture-transcript",
                                "-uitest-linkstate", "connected:20",
                                "-uitest-dictation-transcript", transcript]
        app.launch()
        let toggle = element(app, "debug-toggle-paired-button")
        XCTAssertTrue(toggle.waitForExistence(timeout: 5))
        toggle.tap()
        XCTAssertTrue(element(app, "AgentChatView").waitForExistence(timeout: 5))
        return app
    }

    @MainActor
    func testHoldToTalkDropsEditableTranscriptThenEditAndSend() throws {
        let app = launchChatWithDictation(transcript: "Yes, run it")

        let mic = element(app, "compose-hold-to-talk")
        XCTAssertTrue(mic.waitForExistence(timeout: 3))
        // HOLD to record → RELEASE to stop. Held well past the dictation minimum
        // so the scripted transcript commits (a quick tap would just focus).
        mic.press(forDuration: 1.0)

        // The transcript lands in the field as EDITABLE text — never auto-sent.
        let field = element(app, "compose-field")
        XCTAssertTrue(field.waitForExistence(timeout: 3))
        let value = (field.value as? String) ?? ""
        XCTAssertTrue(value.contains("Yes, run it"),
                      "Expected the dictated transcript in the editable field, got: \(value)")

        // Nothing was sent yet — no optimistic message exists.
        XCTAssertFalse(element(app, "prose-user-sending").exists,
                       "Dictation must never auto-send (edit-before-send, always)")

        // Edit the dictated text, then Send.
        field.tap()
        field.typeText(". Then rebuild.")
        let send = element(app, "compose-send")
        XCTAssertTrue(send.isEnabled)
        send.tap()
        XCTAssertTrue(element(app, "prose-user-sending").waitForExistence(timeout: 3),
                      "Expected an optimistic pending message after editing + Send")
    }
}
