//
//  FocusModePresentationTests.swift
//  FlightDeckRemoteTests
//
//  Unit tests for the eyes-free focus-mode presentation model (PRD §5.3 3b):
//  what to pin (the current pending permission ask) and how the history
//  condenses into a short timeline ("Ran npm test · 42 passed 9:39", pending →
//  "now").
//

import Testing
import Foundation
@testable import FlightDeckRemote

@Suite struct FocusModePresentationTests {

    private let base: Int64 = 1_720_000_000_000

    private func fixture() -> [Wire.TranscriptItem] { ChatFixtures.items() }

    // MARK: - Pinning the pending ask

    @Test func pinsCurrentPendingCommandAndOptions() {
        let items = fixture()
        let pending = Wire.PromptId("fx-prompt-1")
        let p = FocusMode.presentation(items: items, currentPending: pending)

        #expect(p.hasPending)
        #expect(p.pendingCommand == "rm -rf dist/")
        #expect(p.pendingPromptId == pending)
        #expect(p.options.count == 2)
        #expect(p.options.first?.choice == .allowOnce)
    }

    @Test func noPendingWhenNoneCurrent() {
        let items = fixture()
        let p = FocusMode.presentation(items: items, currentPending: nil)
        #expect(!p.hasPending)
        #expect(p.pendingCommand == nil)
        #expect(p.options.isEmpty)
    }

    // MARK: - Timeline condensation

    @Test func timelineIsCappedToMaxEntries() {
        let items = fixture()   // 6 items
        let p = FocusMode.presentation(items: items, currentPending: Wire.PromptId("fx-prompt-1"))
        #expect(p.timeline.count <= FocusMode.maxTimelineEntries)
    }

    @Test func pendingEntryLabelledNow() {
        let items = fixture()
        let p = FocusMode.presentation(items: items, currentPending: Wire.PromptId("fx-prompt-1"))
        let pendingEntry = p.timeline.first(where: { $0.isPending })
        #expect(pendingEntry != nil)
        #expect(pendingEntry?.timeLabel == "now")
    }

    @Test func nonPendingEntriesUseClockLabels() {
        let items = fixture()
        let p = FocusMode.presentation(items: items, currentPending: Wire.PromptId("fx-prompt-1"))
        let others = p.timeline.filter { !$0.isPending }
        #expect(!others.isEmpty)
        // Clock labels are H:mm — never the literal "now".
        #expect(others.allSatisfy { $0.timeLabel != "now" && $0.timeLabel.contains(":") })
    }

    @Test func activityCondensesToItsSummary() {
        let entry = FocusMode.condense(.activity(
            itemId: Wire.ItemId("a"), summary: "Ran npm test · 42 passed",
            detail: "…lots of noise…", body: nil, kind: .test, atMs: base))
        #expect(entry == "Ran npm test · 42 passed")
    }

    @Test func agentProseCondensesToFirstSentence() {
        let entry = FocusMode.condense(.agentMessage(
            itemId: Wire.ItemId("m"),
            text: "Tests pass and the redirect now keeps the return URL. Before I rebuild I want to clear the stale build output.",
            atMs: base))
        #expect(entry == "Tests pass and the redirect now keeps the return URL")
    }

    @Test func longProseWithoutSentenceBreakIsTruncated() {
        let long = String(repeating: "x", count: 200)
        let entry = FocusMode.condense(.agentMessage(itemId: Wire.ItemId("m"), text: long, atMs: base))
        #expect(entry.count <= FocusMode.proseCharBudget + 1) // + ellipsis
        #expect(entry.hasSuffix("…"))
    }

    @Test func userMessageIsPrefixed() {
        let entry = FocusMode.condense(.userMessage(
            itemId: Wire.ItemId("u"), text: "ship it", atMs: base))
        #expect(entry == "You: ship it")
    }

    @Test func permissionPromptCondensesToWantsToRun() {
        let entry = FocusMode.condense(.permissionPrompt(
            itemId: Wire.ItemId("p"), promptId: Wire.PromptId("p1"), kind: .permission,
            command: "rm -rf dist/", options: [], allowFreeText: false, multiSelect: false, atMs: base))
        #expect(entry == "Wants to run rm -rf dist/")
    }
}
