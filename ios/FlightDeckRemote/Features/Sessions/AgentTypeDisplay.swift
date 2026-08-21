//
//  AgentTypeDisplay.swift
//  FlightDeckRemote
//
//  Plain-language labels for `Wire.AgentType` (PRD §5.2: "agent type (Claude
//  Code/OpenCode/Codex)"). An additive extension in our own file — the enum
//  itself lives in Transport/Protocol/Common.swift (read-only consume).
//

import Foundation

extension Wire.AgentType {
    /// Display name for the session row's agent-type label.
    var displayName: String {
        switch self {
        case .claudeCode: "Claude Code"
        case .opencode: "OpenCode"
        case .codex: "Codex"
        }
    }
}
