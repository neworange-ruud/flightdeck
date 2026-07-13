//
//  FlightDeckRemoteUITests.swift
//  FlightDeckRemoteUITests
//
//  Smoke test: the app launches and, since PairingStore.isPaired stubs to
//  false, lands on the Pairing screen (PRD §5.8 entry flow).
//

import XCTest

final class FlightDeckRemoteUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    @MainActor
    func testAppLaunchesToPairingScreen() throws {
        let app = XCUIApplication()
        app.launch()

        let pairingView = app.otherElements["PairingView"]
        XCTAssertTrue(pairingView.waitForExistence(timeout: 5), "Expected the Pairing screen to appear on launch while unpaired")
    }
}
