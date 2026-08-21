//
//  FaceIDRowPresentationTests.swift
//  FlightDeckRemoteTests
//
//  Verifies `FaceIDRowPresentation` — the pure availability-annotation logic
//  behind Settings' "Require Face ID to open" row (SettingsView.swift) —
//  without instantiating the view. Mirrors `ConnectionLatencyPhrase`'s
//  static/pure test pattern in ConnectionIndicatorTests.
//

import Testing
@testable import FlightDeckRemote

struct FaceIDRowPresentationTests {

    @Test func toggleIsEnabledAndNoFootnoteWhenBiometricsAvailable() {
        let presentation = FaceIDRowPresentation.make(canEvaluateBiometrics: true)
        #expect(presentation.isToggleEnabled == true)
        #expect(presentation.footnote == nil)
    }

    @Test func toggleIsDisabledWithFootnoteWhenNoAuthenticationAvailable() {
        let presentation = FaceIDRowPresentation.make(canEvaluateBiometrics: false)
        #expect(presentation.isToggleEnabled == false)
        #expect(presentation.footnote == "No device authentication available")
    }
}
