//
//  AgentStatusDisplay.swift
//  FlightDeckRemote
//
//  Maps the wire `Wire.AgentStatus` (Transport/Protocol/Common.swift) onto
//  the DesignSystem's `AgentStatus` enum, so a session row can drive
//  `StatusDot`/`StatusPill`/`WorkingSpinner` directly. An additive extension
//  in our own file — both enums are elsewhere and read-only consume.
//

import Foundation

extension Wire.AgentStatus {
    /// The DesignSystem `AgentStatus` this wire status renders as.
    var agentStatus: AgentStatus {
        switch self {
        case .working: .working
        case .idle: .idle
        case .needsInput: .needsInput
        case .manual(let label): .manual(label: label)
        }
    }
}
