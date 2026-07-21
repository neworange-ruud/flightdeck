//
//  FocusModePresentation.swift
//  FlightDeckRemote
//
//  Pure, view-agnostic presentation model for the eyes-free focus mode (PRD
//  §5.3 3b). It decides WHAT to pin (the current pending permission ask) and
//  condenses the transcript history into a short timeline ("Ran npm test — 42
//  passed 9:39", "Wants to clean the build output — now"), so the condensation
//  rules are unit-tested without a view.
//
//  No SwiftUI / transport dependency — it only reads `Wire.TranscriptItem`
//  values, so `FocusMode.presentation(...)` is a straight value transform.
//

import Foundation

/// One condensed line in the focus-mode history timeline.
struct FocusTimelineEntry: Identifiable, Equatable, Sendable {
    /// The source item's id (stable for `ForEach`).
    let id: String
    /// The condensed one-line label ("Ran npm test — 42 passed").
    let text: String
    /// A short clock label ("9:39"), or "now" for the current pending ask.
    let timeLabel: String
    /// Whether this is the current pending question (pinned + "now").
    let isPending: Bool
}

/// What the focus-mode screen shows: the pinned pending ask (if any) plus the
/// condensed history timeline.
struct FocusModePresentation: Equatable, Sendable {
    /// The current pending permission command to pin large (e.g. "rm -rf dist/").
    let pendingCommand: String?
    /// The current pending prompt id (drives Approve/Deny), if any.
    let pendingPromptId: Wire.PromptId?
    /// The pending prompt's decision options (Allow once / Deny).
    let options: [Wire.PermissionOption]
    /// The condensed history, oldest → newest, ending with the pending "now".
    let timeline: [FocusTimelineEntry]

    /// Whether there is a live question to pin (else the screen is read-only).
    var hasPending: Bool { pendingPromptId != nil }
}

/// Builder + condensation rules for the focus-mode presentation.
enum FocusMode {

    /// How many recent items the condensed timeline keeps.
    static let maxTimelineEntries = 6

    /// Max characters of agent prose kept in a condensed line before an ellipsis.
    static let proseCharBudget = 64

    /// Build the presentation from the transcript and the current pending id.
    static func presentation(items: [Wire.TranscriptItem],
                             currentPending: Wire.PromptId?) -> FocusModePresentation {
        let pendingItem = items.last(where: { $0.permissionPromptId == currentPending
            && currentPending != nil })

        var command: String?
        var options: [Wire.PermissionOption] = []
        if case let .permissionPrompt(_, _, _, cmd, opts, _, _)? = pendingItem {
            command = cmd
            options = opts
        }

        // Keep the most recent slice, condense each, and mark the pending one.
        let recent = items.suffix(maxTimelineEntries)
        let timeline = recent.map { item -> FocusTimelineEntry in
            let isPending = item.permissionPromptId != nil
                && item.permissionPromptId == currentPending
            return FocusTimelineEntry(
                id: item.itemId.rawValue,
                text: condense(item),
                timeLabel: isPending ? "now" : clockLabel(item.atMs),
                isPending: isPending)
        }

        return FocusModePresentation(
            pendingCommand: command,
            pendingPromptId: currentPending == nil ? nil
                : pendingItem?.permissionPromptId,
            options: options,
            timeline: timeline)
    }

    /// Condense one transcript item into a single timeline line.
    static func condense(_ item: Wire.TranscriptItem) -> String {
        switch item {
        case let .userMessage(_, text, _):
            return "You: \(firstLine(text, budget: proseCharBudget))"
        case let .agentMessage(_, text, _):
            // Agent prose is Markdown; strip syntax so the condensed one-line
            // peek reads cleanly instead of showing raw `**` / `#` / backticks.
            return firstLine(ChatMarkdown.plainText(text), budget: proseCharBudget)
        case let .activity(_, summary, _, _, _, _):
            return summary
        case let .permissionPrompt(_, _, _, command, _, _, _):
            return "Wants to run \(command)"
        }
    }

    /// A short clock label ("9:39") for a timeline entry.
    static func clockLabel(_ atMs: Int64) -> String {
        let date = Date(timeIntervalSince1970: Double(atMs) / 1000)
        let formatter = DateFormatter()
        formatter.dateFormat = "H:mm"
        return formatter.string(from: date)
    }

    /// The first sentence / line of prose, truncated to `budget` chars.
    static func firstLine(_ text: String, budget: Int) -> String {
        let collapsed = text
            .replacingOccurrences(of: "\n", with: " ")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        // Prefer a sentence boundary if it lands within budget.
        if let dot = collapsed.firstIndex(of: "."),
           collapsed.distance(from: collapsed.startIndex, to: dot) <= budget {
            return String(collapsed[..<dot])
        }
        if collapsed.count <= budget { return collapsed }
        let end = collapsed.index(collapsed.startIndex, offsetBy: budget)
        return String(collapsed[..<end]).trimmingCharacters(in: .whitespaces) + "…"
    }
}
