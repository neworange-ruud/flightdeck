//
//  ShellUITests.swift
//  FlightDeckRemoteUITests
//
//  Exercises the minimal shell terminal (PRD §5.4) against the DEBUG
//  `-uitest-fixture-shell` seam (a scripted live shell with an ANSI-coloured
//  line + a scripted sender), so the surface renders and sends deterministically
//  without a live desktop:
//   - the Shell tab lands on a live terminal + key bar with all keys;
//   - key-bar taps send bytes (asserted via the `shell-debug-last-sent` seam);
//   - the dedicated Ctrl-C button fires the interrupt *command*;
//   - sticky `Ctrl` arms (visible) then composes into a control byte;
//   - the paste button is present;
//   - disconnected: the paused note shows and the key bar is disabled.
//

import XCTest

final class ShellUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Launches paired, seeds the fixture snapshot (for the session list) and
    /// the scripted shell, then opens the Shell tab.
    private func launchShell(_ app: XCUIApplication, extraArgs: [String] = []) {
        app.launchArguments += ["-uitest-reset-pairing", "-uitest-fixture-snapshot",
                                "-uitest-fixture-shell"] + extraArgs
        app.launch()
        let toggle = element(app, "debug-toggle-paired-button")
        XCTAssertTrue(toggle.waitForExistence(timeout: 5), "Expected the DEBUG pairing toggle")
        toggle.tap()
        element(app, "tab-shell").tap()
    }

    @MainActor
    func testShellTabRendersLiveTerminalAndKeyBar() throws {
        let app = XCUIApplication()
        launchShell(app)

        XCTAssertTrue(element(app, "ShellView").waitForExistence(timeout: 5))
        XCTAssertTrue(element(app, "shell-terminal").waitForExistence(timeout: 5),
                      "Expected the live terminal renderer")
        XCTAssertTrue(element(app, "shell-key-bar").exists, "Expected the accessory key bar")

        // Every key bar key is present (PRD §5.4).
        for key in ["esc", "tab", "ctrl", "left", "up", "down", "right",
                    "pipe", "slash", "dash", "tilde", "backtick", "interrupt", "paste"] {
            XCTAssertTrue(element(app, "shell-key-\(key)").exists, "Missing key bar key: \(key)")
        }
    }

    @MainActor
    func testKeyBarTabSendsHorizontalTab() throws {
        let app = XCUIApplication()
        launchShell(app)
        XCTAssertTrue(element(app, "shell-key-tab").waitForExistence(timeout: 5))
        element(app, "shell-key-tab").tap()
        let sent = element(app, "shell-debug-last-sent")
        XCTAssertEqual(sent.label, "input:09", "Tab should send HT (0x09)")
    }

    @MainActor
    func testDedicatedCtrlCSendsInterruptCommand() throws {
        let app = XCUIApplication()
        launchShell(app)
        XCTAssertTrue(element(app, "shell-key-interrupt").waitForExistence(timeout: 5))
        element(app, "shell-key-interrupt").tap()
        XCTAssertEqual(element(app, "shell-debug-last-sent").label, "interrupt",
                       "The dedicated Ctrl-C button sends the interrupt command")
    }

    @MainActor
    func testStickyCtrlArmsThenComposesControlByte() throws {
        let app = XCUIApplication()
        launchShell(app)
        let ctrl = element(app, "shell-key-ctrl")
        XCTAssertTrue(ctrl.waitForExistence(timeout: 5))

        // Tap Ctrl → armed (sticky), reflected as selected.
        ctrl.tap()
        XCTAssertTrue(ctrl.isSelected, "Ctrl should be lit/selected once armed")

        // Then a key composes into a control byte and disarms Ctrl.
        // Ctrl + '/' (0x2f) → 0x0f.
        element(app, "shell-key-slash").tap()
        XCTAssertEqual(element(app, "shell-debug-last-sent").label, "input:0f",
                       "Sticky Ctrl should compose the next key into a control byte")
        XCTAssertFalse(ctrl.isSelected, "Ctrl disarms after composing")
    }

    @MainActor
    func testDisconnectedDisablesInput() throws {
        let app = XCUIApplication()
        launchShell(app, extraArgs: ["-uitest-linkstate", "disconnected"])

        XCTAssertTrue(element(app, "shell-paused-note").waitForExistence(timeout: 5),
                      "Expected the paused/reconnecting note when the link is down")
        // The key bar is disabled while paused (nothing sent blind, PRD §8).
        XCTAssertFalse(element(app, "shell-key-tab").isEnabled,
                       "Key bar keys should be disabled while the link is down")
    }
}
