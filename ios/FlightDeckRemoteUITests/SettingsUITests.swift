//
//  SettingsUITests.swift
//  FlightDeckRemoteUITests
//
//  Exercises the Settings screen end to end (PRD §5.6): the connected-device
//  card + all sections render, the "Require Face ID to open" toggle
//  persists its value across a real app relaunch (via the
//  `-uitest-reset-applock` hook — mirrors `-uitest-reset-pairing`), and the
//  unpair flow (tap → confirmation dialog → confirm) lands back on the
//  Pairing screen.
//

import XCTest

final class SettingsUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    /// Looks up an element by accessibility identifier regardless of its
    /// underlying XCUIElement type (mirrors NavigationUITests'/AppLockUITests'
    /// helper — SwiftUI containers don't always map to the type a naive
    /// `app.otherElements[...]` query would expect).
    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Launches unpaired and taps the DEBUG "Toggle Paired" button on the
    /// Pairing screen so the app crosses into the main tab container,
    /// mirroring `NavigationUITests.launchAndPair`. Extra launch arguments
    /// can be layered on top.
    ///
    /// Always resets the Face-ID gate too (`-uitest-reset-applock`): this
    /// suite is the one place that deliberately persists `isLockEnabled =
    /// true` mid-test, so if an earlier (possibly interrupted) run leaked
    /// that flag, the lock overlay would sit above the Pairing screen and
    /// silently swallow the debug-toggle tap — the toggle still *exists*
    /// underneath (ZStack), so only the MainTabView wait would fail,
    /// miles from the actual cause.
    private func launchAndPair(_ app: XCUIApplication, extraArguments: [String] = []) {
        app.launchArguments += ["-uitest-reset-pairing", "-uitest-reset-applock"] + extraArguments
        app.launch()

        let toggle = element(app, "debug-toggle-paired-button")
        XCTAssertTrue(toggle.waitForExistence(timeout: 5), "Expected the DEBUG pairing toggle on the Pairing screen")
        toggle.tap()
        XCTAssertTrue(element(app, "MainTabView").waitForExistence(timeout: 5))
    }

    private func openSettingsTab(_ app: XCUIApplication) {
        element(app, "tab-settings").tap()
        XCTAssertTrue(element(app, "SettingsView").waitForExistence(timeout: 5))
    }

    // MARK: - Layout

    @MainActor
    func testSettingsShowsDeviceCardAndAllSections() throws {
        let app = XCUIApplication()
        launchAndPair(app)
        openSettingsTab(app)

        XCTAssertTrue(element(app, "settings-connection-card").exists)
        XCTAssertTrue(element(app, "settings-device-name").exists)
        XCTAssertTrue(element(app, "connection-indicator").exists, "Expected the reused ConnectionIndicator on the device card")

        XCTAssertTrue(element(app, "settings-security-card").exists)
        XCTAssertTrue(element(app, "settings-faceid-toggle").exists)
        XCTAssertTrue(element(app, "settings-unpair-button").exists)

        XCTAssertTrue(element(app, "settings-notifications-card").exists)
        XCTAssertTrue(element(app, "settings-notif-needsinput").exists)
        XCTAssertTrue(element(app, "settings-notif-finished").exists)
        XCTAssertTrue(element(app, "settings-notif-chime").exists)
        XCTAssertTrue(element(app, "settings-about-card").exists)
    }

    // MARK: - Notification toggles + per-project mute (PRD §5.6/§9.2)

    @MainActor
    func testNotificationTogglesFlipAndMuteAppearsWithProjects() throws {
        let app = XCUIApplication()
        // The snapshot fixture gives the per-project mute list real rows;
        // reset persisted prefs so the "defaults on" assertion is hermetic
        // (this test flips a toggle, which would otherwise leak to later runs).
        launchAndPair(app, extraArguments: ["-uitest-fixture-snapshot", "-uitest-reset-notif-prefs"])
        openSettingsTab(app)

        let finished = element(app, "settings-notif-finished")
        XCTAssertTrue(finished.waitForExistence(timeout: 5))
        XCTAssertEqual(finished.value as? String, "1", "Toggles default on")

        let innerSwitch = finished.switches.firstMatch
        (innerSwitch.exists ? innerSwitch : finished).tap()
        XCTAssertEqual(finished.value as? String, "0", "Expected the finished toggle to flip off")

        // Per-project mute rows render from the fixture snapshot.
        XCTAssertTrue(element(app, "settings-notif-mute-card").waitForExistence(timeout: 5))
    }

    // MARK: - Face ID toggle persistence

    @MainActor
    func testFaceIDTogglePersistsAcrossRelaunch() throws {
        // This test deliberately persists `isLockEnabled = true` mid-flight.
        // Every other UI suite launches WITHOUT `-uitest-reset-applock`
        // (the gate is expected to default off), so a leak here would put a
        // lock screen in front of every later suite. Guarantee cleanup even
        // if an assertion below fails, by relaunching with the reset hook.
        addTeardownBlock { @MainActor in
            let cleanup = XCUIApplication()
            cleanup.launchArguments = ["-uitest-reset-applock"]
            cleanup.launch()
            cleanup.terminate()
        }

        let app = XCUIApplication()
        launchAndPair(app)
        openSettingsTab(app)

        let toggle = element(app, "settings-faceid-toggle")
        XCTAssertTrue(toggle.waitForExistence(timeout: 5))
        XCTAssertEqual(toggle.value as? String, "0", "Expected the gate to start disabled after -uitest-reset-applock")

        // Tap the inner switch, not the toggle row: a SwiftUI Toggle's outer
        // element spans label + switch, so its center point (the empty gap
        // between them) has no hit target and a plain `toggle.tap()` no-ops.
        let innerSwitch = toggle.switches.firstMatch
        (innerSwitch.exists ? innerSwitch : toggle).tap()
        XCTAssertEqual(toggle.value as? String, "1", "Expected the toggle to flip on immediately")

        // Relaunch without the reset argument: pairing persists by design
        // (PairingStore), and the Face-ID toggle should now persist too,
        // through the real `UserDefaultsAppLockSettingsProvider`.
        // `-uitest-mock-biometrics` swaps in the always-succeeding mock
        // authenticator WITHOUT forcing the gate on (unlike
        // `-uitest-enable-applock`, which would mask persistence), so
        // whether the lock screen appears is decided purely by the persisted
        // toggle — its appearance IS the persistence proof — and the unlock
        // button then succeeds deterministically without a real Face ID
        // prompt.
        app.terminate()
        app.launchArguments = ["-uitest-mock-biometrics"]
        app.launch()

        let lockScreen = element(app, "AppLockView")
        XCTAssertTrue(lockScreen.waitForExistence(timeout: 10), "Expected the lock screen on relaunch — the enabled toggle should have persisted")

        element(app, "applock-unlock-button").tap()
        XCTAssertTrue(element(app, "MainTabView").waitForExistence(timeout: 10), "Expected pairing to have persisted across relaunch")
        openSettingsTab(app)

        let reloadedToggle = element(app, "settings-faceid-toggle")
        XCTAssertTrue(reloadedToggle.waitForExistence(timeout: 5))
        XCTAssertEqual(reloadedToggle.value as? String, "1", "Expected the Face-ID toggle to have persisted across relaunch")
    }

    // MARK: - Unpair flow

    @MainActor
    func testUnpairFlowShowsConfirmationThenLandsOnPairingScreen() throws {
        let app = XCUIApplication()
        launchAndPair(app)
        openSettingsTab(app)

        let unpairButton = element(app, "settings-unpair-button")
        XCTAssertTrue(unpairButton.waitForExistence(timeout: 5))
        unpairButton.tap()

        let confirmButton = app.buttons["Unpair"]
        XCTAssertTrue(confirmButton.waitForExistence(timeout: 5), "Expected a confirmation dialog with a destructive 'Unpair' action")
        confirmButton.tap()

        XCTAssertTrue(element(app, "PairingView").waitForExistence(timeout: 5), "Expected unpairing to route back to the Pairing screen")
        XCTAssertFalse(element(app, "MainTabView").exists)
    }

    @MainActor
    func testUnpairFlowCancelStaysOnSettings() throws {
        let app = XCUIApplication()
        launchAndPair(app)
        openSettingsTab(app)

        let unpairButton = element(app, "settings-unpair-button")
        XCTAssertTrue(unpairButton.waitForExistence(timeout: 5))
        unpairButton.tap()

        // Wait for the dialog via its destructive action — always present in
        // both renderings of confirmationDialog.
        XCTAssertTrue(app.buttons["Unpair"].waitForExistence(timeout: 5), "Expected the unpair confirmation dialog")

        // Cancel the dialog. On iOS 26 an inline confirmationDialog is
        // rendered as an anchored popover that DROPS the explicit
        // `Button("Cancel", role: .cancel)` — canceling is tapping outside,
        // exposed as the "PopoverDismissRegion" element. Older renderings
        // (full-width action sheet) keep a real Cancel button; support both.
        let cancelButton = app.buttons["Cancel"]
        if cancelButton.waitForExistence(timeout: 2) {
            cancelButton.tap()
        } else {
            let dismissRegion = element(app, "PopoverDismissRegion")
            XCTAssertTrue(dismissRegion.exists, "Expected either a Cancel button or a popover dismiss region")
            dismissRegion.tap()
        }

        XCTAssertTrue(element(app, "SettingsView").waitForExistence(timeout: 5), "Expected to remain on Settings after canceling unpair")
        XCTAssertFalse(element(app, "PairingView").exists)
    }
}
