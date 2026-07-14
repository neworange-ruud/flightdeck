//
//  GitStatusPresentation.swift
//  FlightDeckRemote
//
//  Pure wire → display mapping for the read-only git status screen (PRD
//  §5.5: "view a session's branch, changed files, ahead/behind, base,
//  drift — read-only, frictionless"). Kept separate from `GitStatusView` so
//  the mapping (ahead/behind formatting, drift, empty/clean state, the
//  changed-files list) is unit-testable without SwiftUI — mirrors
//  `GitIndicatorMapping`'s split between wire type and display, one level up
//  (full detail rather than the compact row indicator).
//

import Foundation

enum GitStatusPresentation {

    /// One row in the changed-files list.
    struct FileRow: Identifiable, Hashable {
        var id: String { path }
        var path: String
        var status: Wire.GitFileStatus
        var addedLines: UInt32
        var removedLines: UInt32
    }

    /// Everything the view renders, already formatted from a
    /// `Wire.GitStatusDetail`.
    struct Rows: Equatable {
        var branch: String
        var baseBranch: String
        var aheadBehindText: String
        /// `nil` when there's no drift from base to call out.
        var driftText: String?
        var files: [FileRow]
        /// True when there are no uncommitted file changes (mirrors
        /// `Wire.GitIndicators.isClean`, at the full-detail level).
        var isClean: Bool
    }

    /// Maps a full `Wire.GitStatusDetail` onto display `Rows`.
    static func present(_ detail: Wire.GitStatusDetail) -> Rows {
        Rows(
            branch: detail.branch ?? "(detached HEAD)",
            baseBranch: detail.baseBranch ?? "—",
            aheadBehindText: aheadBehindText(ahead: detail.ahead, behind: detail.behind,
                                             hasUpstream: detail.hasUpstream),
            driftText: driftText(detail.drift),
            files: detail.files.map {
                FileRow(path: $0.path, status: $0.status,
                       addedLines: $0.addedLines, removedLines: $0.removedLines)
            },
            isClean: detail.files.isEmpty
        )
    }

    /// `"no upstream"` when the branch has none (ahead/behind is moot);
    /// `"up to date"` when neither ahead nor behind; otherwise `"N ahead"` /
    /// `"N behind"` joined with " · ".
    static func aheadBehindText(ahead: UInt32, behind: UInt32, hasUpstream: Bool) -> String {
        guard hasUpstream else { return "no upstream" }
        guard ahead != 0 || behind != 0 else { return "up to date" }
        var parts: [String] = []
        if ahead > 0 { parts.append("\(ahead) ahead") }
        if behind > 0 { parts.append("\(behind) behind") }
        return parts.joined(separator: " · ")
    }

    /// `nil` when there's no drift (nothing to show); otherwise a
    /// human-readable "N commit(s) behind base" sentence.
    static func driftText(_ drift: UInt32) -> String? {
        guard drift > 0 else { return nil }
        return "\(drift) commit\(drift == 1 ? "" : "s") behind base"
    }

    /// Single-letter glyph for a changed-file's status (Added / Modified /
    /// Deleted / Renamed / Untracked).
    static func shortLabel(for status: Wire.GitFileStatus) -> String {
        switch status {
        case .added: "A"
        case .modified: "M"
        case .deleted: "D"
        case .renamed: "R"
        case .untracked: "U"
        }
    }
}
