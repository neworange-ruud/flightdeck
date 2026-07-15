//
//  RemoteLiveE2EUITests.swift
//  FlightDeckRemoteUITests
//
//  Tier B full-stack E2E (see the remote-control-c3m epic / plan
//  "kind-jingling-plum.md", "Tier B — Full-stack simulator E2E"). Unlike the
//  other UITest files — which pin `MockPairingService` via `-uitest…`
//  arguments and pair against deterministic in-app fakes — this test pairs the
//  REAL iOS app LIVE to a real local relay + real desktop that the
//  `scripts/e2e/run-fullstack.sh` orchestrator stands up around the run.
//
//  It forces the real pairing service with `FLIGHTDECK_PAIRING=real` and feeds
//  the relay URL to the phone the ONLY way the app accepts it — via the
//  `fdr1:` QR payload — by pasting the orchestrator-supplied payload into the
//  DEBUG QR field (there is no camera in the simulator). Landing on
//  `MainTabView` proves a true round trip: real phone <-> real relay <-> real
//  desktop.
//
//  ENVIRONMENT (delivered by the orchestrator to the test-RUNNER process;
//  xcodebuild strips the `TEST_RUNNER_` prefix before the test sees them, so
//  the names read here are the un-prefixed `FLIGHTDECK_*` forms — matched
//  field-for-field to `scripts/e2e/run-fullstack.sh::run_xcuitest`):
//    FLIGHTDECK_E2E_FDR1  the `fdr1:` pairing payload (relay URL + claim token
//                         + pairing secret). REQUIRED — absent means the test
//                         was run standalone (plain `xcodebuild test`) without
//                         the harness, so it XCTSkips gracefully.
//    FLIGHTDECK_PAIRING   forwarded into the app so `PairingServiceFactory`
//                         selects `RealPairingService`. Defaults to "real"
//                         here if the runner didn't set it.
//
//  CRITICAL: the app is launched with NO `-uitest…` arguments. Any such
//  argument forces `MockPairingService` (see `PairingServiceFactory`), which
//  would clobber the real relay-backed pairing this test exists to verify.
//
//  EXTENSION POINT (remote-control-c3m.10): the capability flows — chat reply,
//  new agent, shell, git — are a SEPARATE follow-up issue that EXTENDS this
//  same file. They should be added as an `extension RemoteLiveE2EUITests` (or
//  additional test methods) that reuse `launchAndPairLive(_:)` to reach
//  `MainTabView`, then drive the real UI and assert the phone reflects real
//  desktop state. Keep the shared launch/pairing/skip helpers below as the
//  single entry point so every capability test starts from an identically
//  paired session.
//

import XCTest

final class RemoteLiveE2EUITests: XCTestCase {

    // MARK: - Environment keys (mirrors run-fullstack.sh::run_xcuitest)

    /// The `fdr1:` pairing payload the orchestrator constructs and hands to the
    /// test runner. Read from the runner's own process environment (NOT the
    /// app's) so it can be forwarded into `app.launchEnvironment`.
    private static let payloadEnvKey = "FLIGHTDECK_E2E_FDR1"

    /// Forces `RealPairingService` in the app. Forwarded into the app env.
    private static let pairingModeEnvKey = "FLIGHTDECK_PAIRING"

    /// Real pairing over the relay + E2E derivation takes a moment; give the
    /// live round trip a generous landing budget.
    private static let liveLandingTimeout: TimeInterval = 30

    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    // MARK: - Shared helpers (reused by c3m.10 capability flows)

    /// Looks up an element by accessibility identifier regardless of its
    /// underlying `XCUIElement` type (same helper as the other UITest files —
    /// SwiftUI containers don't always map to the type a naive
    /// `app.otherElements[...]` query would expect).
    func element(_ app: XCUIApplication, _ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    /// Reads the `fdr1:` payload from the RUNNER's environment, skipping the
    /// whole test cleanly when it is absent (standalone `xcodebuild test`
    /// without the orchestrator). This keeps the file compiling and behaving
    /// sanely outside the harness while running for real under
    /// `run-fullstack.sh`.
    func requireLivePayload() throws -> String {
        let env = ProcessInfo.processInfo.environment
        guard let payload = env[Self.payloadEnvKey], !payload.isEmpty else {
            throw XCTSkip(
                """
                Skipping live E2E: \(Self.payloadEnvKey) not set. This test pairs \
                the real app to a live local relay + desktop and only runs under \
                scripts/e2e/run-fullstack.sh (which supplies the fdr1: payload via \
                TEST_RUNNER_\(Self.payloadEnvKey)). Run it through that orchestrator.
                """
            )
        }
        return payload
    }

    /// Launches the REAL app (no `-uitest…` args), forwards the real-pairing
    /// switch + `fdr1:` payload into `launchEnvironment`, pastes the payload
    /// into the DEBUG QR field, and returns once the app has landed on
    /// `MainTabView` — i.e. paired to the real local relay + desktop.
    ///
    /// c3m.10 capability tests should call this first, then drive the real UI.
    @discardableResult
    func launchAndPairLive(_ app: XCUIApplication) throws -> XCUIApplication {
        let payload = try requireLivePayload()

        // Force the real pairing service and hand the phone the relay URL via
        // the fdr1: payload. Default the mode to "real" if the runner omitted
        // it (the payload's presence already means we're under the harness).
        let runnerEnv = ProcessInfo.processInfo.environment
        app.launchEnvironment[Self.pairingModeEnvKey] =
            runnerEnv[Self.pairingModeEnvKey] ?? "real"
        app.launchEnvironment[Self.payloadEnvKey] = payload

        // NO `-uitest…` arguments: those force MockPairingService and would
        // clobber the real relay-backed service this test verifies.
        app.launch()

        // Pairing screen → open the QR scanner (the DEBUG paste field lives on
        // the scanner, always shown in the simulator since there's no camera).
        let scanButton = element(app, "scan-qr-button")
        XCTAssertTrue(
            scanButton.waitForExistence(timeout: 10),
            "Expected the 'Scan QR instead' button on the real Pairing screen"
        )
        scanButton.tap()

        // Paste the fdr1: payload into the DEBUG field and redeem it.
        let field = element(app, "qr-scanner-debug-payload-field")
        XCTAssertTrue(
            field.waitForExistence(timeout: 10),
            "Expected the DEBUG QR payload field on the scanner screen"
        )
        field.tap()
        field.typeText(payload)

        let usePayloadButton = element(app, "qr-scanner-debug-use-payload-button")
        XCTAssertTrue(usePayloadButton.waitForExistence(timeout: 5))
        usePayloadButton.tap()

        // Landing on MainTabView proves the live pairing completed against the
        // real relay + desktop (real handshake + E2E derivation).
        XCTAssertTrue(
            element(app, "MainTabView").waitForExistence(timeout: Self.liveLandingTimeout),
            "Expected the real app to pair live and reveal the main tab container"
        )
        XCTAssertFalse(
            element(app, "PairingView").exists,
            "Should have left the Pairing screen after a successful live pairing"
        )
        return app
    }

    // MARK: - Tests

    /// End-to-end: the real app pairs live to the local relay + desktop by
    /// pasting the orchestrator's `fdr1:` payload, and lands on `MainTabView`.
    @MainActor
    func testLivePairingReachesMainTabView() throws {
        let app = XCUIApplication()
        try launchAndPairLive(app)
        // `launchAndPairLive` already asserts the MainTabView landing; reaching
        // here means the live round trip succeeded.
    }
}
