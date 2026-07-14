//
//  RollupDotDisplayTests.swift
//  FlightDeckRemoteTests
//
//  Covers `Wire.RollupDot.agentStatus` — the mapping a project card's
//  `StatusDot` renders from the wire roll-up dot.
//

import Testing
@testable import FlightDeckRemote

struct RollupDotDisplayTests {

    @Test func mapsEveryDotToItsAgentStatus() {
        #expect(Wire.RollupDot.needsInput.agentStatus == .needsInput)
        #expect(Wire.RollupDot.working.agentStatus == .working)
        #expect(Wire.RollupDot.idle.agentStatus == .idle)
        #expect(Wire.RollupDot.manual.agentStatus == .manual())
    }
}
