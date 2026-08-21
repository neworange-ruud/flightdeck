//
//  SessionSort.swift
//  FlightDeckRemote
//
//  Session ordering for the Agent sessions list (PRD §5.2): needs-input
//  first (most urgent), then working, then manual, then idle — mirroring
//  `RollupModel`'s dot precedence. Ties keep their original (server-sent)
//  relative order (`sorted` is stable). Pure and unit-tested.
//

import Foundation

enum SessionSort {
    /// Precedence rank, low = shown first.
    private static func rank(_ status: Wire.AgentStatus) -> Int {
        switch status {
        case .needsInput: 0
        case .working: 1
        case .manual: 2
        case .idle: 3
        }
    }

    /// Sort sessions needs-input > working > manual > idle.
    static func sorted(_ sessions: [Wire.SessionState]) -> [Wire.SessionState] {
        sessions.enumerated()
            .sorted { lhs, rhs in
                let l = rank(lhs.element.status)
                let r = rank(rhs.element.status)
                if l != r { return l < r }
                return lhs.offset < rhs.offset // stable tie-break
            }
            .map(\.element)
    }
}
