//
//  AppLockUITests.swift
//  FlightDeckRemoteUITests
//
//  Exercises the Face-ID app-open gate (PRD §5.6/§9). Real Face ID can't be
//  driven in the simulator, so these tests use the `-uitest-enable-applock`
//  DEBUG hook (see `AppLockController.init`), which forces the gate on and
//  swaps in a mock authenticator that always succeeds — this also suppresses
//  the automatic "unlock once" attempt so the lock screen reliably stays up
//  until the test drives the "Unlock with Face ID" button itself.
//
//  Default-launch (no `-uitest-enable-applock`) behavior — no lock screen,
//  since the gate defaults to disabled — is covered by every other UI test
//  suite staying green, plus `testDefaultLaunchNeverShowsLockScreen` below as
//  an explicit assertion.
//

import XCTest

final class AppLockUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    /// Looks up an element by accessibility identifier regardless of its
    /// underlying XCUIElement type (mirrors NavigationUITests' helper —
    /// SwiftUI containers don't always map to the type a naive
    /// `app.otherElements[...]` query would expect).
    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    @MainActor
    func testLockScreenAppearsAndUnlockButtonRevealsUnderlyingScreen() throws {
        let app = XCUIApplication()
        app.launchArguments += ["-uitest-reset-pairing", "-uitest-enable-applock"]
        app.launch()

        let lockScreen = element(app, "AppLockView")
        XCTAssertTrue(
            lockScreen.waitForExistence(timeout: 5),
            "Expected the Face-ID lock screen on launch when the gate is force-enabled"
        )

        let unlockButton = element(app, "applock-unlock-button")
        XCTAssertTrue(unlockButton.waitForExistence(timeout: 5))
        unlockButton.tap()

        XCTAssertTrue(
            element(app, "PairingView").waitForExistence(timeout: 5),
            "Expected the underlying Pairing screen once unlocked (pairing was reset)"
        )
        XCTAssertFalse(lockScreen.exists, "Lock screen should be dismissed after a successful unlock")
    }

    @MainActor
    func testDefaultLaunchNeverShowsLockScreen() throws {
        let app = XCUIApplication()
        app.launchArguments += ["-uitest-reset-pairing"]
        app.launch()

        XCTAssertTrue(element(app, "PairingView").waitForExistence(timeout: 5))
        XCTAssertFalse(
            element(app, "AppLockView").exists,
            "The Face-ID gate defaults to disabled — no lock screen should ever appear on a plain launch"
        )
    }
}
