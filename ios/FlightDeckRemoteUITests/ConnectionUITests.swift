//
//  ConnectionUITests.swift
//  FlightDeckRemoteUITests
//
//  Exercises the reconnecting banner's visible behavior (PRD §5.6/§8) end to
//  end via the `-uitest-linkstate <state>` DEBUG seam (`ConnectionDebugSeam`)
//  — real relay connectivity can't be driven in the simulator, so this forces
//  the link state the same way `-uitest-enable-applock` forces the Face-ID
//  gate (see `AppLockUITests`).
//

import XCTest

final class ConnectionUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    /// Looks up an element by accessibility identifier regardless of its
    /// underlying XCUIElement type (mirrors NavigationUITests'/AppLockUITests'
    /// helper).
    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Launches unpaired, forces the given link state, then taps the DEBUG
    /// "Toggle Paired" button on the Pairing screen to reach `MainTabView`.
    private func launchAndPair(_ app: XCUIApplication, linkState: String) {
        app.launchArguments += ["-uitest-reset-pairing", "-uitest-linkstate", linkState]
        app.launch()

        let toggle = element(app, "debug-toggle-paired-button")
        XCTAssertTrue(toggle.waitForExistence(timeout: 5), "Expected the DEBUG pairing toggle on the Pairing screen")
        toggle.tap()
    }

    @MainActor
    func testForcedReconnectingStateShowsTheBannerText() throws {
        let app = XCUIApplication()
        launchAndPair(app, linkState: "disconnected")

        XCTAssertTrue(element(app, "MainTabView").waitForExistence(timeout: 5))
        XCTAssertTrue(
            element(app, "reconnecting-banner-headline").waitForExistence(timeout: 5),
            "Expected the reconnecting banner while forced disconnected and paired"
        )
        XCTAssertTrue(element(app, "reconnecting-banner-body").exists)
    }

    @MainActor
    func testForcedConnectedStateHidesTheBanner() throws {
        let app = XCUIApplication()
        launchAndPair(app, linkState: "connected:20")

        XCTAssertTrue(element(app, "MainTabView").waitForExistence(timeout: 5))
        XCTAssertFalse(
            element(app, "reconnecting-banner-headline").waitForExistence(timeout: 2),
            "Expected no reconnecting banner while forced fully connected"
        )
    }

    @MainActor
    func testForcedConnectingStateAlsoShowsTheBanner() throws {
        let app = XCUIApplication()
        launchAndPair(app, linkState: "connecting")

        XCTAssertTrue(element(app, "MainTabView").waitForExistence(timeout: 5))
        XCTAssertTrue(
            element(app, "reconnecting-banner-headline").waitForExistence(timeout: 5),
            "Expected the reconnecting banner while forced connecting and paired"
        )
    }
}
