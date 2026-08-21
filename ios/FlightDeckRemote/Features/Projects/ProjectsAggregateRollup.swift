//
//  ProjectsAggregateRollup.swift
//  FlightDeckRemote
//
//  The Projects list header subtitle (PRD §5.2): a roll-up ACROSS projects,
//  e.g. "1 project needs you · 2 working" — distinct from `RollupModel`'s
//  per-project agent-level summary. Counts how many projects' dominant dot
//  (`Wire.ProjectState.rollup.dot`) is each status, using the same
//  precedence order (needs-input, working, manual, then idle).
//
//  An additive extension in our own file — `RollupModel` itself
//  (Features/Monitor) is read-only consume.
//

import Foundation

extension RollupModel {
    /// Aggregate subtitle for the Projects list header.
    ///
    /// * `1 project needs you · 2 working` — active segments, zeroes
    ///   dropped; only the leading segment says "project(s)", matching the
    ///   PRD example verbatim.
    /// * `3 projects · idle` — everything idle.
    /// * `1 project · idle` — single idle project.
    /// * `No projects yet` — no projects at all.
    static func aggregateSubtitle(projects: [Wire.ProjectState]) -> String {
        guard !projects.isEmpty else { return "No projects yet" }

        var needsInput = 0
        var working = 0
        var manual = 0
        var idle = 0
        for project in projects {
            switch project.rollup.dot {
            case .needsInput: needsInput += 1
            case .working: working += 1
            case .manual: manual += 1
            case .idle: idle += 1
            }
        }

        var segments: [String] = []
        if needsInput > 0 {
            segments.append(needsInput == 1 ? "1 project needs you" : "\(needsInput) projects need you")
        }
        if working > 0 {
            segments.append(working == 1 ? "1 working" : "\(working) working")
        }
        if manual > 0 {
            segments.append(manual == 1 ? "1 manual" : "\(manual) manual")
        }

        guard !segments.isEmpty else {
            return idle == 1 ? "1 project · idle" : "\(idle) projects · idle"
        }
        return segments.joined(separator: " · ")
    }
}
