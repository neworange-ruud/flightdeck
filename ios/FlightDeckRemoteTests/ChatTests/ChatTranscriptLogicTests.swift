//
//  ChatTranscriptLogicTests.swift
//  FlightDeckRemoteTests
//
//  Unit tests for the pure transcript logic (`ChatTranscript`): the
//  auto-scroll decision, sparse timestamp grouping, pagination gating, and the
//  row fold.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@Suite struct ChatTranscriptLogicTests {

    // MARK: - Auto-scroll decision

    @Test func autoScrollWhenAtBottomAndNotScrolled() {
        #expect(ChatTranscript.shouldAutoScroll(isNearBottom: true, userScrolled: false))
    }

    @Test func autoScrollSuppressedWhenUserScrolledUp() {
        #expect(ChatTranscript.shouldAutoScroll(isNearBottom: false, userScrolled: true) == false)
    }

    @Test func autoScrollReArmsWhenScrolledBackToBottom() {
        // Near-bottom wins even if `userScrolled` is still latched true.
        #expect(ChatTranscript.shouldAutoScroll(isNearBottom: true, userScrolled: true))
    }

    @Test func autoScrollWhenNotScrolledEvenIfNotNearBottom() {
        #expect(ChatTranscript.shouldAutoScroll(isNearBottom: false, userScrolled: false))
    }

    // MARK: - Timestamp grouping

    @Test func firstItemAlwaysShowsTimestamp() {
        #expect(ChatTranscript.shouldShowTimestamp(previousAtMs: nil, currentAtMs: 1_000))
    }

    @Test func closeItemsShareOneTimestamp() {
        #expect(ChatTranscript.shouldShowTimestamp(previousAtMs: 1_000,
                                                   currentAtMs: 1_000 + 30_000) == false)
    }

    @Test func longGapShowsFreshTimestamp() {
        let gap = ChatTranscript.timestampGapMs
        #expect(ChatTranscript.shouldShowTimestamp(previousAtMs: 1_000,
                                                   currentAtMs: 1_000 + gap))
    }

    // MARK: - Pagination gating

    @Test func loadEarlierOnlyWhenWindowNotAtHead() {
        #expect(ChatTranscript.shouldLoadEarlier(fromIndex: 0) == false)
        #expect(ChatTranscript.shouldLoadEarlier(fromIndex: 1))
        #expect(ChatTranscript.shouldLoadEarlier(fromIndex: 42))
    }

    // MARK: - Row fold

    private func msg(_ id: String, _ atMs: Int64) -> Wire.TranscriptItem {
        .agentMessage(itemId: Wire.ItemId(id), text: "t", atMs: atMs)
    }

    @Test func rowsAnnotateSparseTimestamps() {
        let gap = ChatTranscript.timestampGapMs
        let items = [
            msg("a", 0),           // first → timestamp
            msg("b", 10_000),      // close → no timestamp
            msg("c", 10_000 + gap) // long gap → timestamp
        ]
        let rows = ChatTranscript.rows(for: items)
        #expect(rows.count == 3)
        #expect(rows[0].showsTimestamp)
        #expect(rows[1].showsTimestamp == false)
        #expect(rows[2].showsTimestamp)
        // Index + stable id preserved.
        #expect(rows[0].index == 0)
        #expect(rows[2].id == "c")
    }

    @Test func rowsEmptyForNoItems() {
        #expect(ChatTranscript.rows(for: []).isEmpty)
    }

    // MARK: - Item accessors

    @Test func itemAccessorsCoverEveryVariant() {
        let perm = Wire.TranscriptItem.permissionPrompt(
            itemId: Wire.ItemId("p"), promptId: Wire.PromptId("prom"), kind: .permission,
            command: "ls", options: [], allowFreeText: false, multiSelect: false, atMs: 7)
        #expect(perm.itemId == Wire.ItemId("p"))
        #expect(perm.atMs == 7)
        #expect(perm.permissionPromptId == Wire.PromptId("prom"))

        let activity = Wire.TranscriptItem.activity(
            itemId: Wire.ItemId("act"), summary: "s", detail: nil, body: nil,
            kind: .test, atMs: 9)
        #expect(activity.itemId == Wire.ItemId("act"))
        #expect(activity.permissionPromptId == nil)
    }
}
