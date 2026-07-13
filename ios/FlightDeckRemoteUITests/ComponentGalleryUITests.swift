//
//  ComponentGalleryUITests.swift
//  FlightDeckRemoteUITests
//
//  Navigates to the DesignSystem's ComponentGallery (via the DEBUG-only
//  floating launcher button wired up in FlightDeckRemoteApp) and asserts a
//  representative set of component identifiers render.
//

import XCTest

final class ComponentGalleryUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    /// Looks up an element by accessibility identifier regardless of its
    /// underlying XCUIElement type (SwiftUI shapes/containers don't always
    /// map to the type a naive `app.otherElements[...]` query would expect).
    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    @MainActor
    func testGalleryRendersComponentsFromDebugLauncher() throws {
        let app = XCUIApplication()
        app.launch()

        let launcher = element(app, "component-gallery-launcher")
        XCTAssertTrue(launcher.waitForExistence(timeout: 5), "Expected the DEBUG gallery launcher button to appear")
        launcher.tap()

        let gallery = element(app, "component-gallery")
        XCTAssertTrue(gallery.waitForExistence(timeout: 5), "Expected the ComponentGallery to appear after tapping the launcher")

        // A representative sample across the component set — not exhaustive.
        XCTAssertTrue(element(app, "status-dot-working").waitForExistence(timeout: 2))
        XCTAssertTrue(element(app, "status-dot-needs-input").exists)
        XCTAssertTrue(element(app, "working-spinner").exists)
        XCTAssertTrue(element(app, "status-pill-working").exists)
        XCTAssertTrue(element(app, "status-pill-needs-you").exists)
        XCTAssertTrue(element(app, "git-indicator-text").exists)
        XCTAssertTrue(element(app, "notification-cell-needs-input").exists)
        XCTAssertTrue(element(app, "notification-cell-finished").exists)
    }
}
