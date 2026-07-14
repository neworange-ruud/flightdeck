//
//  ProjectsSessionsUITests.swift
//  FlightDeckRemoteUITests
//
//  Exercises the Projects list + Agent sessions list (PRD §5.2) against the
//  `-uitest-fixture-snapshot` DEBUG fixture (`Wire.StateSnapshot.uiTestFixture`,
//  seeded via `TransportStore.debugSeed`), so these screens render real
//  content deterministically without a live desktop:
//   - the Projects list renders one card per fixture project;
//   - tapping a card navigates to its sessions list;
//   - a session row shows its status pill, git indicators, and (for the
//     needs-input session) its pending-question preview;
//   - tapping a session pushes the Chat placeholder;
//   - the "New agent session" CTA opens the same sheet as the FAB.
//

import XCTest

final class ProjectsSessionsUITests: XCTestCase {

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    /// Looks up an element by accessibility identifier regardless of its
    /// underlying XCUIElement type (mirrors the other UI test suites'
    /// helper — SwiftUI containers don't always map to the type a naive
    /// `app.otherElements[...]` query would expect).
    private func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Launches unpaired (reset), pairs via the DEBUG toggle, and seeds the
    /// fixture snapshot so the Projects/Sessions screens have fixture data
    /// to render without a live desktop.
    private func launchPairedWithFixture(_ app: XCUIApplication) {
        app.launchArguments += ["-uitest-reset-pairing", "-uitest-fixture-snapshot"]
        app.launch()
        let toggle = element(app, "debug-toggle-paired-button")
        XCTAssertTrue(toggle.waitForExistence(timeout: 5), "Expected the DEBUG pairing toggle on the Pairing screen")
        toggle.tap()
    }

    @MainActor
    func testProjectsListRendersFixtureProjectCards() throws {
        let app = XCUIApplication()
        launchPairedWithFixture(app)

        XCTAssertTrue(element(app, "ProjectsListView").waitForExistence(timeout: 5))
        XCTAssertTrue(element(app, "project-card-proj_flightdeck").waitForExistence(timeout: 5))
        XCTAssertTrue(element(app, "project-card-proj_remote_control").exists)
        XCTAssertTrue(element(app, "project-card-proj_marketing_site").exists)
    }

    @MainActor
    func testTappingProjectCardNavigatesToSessions() throws {
        let app = XCUIApplication()
        launchPairedWithFixture(app)

        let card = element(app, "project-card-proj_flightdeck")
        XCTAssertTrue(card.waitForExistence(timeout: 5))
        card.tap()

        XCTAssertTrue(element(app, "SessionsListView").waitForExistence(timeout: 5))
        XCTAssertTrue(element(app, "session-card-sess_fix_login").waitForExistence(timeout: 5))
    }

    @MainActor
    func testSessionsShowStatusPillsGitIndicatorsAndPendingQuestionPreview() throws {
        let app = XCUIApplication()
        launchPairedWithFixture(app)

        element(app, "project-card-proj_flightdeck").tap()
        XCTAssertTrue(element(app, "SessionsListView").waitForExistence(timeout: 5))

        XCTAssertTrue(element(app, "session-card-sess_fix_login").waitForExistence(timeout: 5))
        XCTAssertTrue(element(app, "status-pill-needs-you").exists, "Expected the needs-input session's pill")
        XCTAssertTrue(element(app, "git-indicator-text").exists, "Expected compact git indicators on session rows")
        XCTAssertTrue(element(app, "session-pending-question-sess_fix_login").exists,
                      "Expected the waiting agent's question preview")
    }

    @MainActor
    func testTappingSessionPushesChatPlaceholder() throws {
        let app = XCUIApplication()
        launchPairedWithFixture(app)

        element(app, "project-card-proj_flightdeck").tap()
        XCTAssertTrue(element(app, "SessionsListView").waitForExistence(timeout: 5))

        let sessionCard = element(app, "session-card-sess_fix_login")
        XCTAssertTrue(sessionCard.waitForExistence(timeout: 5))
        sessionCard.tap()
        XCTAssertTrue(element(app, "AgentChatView").waitForExistence(timeout: 5))
    }

    @MainActor
    func testNewAgentCTAOpensTheSharedSheet() throws {
        let app = XCUIApplication()
        launchPairedWithFixture(app)

        element(app, "project-card-proj_flightdeck").tap()
        XCTAssertTrue(element(app, "SessionsListView").waitForExistence(timeout: 5))

        let cta = element(app, "new-agent-session-cta")
        XCTAssertTrue(cta.waitForExistence(timeout: 5))
        cta.tap()
        XCTAssertTrue(element(app, "NewAgentPlaceholderSheet").waitForExistence(timeout: 5))
    }

    @MainActor
    func testSearchTogglesFieldAndFiltersByName() throws {
        let app = XCUIApplication()
        launchPairedWithFixture(app)

        XCTAssertTrue(element(app, "ProjectsListView").waitForExistence(timeout: 5))
        element(app, "projects-search-toggle").tap()

        let searchField = element(app, "projects-search-field")
        XCTAssertTrue(searchField.waitForExistence(timeout: 5))
        searchField.tap()
        searchField.typeText("marketing")

        XCTAssertTrue(element(app, "project-card-proj_marketing_site").waitForExistence(timeout: 5))
        XCTAssertFalse(element(app, "project-card-proj_flightdeck").exists)
    }
}
