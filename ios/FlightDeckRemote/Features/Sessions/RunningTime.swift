//
//  RunningTime.swift
//  FlightDeckRemote
//
//  Formats a session's `running_time_secs` for the session row (PRD §5.2):
//  `1h 12m` / `8m` / `just started`. Pure and unit-tested.
//

import Foundation

enum RunningTime {
    /// Below this many seconds a session just kicked off — showing seconds
    /// would be noisy and not actionable, so the row reads `just started`.
    static let justStartedThresholdSecs: UInt64 = 60

    /// Format a running-time duration for display on a session row.
    static func format(seconds: UInt64) -> String {
        guard seconds >= justStartedThresholdSecs else { return "just started" }

        let totalMinutes = seconds / 60
        let hours = totalMinutes / 60
        let minutes = totalMinutes % 60

        guard hours > 0 else { return "\(minutes)m" }
        return "\(hours)h \(minutes)m"
    }
}
