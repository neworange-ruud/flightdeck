//
//  PairingUITests.swift
//  FlightDeckRemoteUITests
//
//  Exercises the pairing screen itself (PRD §5.6): the title/code
//  boxes/scan button render, typing a complete code enables "Pair", a
//  valid code (against `MockPairingService`, the app's current default
//  `PairingServicing`) transitions into the main tab container, and an
//  invalid code shows an inline error and stays on the Pairing screen.
//
//  Mirrors NavigationUITests' launch-unpaired convention: pairing state
//  persists across launches by design, so every scenario resets it via
//  `-uitest-reset-pairing`.
//

import XCTest

final class PairingUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    /// Looks up an element by accessibility identifier regardless of its
    /// underlying `XCUIElement` type (same helper as NavigationUITests /
    /// ComponentGalleryUITests — SwiftUI containers don't always map to the
    /// type a naive `app.otherElements[...]` query would expect).
    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    private func launchUnpaired(_ app: XCUIApplication) {
        app.launchArguments += ["-uitest-reset-pairing"]
        app.launch()
    }

    /// Taps the hidden code field and types the given digits.
    private func enterCode(_ code: String, in app: XCUIApplication) {
        let field = element(app, "code-entry-field")
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.tap()
        field.typeText(code)
    }

    @MainActor
    func testPairingScreenRendersTitleCodeBoxesAndScanButton() throws {
        let app = XCUIApplication()
        launchUnpaired(app)

        XCTAssertTrue(element(app, "PairingView").waitForExistence(timeout: 5))
        XCTAssertTrue(element(app, "pairing-title").exists)
        for index in 0..<4 {
            XCTAssertTrue(element(app, "code-digit-box-\(index)").exists, "Expected digit box \(index)")
        }
        XCTAssertTrue(element(app, "scan-qr-button").exists)
        XCTAssertTrue(element(app, "pair-button").exists)
    }

    @MainActor
    func testTypingValidCodeEnablesPairButton() throws {
        let app = XCUIApplication()
        launchUnpaired(app)

        let pairButton = element(app, "pair-button")
        XCTAssertTrue(pairButton.waitForExistence(timeout: 5))
        XCTAssertFalse(pairButton.isEnabled, "Pair should start disabled with no code entered")

        enterCode("4729", in: app)

        XCTAssertTrue(pairButton.isEnabled, "Pair should enable once 4 digits are entered")
    }

    @MainActor
    func testTappingPairWithValidCodeTransitionsToMainTabBar() throws {
        let app = XCUIApplication()
        launchUnpaired(app)

        enterCode("4729", in: app)
        let pairButton = element(app, "pair-button")
        XCTAssertTrue(pairButton.isEnabled)
        pairButton.tap()

        // MockPairingService simulates a ~1s round trip.
        XCTAssertTrue(element(app, "MainTabView").waitForExistence(timeout: 10), "Expected a valid code to pair and reveal the main tab container")
        XCTAssertFalse(element(app, "PairingView").exists)
    }

    @MainActor
    func testBadCodeShowsErrorAndStaysOnPairingScreen() throws {
        let app = XCUIApplication()
        launchUnpaired(app)

        enterCode("0000", in: app)
        let pairButton = element(app, "pair-button")
        XCTAssertTrue(pairButton.isEnabled)
        pairButton.tap()

        XCTAssertTrue(element(app, "pairing-error-text").waitForExistence(timeout: 10), "Expected an inline error for a bad code")
        XCTAssertTrue(element(app, "PairingView").exists, "Should remain on the Pairing screen after a rejected code")
        XCTAssertFalse(element(app, "MainTabView").exists)
    }
}
