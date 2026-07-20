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
}
