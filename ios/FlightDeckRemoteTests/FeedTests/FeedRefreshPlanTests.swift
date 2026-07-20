//
//  FeedRefreshPlanTests.swift
//  FlightDeckRemoteTests
//
//  Covers the unified feed's pull-to-refresh decision (remote-control-b8d.8):
//  a live (`.connected`) machine resyncs; anything else reconnects instead.
//

import Testing
@testable import FlightDeckRemote

@Suite struct FeedRefreshPlanTests {

    @Test func connectedMachineResyncs() {
        #expect(FeedRefreshPlan.action(for: .connected(latencyMs: 12)) == .resync)
    }

    @Test func disconnectedMachineReconnects() {
        #expect(FeedRefreshPlan.action(for: .disconnected) == .reconnect)
    }

    @Test func connectingMachineReconnects() {
        #expect(FeedRefreshPlan.action(for: .connecting) == .reconnect)
    }

    @Test func authenticatingMachineReconnects() {
        #expect(FeedRefreshPlan.action(for: .authenticating) == .reconnect)
    }
}
