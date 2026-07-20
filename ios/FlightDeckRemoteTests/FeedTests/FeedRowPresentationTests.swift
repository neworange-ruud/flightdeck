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

    // MARK: - Event-aware accent (remote-control-fa8)

    @Test func liveErrorRowAccentsRed() {
        let item = FeedItemFixtures.item(
            pairingId: "A", projectId: "p", isOnline: true,
            latestEvent: FeedItemFixtures.event(project: "p", atMs: 1, kind: .error(message: "boom")))
        #expect(FeedRowPresentation.accentColor(item: item) == Theme.statusRed)
    }

    @Test func liveNeedsInputEventRowAccentsOrange() {
        let item = FeedItemFixtures.item(
            pairingId: "A", projectId: "p", isOnline: true,
            latestEvent: FeedItemFixtures.event(project: "p", atMs: 1, kind: .needsInput(preview: "?")))
        #expect(FeedRowPresentation.accentColor(item: item) == RollupModel.color(for: .needsInput))
    }

    @Test func liveNeedsInputRollupRowAccentsOrangeEvenWithoutEvent() {
        let item = FeedItemFixtures.item(pairingId: "A", projectId: "p", dot: .needsInput, isOnline: true)
        #expect(FeedRowPresentation.accentColor(item: item) == RollupModel.color(for: .needsInput))
    }

    @Test func offlineErrorRowIsNeverAccented() {
        let item = FeedItemFixtures.item(
            pairingId: "A", projectId: "p", isOnline: false,
            latestEvent: FeedItemFixtures.event(project: "p", atMs: 1, kind: .error(message: "boom")))
        #expect(FeedRowPresentation.accentColor(item: item) == nil)
    }

    @Test func calmRowHasNoAccent() {
        let item = FeedItemFixtures.item(
            pairingId: "A", projectId: "p", isOnline: true,
            latestEvent: FeedItemFixtures.event(project: "p", atMs: 1,
                                                kind: .finished(summary: "done", filesChanged: 0, readyToPush: false)))
        #expect(FeedRowPresentation.accentColor(item: item) == nil)
    }

    // MARK: - Event-derived summary

    @Test func summaryUsesTheEventMessageWhenPresent() {
        let item = FeedItemFixtures.item(
            pairingId: "A", projectId: "p", isOnline: true,
            latestEvent: FeedItemFixtures.event(project: "p", atMs: 1, kind: .needsInput(preview: "which env?")))
        #expect(FeedRowPresentation.summaryText(item: item, rollupSummary: "1 agent") == "which env?")
    }

    @Test func summaryFallsBackToRollupWhenNoEvent() {
        let item = FeedItemFixtures.item(pairingId: "A", projectId: "p", isOnline: true)
        #expect(FeedRowPresentation.summaryText(item: item, rollupSummary: "idle · 1 agent") == "idle · 1 agent")
    }

    @Test func errorSummaryIsRed() {
        let errItem = FeedItemFixtures.item(
            pairingId: "A", projectId: "p", isOnline: true,
            latestEvent: FeedItemFixtures.event(project: "p", atMs: 1, kind: .error(message: "boom")))
        #expect(FeedRowPresentation.summaryColor(item: errItem) == Theme.statusRed)

        let calmItem = FeedItemFixtures.item(pairingId: "A", projectId: "q", isOnline: true)
        #expect(FeedRowPresentation.summaryColor(item: calmItem) == Theme.textMuted)
    }
}
