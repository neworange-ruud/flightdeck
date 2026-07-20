//
//  FeedRowPresentationTests.swift
//  FlightDeckRemoteTests
//
//  Covers the unified feed's pure per-row presentation decisions (remote-
//  control-b8d.8): offline dimming opacity, and that a needs-input accent
//  only ever surfaces for a LIVE row (an offline row is dimmed + badged
//  instead, never accented, regardless of its dominant status).
//

import Testing
import SwiftUI
@testable import FlightDeckRemote

@Suite struct FeedRowPresentationTests {

    // MARK: - Opacity

    @Test func onlineRowIsFullBrightness() {
        #expect(FeedRowPresentation.contentOpacity(isOffline: false) == FeedRowPresentation.onlineOpacity)
    }

    @Test func offlineRowIsDimmed() {
        #expect(FeedRowPresentation.contentOpacity(isOffline: true) == FeedRowPresentation.offlineOpacity)
        #expect(FeedRowPresentation.offlineOpacity < FeedRowPresentation.onlineOpacity)
    }

    // MARK: - Accent color

    @Test func onlineNeedsInputRowIsAccented() {
        #expect(FeedRowPresentation.accentColor(dot: .needsInput, isOffline: false) == RollupModel.color(for: .needsInput))
    }

    @Test func offlineNeedsInputRowIsNeverAccented() {
        #expect(FeedRowPresentation.accentColor(dot: .needsInput, isOffline: true) == nil)
    }

    @Test func onlineNonNeedsInputRowsAreNeverAccented() {
        #expect(FeedRowPresentation.accentColor(dot: .working, isOffline: false) == nil)
        #expect(FeedRowPresentation.accentColor(dot: .manual, isOffline: false) == nil)
        #expect(FeedRowPresentation.accentColor(dot: .idle, isOffline: false) == nil)
    }

    @Test func offlineRowsAreNeverAccentedRegardlessOfDot() {
        #expect(FeedRowPresentation.accentColor(dot: .working, isOffline: true) == nil)
        #expect(FeedRowPresentation.accentColor(dot: .manual, isOffline: true) == nil)
        #expect(FeedRowPresentation.accentColor(dot: .idle, isOffline: true) == nil)
    }
}
