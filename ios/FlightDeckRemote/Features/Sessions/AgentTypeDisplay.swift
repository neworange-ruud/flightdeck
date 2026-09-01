//
//  AgentTypeDisplay.swift
//  FlightDeckRemote
//
//  Plain-language labels for `Wire.AgentType` (PRD §5.2: "agent type (Claude
//  Code/OpenCode/Codex/Cursor)"). An additive extension in our own file — the
//  enum itself lives in Transport/Protocol/Common.swift (read-only consume).
//
//  `.unknown` is a backend a newer desktop reported that this build does not
//  know by name; the row still needs a label, so it gets the generic one.
//

import Foundation

extension Wire.AgentType {
    /// Display name for the session row's agent-type label.
    var displayName: String {
        switch self {
        case .claudeCode: "Claude Code"
        case .opencode: "OpenCode"
        case .codex: "Codex"
        case .cursor: "Cursor"
        case .unknown: "Agent"
        }
    }
}
