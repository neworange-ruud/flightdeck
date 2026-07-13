//
//  FlightDeckRemoteUITests.swift
//  FlightDeckRemoteUITests
//
//  Smoke test: the app launches unpaired and lands on the Pairing screen
//  (PRD §5.8 entry flow). Pairing state now persists across launches (see
//  PairingStore), so the launch passes `-uitest-reset-pairing` to guarantee
//  a known unpaired starting state regardless of other tests/runs.
//

import XCTest

final class FlightDeckRemoteUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    @MainActor
    func testAppLaunchesToPairingScreen() throws {
        let app = XCUIApplication()
        app.launchArguments += ["-uitest-reset-pairing"]
        app.launch()

        let pairingView = app.otherElements["PairingView"]
        XCTAssertTrue(pairingView.waitForExistence(timeout: 5), "Expected the Pairing screen to appear on launch while unpaired")
    }
}
