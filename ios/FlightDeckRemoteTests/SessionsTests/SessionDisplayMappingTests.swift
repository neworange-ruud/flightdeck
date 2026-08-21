//
//  SessionDisplayMappingTests.swift
//  FlightDeckRemoteTests
//
//  Covers the small display-mapping extensions the session row uses:
//  `Wire.AgentType.displayName` ("Claude Code"/"OpenCode"/"Codex") and
//  `Wire.AgentStatus.agentStatus` (the DesignSystem `AgentStatus` a status
//  dot/pill renders).
//

import Testing
@testable import FlightDeckRemote

struct SessionDisplayMappingTests {

    @Test func agentTypeDisplayNames() {
        #expect(Wire.AgentType.claudeCode.displayName == "Claude Code")
        #expect(Wire.AgentType.opencode.displayName == "OpenCode")
        #expect(Wire.AgentType.codex.displayName == "Codex")
    }

    @Test func agentStatusMapsEveryCase() {
        #expect(Wire.AgentStatus.working.agentStatus == .working)
        #expect(Wire.AgentStatus.idle.agentStatus == .idle)
        #expect(Wire.AgentStatus.needsInput.agentStatus == .needsInput)
        #expect(Wire.AgentStatus.manual(label: "reviewing").agentStatus == .manual(label: "reviewing"))
    }
}
