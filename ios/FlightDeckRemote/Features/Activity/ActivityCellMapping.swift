//
//  ActivityCellMapping.swift
//  FlightDeckRemote
//
//  Pure `Wire.AgentEvent` → text mapping: the message composition per
//  `EventKind` (needs-input preview / finished summary + files-changed / error
//  message) and coarse relative-time formatting. Originally the Activity tab's
//  cell mapper; after that tab was folded into the unified Feed
//  (remote-control-fa8) the `message(for:)` half is reused by
//  `FeedRowPresentation.eventSummary` to render a feed row's event-derived
//  summary line. Kept pure and free of SwiftUI/Theme-adjacent formatting
//  decisions so the mapping is unit-testable without a view host.
//

import Foundation

/// Which visual variant an event renders as.
enum ActivityCellKind: Equatable {
    case needsInput
    case finished
    case error
}

/// Everything a cell needs to render, pre-computed from an event + the
/// project name looked up against the live/cached snapshot.
struct ActivityCellViewModel: Identifiable, Equatable {
    var id: Wire.EventId
    var kind: ActivityCellKind
    var title: String
    var message: String
    var projectTag: String
    var occurredAtMs: Int64
}

enum ActivityCellMapper {
    /// Builds the view-model for `event`. `projectName` is the display name
    /// to show in the project tag — pass the looked-up name from the
    /// snapshot, or the raw project id string as an honest fallback when the
    /// project isn't known (a deep-linked-but-closed project).
    static func viewModel(
        for event: Wire.AgentEvent,
        projectName: String,
        nowMs: Int64
    ) -> ActivityCellViewModel {
        ActivityCellViewModel(
            id: event.eventId,
            kind: kind(for: event.kind),
            title: event.title,
            message: message(for: event.kind),
            projectTag: "\(projectName) · \(relativeTimeString(fromMs: event.occurredAtMs, nowMs: nowMs))",
            occurredAtMs: event.occurredAtMs
        )
    }

    static func kind(for eventKind: Wire.EventKind) -> ActivityCellKind {
        switch eventKind {
        case .needsInput: .needsInput
        case .finished: .finished
        case .error: .error
        }
    }

    /// The cell's body text: the needs-input preview / error message
    /// verbatim, or the finished summary augmented with the files-changed
    /// count and a "ready to push" note when applicable.
    static func message(for eventKind: Wire.EventKind) -> String {
        switch eventKind {
        case let .needsInput(preview):
            return preview
        case let .finished(summary, filesChanged, readyToPush):
            var parts = [summary]
            if filesChanged > 0 {
                parts.append("\(filesChanged) file\(filesChanged == 1 ? "" : "s") changed")
            }
            if readyToPush {
                parts.append("ready to push")
            }
            return parts.joined(separator: " · ")
        case let .error(message):
            return message
        }
    }

    /// A short, coarse-grained relative time string ("just now", "5m ago",
    /// "3h ago", "2d ago"). Deliberately simple/coarse rather than pulling in
    /// `RelativeDateTimeFormatter`'s locale-aware phrasing — the feed only
    /// needs a rough sense of recency, and a hand-rolled formatter keeps this
    /// pure and trivially unit-testable.
    static func relativeTimeString(fromMs: Int64, nowMs: Int64) -> String {
        let deltaMs = max(0, nowMs - fromMs)
        let seconds = deltaMs / 1000
        if seconds < 60 { return "just now" }
        let minutes = seconds / 60
        if minutes < 60 { return "\(minutes)m ago" }
        let hours = minutes / 60
        if hours < 24 { return "\(hours)h ago" }
        let days = hours / 24
        return "\(days)d ago"
    }
}
