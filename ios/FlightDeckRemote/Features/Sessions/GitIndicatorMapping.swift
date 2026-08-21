//
//  GitIndicatorMapping.swift
//  FlightDeckRemote
//
//  Maps the wire `Wire.GitIndicators` (Transport/Protocol/Common.swift) onto
//  the DesignSystem's `GitIndicatorText.Kind` (PRD §5.2/§11: compact Geist
//  Mono indicators — `~3 drift:2`, `+12 ~4`, `clean`, `no-upstream`).
//
//  An additive extension in our own file — `GitIndicatorText` itself is
//  DesignSystem's (read-only consume).
//

import Foundation

extension GitIndicatorText.Kind {
    /// `no-upstream` wins when the branch has none (drift/dirty state is
    /// moot until it has somewhere to compare against); otherwise `clean`
    /// when there's neither an uncommitted diff nor base drift, else the
    /// compact diff form (with `drift:N` appended when present).
    static func from(_ git: Wire.GitIndicators) -> GitIndicatorText.Kind {
        guard git.hasUpstream else { return .noUpstream }
        guard !git.isClean || git.drift > 0 else { return .clean }
        return .diff(
            added: Int(git.added),
            modified: Int(git.modified),
            deleted: Int(git.removed),
            drift: Int(git.drift)
        )
    }
}
