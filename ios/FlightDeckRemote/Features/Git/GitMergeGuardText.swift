//
//  GitMergeGuardText.swift
//  FlightDeckRemote
//
//  "Guarded merge-back" (PRD §5.5: "guarded merge-back... confirmation-gated").
//  Surfaces whatever guard info the session's latest known git status
//  (`Wire.GitStatusDetail`, folded onto `TransportStore.gitStatus` from the
//  desktop's passively-pushed `git_status` frame) provides — uncommitted
//  changes and/or drift from base — as an extra warning line in the
//  merge-back confirmation sheet, alongside the standard confirmation copy
//  (`SessionControlAction.confirmation`, Features/Control). The wire protocol
//  has no separate "conflict check" frame to request; this surfaces the
//  read-only status the desktop already pushes, honestly, rather than
//  inventing a prediction the app can't back up.
//
//  Pure mapping — no view/store coupling — so it's unit-testable directly.
//

import Foundation

enum GitMergeGuardText {
    /// `nil` when there's no known git status yet, or nothing to warn about
    /// (a clean working tree with no drift) — the merge-back confirmation
    /// then shows just the standard copy, with no extra guard line.
    static func build(from detail: Wire.GitStatusDetail?) -> String? {
        guard let detail else { return nil }
        var parts: [String] = []
        if !detail.files.isEmpty {
            parts.append("\(detail.files.count) uncommitted change\(detail.files.count == 1 ? "" : "s")")
        }
        if detail.drift > 0 {
            parts.append("\(detail.drift) commit\(detail.drift == 1 ? "" : "s") of drift from base")
        }
        guard !parts.isEmpty else { return nil }
        return "Heads up: \(parts.joined(separator: " and ")). The merge may conflict."
    }
}
