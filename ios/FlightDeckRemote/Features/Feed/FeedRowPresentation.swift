//
//  FeedRowPresentation.swift
//  FlightDeckRemote
//
//  Pure per-row presentation decisions for the unified feed (remote-control-
//  b8d.8), factored out of `FeedView` so they're unit-testable without
//  instantiating SwiftUI (mirrors `FaceIDRowPresentation` in SettingsView.swift
//  and `RollupModel`'s own pure-function shape).
//

import SwiftUI

enum FeedRowPresentation {

    /// Content opacity for a feed row: dimmed (but still legible) for an
    /// offline item, full brightness otherwise (PRD/issue: "offline items
    /// dimmed"). The row's retry affordance is a SIBLING of the dimmed
    /// content in `FeedView` (not inside it), so it stays fully legible even
    /// while the row around it is dimmed.
    static let offlineOpacity: Double = 0.55
    static let onlineOpacity: Double = 1.0

    static func contentOpacity(isOffline: Bool) -> Double {
        isOffline ? offlineOpacity : onlineOpacity
    }

    /// The row's card left-accent color (the colored bar `CardStyle` draws):
    /// only for a LIVE row whose dominant status is needs-input. An offline
    /// row is already dimmed + badged — layering a bright "needs input"
    /// accent on top of a dimmed, last-known-state row would overstate how
    /// urgent/current that state actually is, so offline never accents.
    static func accentColor(dot: Wire.RollupDot, isOffline: Bool) -> Color? {
        guard !isOffline, dot == .needsInput else { return nil }
        return RollupModel.color(for: dot)
    }

    /// The full row accent (remote-control-fa8), folding the latest event in:
    /// a LIVE error row accents red; a LIVE needs-input row (live roll-up dot
    /// OR a needs-input event) accents orange; everything else — and every
    /// offline row (dimmed + badged instead) — draws no accent.
    static func accentColor(item: FeedItem) -> Color? {
        guard !item.isOffline else { return nil }
        if item.isErrorEvent { return Theme.statusRed }
        if item.project.rollup.dot == .needsInput || item.isNeedsInputEvent {
            return RollupModel.color(for: .needsInput)
        }
        return nil
    }

    /// The event-derived summary line: the latest event's needs-input preview /
    /// finished summary / error message (reusing the Activity feed's pure
    /// `ActivityCellMapper.message` formatting), or `nil` when the project has
    /// produced no event so the caller falls back to the roll-up summary.
    static func eventSummary(for item: FeedItem) -> String? {
        item.latestEvent.map { ActivityCellMapper.message(for: $0.kind) }
    }

    /// The summary text a row shows: the event-derived line when present, else
    /// the passed-in roll-up summary.
    static func summaryText(item: FeedItem, rollupSummary: String) -> String {
        eventSummary(for: item) ?? rollupSummary
    }

    /// The summary text color: red for a latest-event error, muted otherwise
    /// (offline rows are further dimmed by `contentOpacity`).
    static func summaryColor(item: FeedItem) -> Color {
        item.isErrorEvent ? Theme.statusRed : Theme.textMuted
    }
}
