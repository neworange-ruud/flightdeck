//
//  RollupModel.swift
//  FlightDeckRemote
//
//  Pure client-side status roll-up (PRD §4): folds a project's sessions into
//  the single dot + plain-language summary shown on a project row.
//
//  Precedence (PRD §4 / spec §9, high to low):
//      needs_input > working > manual > idle
//  — orange if ANY agent needs input; else red if any is working; manual is
//  shown in the counts/summary but never outranks needs-input or working;
//  else green/dim idle.
//
//  The desktop already ships an authoritative `StatusRollup` (dot + summary +
//  counts) inside snapshots and `rollup` updates — richer summaries like
//  "1 done, ready to push" only come from there, since the desktop knows git
//  state. When a desktop rollup is available, use it verbatim
//  (`ProjectRollupViewModel(rollup:)` / `viewModel(for:)`); compute locally
//  from sessions only as a fallback (stale rollup after `status_update`
//  deltas) and for previews.
//
//  Models/logic only — no screens here.
//

import SwiftUI

/// Everything a project row needs to render its roll-up: the dominant dot,
/// its design-token color, the plain-language summary, and per-state counts.
struct ProjectRollupViewModel: Equatable {
    /// Which status dominates the dot.
    let dot: Wire.RollupDot
    /// Plain-language summary, e.g. `1 needs input · 1 working · 3 agents`.
    let summary: String
    /// Number of agents needing input.
    let needsInput: UInt32
    /// Number of agents working.
    let working: UInt32
    /// Number of agents under manual override.
    let manual: UInt32
    /// Number of agents idle/finished.
    let idle: UInt32
    /// Total agent count.
    let agentCount: UInt32

    /// The design token color for the dot (PRD §4 status palette).
    var dotColor: Color { RollupModel.color(for: dot) }

    /// Build from a desktop-provided rollup (authoritative; preferred).
    /// Keeps the desktop's summary verbatim — it can carry hints the client
    /// cannot derive, e.g. `1 done, ready to push`.
    init(rollup: Wire.StatusRollup) {
        dot = rollup.dot
        summary = rollup.summary
        needsInput = rollup.needsInput
        working = rollup.working
        manual = rollup.manual
        idle = rollup.idle
        agentCount = rollup.agentCount
    }

    init(dot: Wire.RollupDot, summary: String, needsInput: UInt32,
         working: UInt32, manual: UInt32, idle: UInt32, agentCount: UInt32) {
        self.dot = dot
        self.summary = summary
        self.needsInput = needsInput
        self.working = working
        self.manual = manual
        self.idle = idle
        self.agentCount = agentCount
    }
}

/// Pure functions that fold session states into a project roll-up.
enum RollupModel {

    /// Fold a project's sessions into a roll-up view model, applying the PRD
    /// precedence (needs-input > working > manual > idle) and the
    /// plain-language summary patterns. Local fallback — prefer
    /// `viewModel(for:)` when a desktop rollup is present.
    static func rollup(sessions: [Wire.SessionState]) -> ProjectRollupViewModel {
        var needsInput: UInt32 = 0
        var working: UInt32 = 0
        var manual: UInt32 = 0
        var idle: UInt32 = 0

        for session in sessions {
            switch session.status {
            case .needsInput: needsInput += 1
            case .working: working += 1
            case .manual: manual += 1
            case .idle: idle += 1
            }
        }

        let agentCount = UInt32(sessions.count)
        return ProjectRollupViewModel(
            dot: dot(needsInput: needsInput, working: working, manual: manual),
            summary: summary(needsInput: needsInput, working: working,
                             manual: manual, agentCount: agentCount),
            needsInput: needsInput,
            working: working,
            manual: manual,
            idle: idle,
            agentCount: agentCount)
    }

    /// The roll-up for a project state: uses the desktop's authoritative
    /// `rollup` (dot + summary + counts) as-is.
    static func viewModel(for project: Wire.ProjectState) -> ProjectRollupViewModel {
        ProjectRollupViewModel(rollup: project.rollup)
    }

    /// Which status dominates the dot. Precedence, high to low:
    /// needs-input > working > manual > idle. Manual is counted but never
    /// outranks needs-input or working.
    static func dot(needsInput: UInt32, working: UInt32,
                    manual: UInt32) -> Wire.RollupDot {
        if needsInput > 0 { return .needsInput }
        if working > 0 { return .working }
        if manual > 0 { return .manual }
        return .idle
    }

    /// Map a roll-up dot to its design token color (PRD §4).
    static func color(for dot: Wire.RollupDot) -> Color {
        switch dot {
        case .needsInput: Theme.statusNeedsInput
        case .working: Theme.statusWorking
        case .manual: Theme.statusManual
        case .idle: Theme.statusIdle
        }
    }

    /// Plain-language summary for a project row (PRD §4 patterns):
    ///
    /// * `1 needs input · 1 working · 3 agents` — active segments (needs
    ///   input, working, manual — in that order, zeroes dropped) followed by
    ///   the total agent count;
    /// * `idle · 2 agents` — everything idle/done;
    /// * single-session form drops the redundant count: `1 needs input`,
    ///   `1 working`, `1 manual`, `idle · 1 agent`;
    /// * `no agents` — empty project.
    ///
    /// (Richer hints like `1 done, ready to push` come from the desktop's
    /// `StatusRollup.summary` and are used verbatim, never composed here.)
    static func summary(needsInput: UInt32, working: UInt32, manual: UInt32,
                        agentCount: UInt32) -> String {
        guard agentCount > 0 else { return "no agents" }

        var segments: [String] = []
        if needsInput > 0 { segments.append("\(needsInput) needs input") }
        if working > 0 { segments.append("\(working) working") }
        if manual > 0 { segments.append("\(manual) manual") }

        guard !segments.isEmpty else {
            return "idle · \(agentLabel(agentCount))"
        }
        if agentCount == 1 {
            // A lone agent: "1 working · 1 agent" would be redundant.
            return segments[0]
        }
        segments.append(agentLabel(agentCount))
        return segments.joined(separator: " · ")
    }

    private static func agentLabel(_ count: UInt32) -> String {
        count == 1 ? "1 agent" : "\(count) agents"
    }
}
