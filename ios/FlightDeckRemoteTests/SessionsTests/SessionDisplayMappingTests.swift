//
//  SessionDisplayMappingTests.swift
//  FlightDeckRemoteTests
//
//  Covers the small display-mapping extensions the session row uses:
//  `Wire.AgentType.displayName` ("Claude Code"/"OpenCode"/"Codex"/"Cursor") and
//  `Wire.AgentStatus.agentStatus` (the DesignSystem `AgentStatus` a status
//  dot/pill renders), plus `AgentType`'s lenient decoding.
//

import Foundation
import Testing
@testable import FlightDeckRemote

struct SessionDisplayMappingTests {

    @Test func agentTypeDisplayNames() {
        #expect(Wire.AgentType.claudeCode.displayName == "Claude Code")
        #expect(Wire.AgentType.opencode.displayName == "OpenCode")
        #expect(Wire.AgentType.codex.displayName == "Codex")
        #expect(Wire.AgentType.cursor.displayName == "Cursor")
        #expect(Wire.AgentType.unknown.displayName == "Agent")
    }

    @Test func agentTypeDecodesEveryKnownWireValue() throws {
        for (wire, expected): (String, Wire.AgentType) in [
            ("claude_code", .claudeCode),
            ("opencode", .opencode),
            ("codex", .codex),
            ("cursor", .cursor),
        ] {
            let data = Data("\"\(wire)\"".utf8)
            #expect(try JSONDecoder().decode(Wire.AgentType.self, from: data) == expected)
        }
    }

    @Test func agentTypeDecodesAnUnfamiliarBackendAsUnknown() throws {
        // A desktop newer than this app can name a backend that did not exist
        // when the app shipped. Throwing there would fail the whole snapshot
        // and empty the session list over one unfamiliar string.
        let data = Data("\"some_future_agent\"".utf8)
        #expect(try JSONDecoder().decode(Wire.AgentType.self, from: data) == .unknown)
    }

    @Test func agentStatusMapsEveryCase() {
        #expect(Wire.AgentStatus.working.agentStatus == .working)
        #expect(Wire.AgentStatus.idle.agentStatus == .idle)
        #expect(Wire.AgentStatus.needsInput.agentStatus == .needsInput)
        #expect(Wire.AgentStatus.manual(label: "reviewing").agentStatus == .manual(label: "reviewing"))
    }
}
