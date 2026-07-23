//
//  ChatTranscriptLogic.swift
//  FlightDeckRemote
//
//  Pure, view-agnostic logic for the cleaned agent-chat transcript (PRD §5.3
//  3a). Everything here is a value transform with no SwiftUI dependency so it
//  can be unit-tested directly:
//
//   - `shouldAutoScroll` — the standard-chat auto-scroll decision (stick to
//     the bottom on append *unless* the user has scrolled up).
//   - `shouldShowTimestamp` — sparse timestamps: show only on the first item
//     or after a long enough gap, so a burst of activity doesn't get one
//     stamp per line.
//   - `shouldLoadEarlier` — whether the "load earlier" affordance is offered
//     (the loaded window doesn't start at the transcript's head).
//   - `rows(for:)` — folds a flat `[Wire.TranscriptItem]` into render rows,
//     annotating which rows carry a timestamp divider.
//
//  The convenience accessors on `Wire.TranscriptItem` (`itemId`, `atMs`) are
//  additive extensions — the wire enum's associated values aren't otherwise
//  reachable without a full pattern-match at every call site.
//

import Foundation

/// Namespace for the transcript's pure presentation logic.
enum ChatTranscript {

    /// Default gap after which a fresh timestamp divider is shown (5 min).
    static let timestampGapMs: Int64 = 5 * 60 * 1000

    /// Standard chat auto-scroll rule: on a new item, stick to the bottom
    /// *unless* the user has deliberately scrolled up and is not near the
    /// bottom. `isNearBottom` wins so that scrolling back down re-arms
    /// auto-follow even if `userScrolled` is still latched.
    static func shouldAutoScroll(isNearBottom: Bool, userScrolled: Bool) -> Bool {
        !userScrolled || isNearBottom
    }

    /// Sparse-timestamp rule: show a divider for the first item, or when the
    /// gap since the previous item is at least `gapMs`.
    static func shouldShowTimestamp(previousAtMs: Int64?,
                                    currentAtMs: Int64,
                                    gapMs: Int64 = timestampGapMs) -> Bool {
        guard let previousAtMs else { return true }
        return currentAtMs - previousAtMs >= gapMs
    }

    /// Whether to offer the "load earlier" affordance: the loaded window's
    /// first item is not the head of the transcript (`from_index > 0`).
    static func shouldLoadEarlier(fromIndex: UInt64) -> Bool {
        fromIndex > 0
    }

    /// Folds transcript items into render rows, marking which rows show a
    /// timestamp divider (sparse grouping).
    static func rows(for items: [Wire.TranscriptItem],
                     gapMs: Int64 = timestampGapMs) -> [ChatRow] {
        var out: [ChatRow] = []
        out.reserveCapacity(items.count)
        var previous: Int64?
        for (index, item) in items.enumerated() {
            let show = shouldShowTimestamp(previousAtMs: previous,
                                           currentAtMs: item.atMs, gapMs: gapMs)
            out.append(ChatRow(index: index, item: item, showsTimestamp: show))
            previous = item.atMs
        }
        return out
    }
}

/// One rendered transcript row: the item, its stable index within the loaded
/// window (drives `pill-<index>` identifiers and scroll targets), and whether
/// a timestamp divider precedes it.
struct ChatRow: Identifiable, Equatable {
    let index: Int
    let item: Wire.TranscriptItem
    let showsTimestamp: Bool

    /// Stable across re-folds: the item's own id keeps `ForEach`/scroll
    /// targeting stable even when earlier items are prepended.
    var id: String { item.itemId.rawValue }
}

extension Wire.TranscriptItem {

    /// The item's transcript id (present on every variant).
    var itemId: Wire.ItemId {
        switch self {
        case let .userMessage(itemId, _, _): itemId
        case let .agentMessage(itemId, _, _): itemId
        case let .activity(itemId, _, _, _, _, _): itemId
        case let .permissionPrompt(itemId, _, _, _, _, _, _, _, _): itemId
        }
    }

    /// The item's wall-clock timestamp (unix ms).
    var atMs: Int64 {
        switch self {
        case let .userMessage(_, _, atMs): atMs
        case let .agentMessage(_, _, atMs): atMs
        case let .activity(_, _, _, _, _, atMs): atMs
        case let .permissionPrompt(_, _, _, _, _, _, _, _, atMs): atMs
        }
    }

    /// The pending permission-prompt id, if this item is a prompt.
    var permissionPromptId: Wire.PromptId? {
        if case let .permissionPrompt(_, promptId, _, _, _, _, _, _, _) = self { return promptId }
        return nil
    }
}
