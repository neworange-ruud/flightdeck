//
//  PairedLaunchSupport.swift
//  FlightDeckRemoteUITests
//
//  Shared helper for the "launch unpaired, then cross into the paired tab
//  container via the DEBUG toggle" step that nearly every UI test starts with.
//
//  It exists because that step had a real race (remote-control-7lo): the toggle
//  can be present in the accessibility tree a beat before UIKit will actually
//  deliver a touch to it, and a tap that lands too early is silently swallowed —
//  the app stays on the unpaired screen and every later assertion fails with a
//  confusing "expected the chat screen" rather than "the toggle tap was lost".
//  Waiting for hittability and retrying the tap once turns that flake into a
//  deterministic step.
//

import XCTest

extension XCTestCase {

    /// Tap the DEBUG "already paired" toggle and wait for `screenIdentifier` to
    /// appear, retrying the tap once if the first one was swallowed.
    ///
    /// - Parameters:
    ///   - app: the launched application.
    ///   - screenIdentifier: the accessibility identifier of the screen expected
    ///     to appear after crossing into the paired container (e.g.
    ///     `"AgentChatView"` for the fixture-transcript routes, which auto-push).
    ///   - timeout: total budget for reaching `screenIdentifier`. Deliberately
    ///     generous: the full UI suite runs dozens of app launches back to back
    ///     (and `ShellUITests` can starve the simulator for minutes), so a tight
    ///     budget here fails on machine load rather than on a real defect.
    func crossIntoPairedApp(_ app: XCUIApplication,
                            expecting screenIdentifier: String,
                            timeout: TimeInterval = 40,
                            file: StaticString = #filePath,
                            line: UInt = #line) {
        let toggle = app.descendants(matching: .any)
            .matching(identifier: "debug-toggle-paired-button").firstMatch
        XCTAssertTrue(toggle.waitForExistence(timeout: timeout),
                      "Expected the DEBUG pairing toggle", file: file, line: line)
        // Existence is not touchability: tapping before the view is hittable is
        // one of the two flakes this helper exists to remove.
        waitForHittable(toggle, timeout: timeout, file: file, line: line)
        toggle.tap()

        // The two failure modes need opposite responses, so poll in short slices
        // and decide from the live state each time:
        //  - toggle still on screen ⇒ the tap was swallowed; tap again.
        //  - toggle gone ⇒ we already crossed and the destination route is merely
        //    slow to push; keep waiting rather than failing.
        let screen = app.descendants(matching: .any)
            .matching(identifier: screenIdentifier).firstMatch
        let deadline = Date().addingTimeInterval(timeout)
        var retaps = 0
        while Date() < deadline {
            let slice = min(5, max(1, deadline.timeIntervalSinceNow))
            if screen.waitForExistence(timeout: slice) { return }
            if toggle.exists, retaps < 2 {
                retaps += 1
                toggle.tap()
            }
        }
        XCTFail("Expected \(screenIdentifier) within \(timeout)s of crossing into the "
                + "paired app; pairing toggle still present = \(toggle.exists), "
                + "retaps = \(retaps)",
                file: file, line: line)
    }

    /// Block until `element` reports `isHittable`, failing after `timeout`.
    func waitForHittable(_ element: XCUIElement,
                         timeout: TimeInterval = 10,
                         file: StaticString = #filePath,
                         line: UInt = #line) {
        let hittable = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "isHittable == true"), object: element)
        let result = XCTWaiter().wait(for: [hittable], timeout: timeout)
        XCTAssertEqual(result, .completed,
                       "Element never became hittable within \(timeout)s: \(element)",
                       file: file, line: line)
    }
}
