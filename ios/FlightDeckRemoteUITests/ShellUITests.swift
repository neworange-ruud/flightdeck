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
//   - disconnected: the paused note shows and the key bar is disabled;
//   - the key bar's keys stay hittable above the home indicator (PRD §5.4
//     hittability follow-up), in both portrait and landscape (PRD §5.4 4b);
//   - the font-size toolbar button exists and cycles without disturbing the
//     live terminal (the pure cycling/clamping rule is unit-tested in
//     `ShellFontSizeTests`, not re-derived here).
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

    /// PRD §5.4 hittability follow-up: the key bar is mounted via
    /// `.safeAreaInset(edge: .bottom)` (see `ShellView`) so it always sits
    /// above the home indicator (and, in the Shell tab, the custom bottom tab
    /// bar) — every key must exist *and* be hittable, not just present in the
    /// accessibility tree.
    @MainActor
    func testKeyBarKeysAreAllHittableAbovetheHomeIndicator() throws {
        let app = XCUIApplication()
        launchShell(app)
        XCTAssertTrue(element(app, "shell-key-bar").waitForExistence(timeout: 5))

        for key in ["esc", "tab", "ctrl", "left", "up", "down", "right",
                    "pipe", "slash", "dash", "tilde", "backtick", "interrupt", "paste"] {
            let el = element(app, "shell-key-\(key)")
            XCTAssertTrue(el.exists, "Missing key bar key: \(key)")
            XCTAssertTrue(el.isHittable, "Key bar key not hittable: \(key)")
        }
    }

    /// PRD §5.4 4b (landscape): the terminal gets the extra width and the key
    /// bar — Ctrl, the full symbol run, and paste — stays laid out (no
    /// truncation) and hittable above the home indicator once rotated.
    @MainActor
    func testLandscapeKeyBarStaysHittableAndUntruncated() throws {
        let app = XCUIApplication()
        launchShell(app)
        XCTAssertTrue(element(app, "shell-terminal").waitForExistence(timeout: 5))

        XCUIDevice.shared.orientation = .landscapeLeft
        defer { XCUIDevice.shared.orientation = .portrait }

        XCTAssertTrue(element(app, "shell-terminal").waitForExistence(timeout: 5),
                      "Expected the terminal to still render after rotating to landscape")
        for key in ["esc", "tab", "ctrl", "left", "up", "down", "right",
                    "pipe", "slash", "dash", "tilde", "backtick", "interrupt", "paste"] {
            let el = element(app, "shell-key-\(key)")
            XCTAssertTrue(el.exists, "Missing key bar key in landscape: \(key)")
            XCTAssertTrue(el.isHittable, "Key bar key not hittable in landscape: \(key)")
        }
    }

    /// PRD §5.4 font-size control: the toolbar's "font" button exists and can
    /// be cycled through its full ladder without disturbing the live
    /// terminal. The pure cycling/clamping rule itself is unit-tested in
    /// `ShellFontSizeTests` — this just proves the button is wired up.
    @MainActor
    func testFontSizeButtonCyclesWithoutDisturbingTheTerminal() throws {
        let app = XCUIApplication()
        launchShell(app)
        let fontButton = element(app, "shell-font-size")
        XCTAssertTrue(fontButton.waitForExistence(timeout: 5))
        XCTAssertTrue(fontButton.isHittable, "Font-size button should be hittable in the terminal toolbar")

        // Cycle a couple of steps — enough to prove the button is wired to a
        // live cycle without disturbing the terminal. (The full 5-step ladder
        // + wrap-around is exhaustively covered in `ShellFontSizeTests`; the
        // fixture shell's animated "working" spinner makes each interaction
        // here expensive, so we keep the tap count low.)
        fontButton.tap()
        fontButton.tap()
        XCTAssertTrue(element(app, "shell-terminal").exists,
                      "Terminal should still be live after cycling font size")
    }
}
