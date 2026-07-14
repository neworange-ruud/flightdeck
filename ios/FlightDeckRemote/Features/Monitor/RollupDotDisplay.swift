//
//  RollupDotDisplay.swift
//  FlightDeckRemote
//
//  Maps the wire `Wire.RollupDot` (Transport/Protocol/Common.swift) onto the
//  DesignSystem's `AgentStatus` enum, so a project roll-up can drive
//  `StatusDot`/`StatusPill` directly. An additive extension in our own file
//  — both enums are elsewhere and read-only consume.
//

import Foundation

extension Wire.RollupDot {
    /// The `AgentStatus` a project card's `StatusDot` should render for this
    /// dominant dot. `manual` carries no per-project label at the roll-up
    /// level, so it uses the generic "manual" label.
    var agentStatus: AgentStatus {
        switch self {
        case .needsInput: .needsInput
        case .working: .working
        case .manual: .manual()
        case .idle: .idle
        }
    }
}
