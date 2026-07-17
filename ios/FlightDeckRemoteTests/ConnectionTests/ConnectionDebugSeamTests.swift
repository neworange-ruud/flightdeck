//
//  ConnectionDebugSeamTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the `-uitest-linkstate <state>` launch-argument parser used by
//  `ReconnectingBannerModel`/`CommandsPausedGate` to force a `RemoteLinkState`
//  for UI tests and previews.
//

import Testing
@testable import FlightDeckRemote

@Suite struct ConnectionDebugSeamTests {

    @Test func absentFlagYieldsNil() {
        #expect(ConnectionDebugSeam.forcedLinkState(arguments: []) == nil)
        #expect(ConnectionDebugSeam.forcedLinkState(arguments: ["-uitest-reset-pairing"]) == nil)
    }

    @Test func missingValueYieldsNil() {
        #expect(ConnectionDebugSeam.forcedLinkState(arguments: ["-uitest-linkstate"]) == nil)
    }

    @Test func parsesEachBareState() {
        #expect(ConnectionDebugSeam.forcedLinkState(arguments: ["-uitest-linkstate", "disconnected"]) == .disconnected)
        #expect(ConnectionDebugSeam.forcedLinkState(arguments: ["-uitest-linkstate", "connecting"]) == .connecting)
        #expect(ConnectionDebugSeam.forcedLinkState(arguments: ["-uitest-linkstate", "authenticating"]) == .authenticating)
        #expect(ConnectionDebugSeam.forcedLinkState(arguments: ["-uitest-linkstate", "connected"]) == .connected(latencyMs: 0))
    }

    @Test func parsesConnectedWithExplicitLatency() {
        #expect(ConnectionDebugSeam.forcedLinkState(arguments: ["-uitest-linkstate", "connected:42"]) == .connected(latencyMs: 42))
    }

    @Test func malformedLatencyFallsBackToZero() {
        #expect(ConnectionDebugSeam.forcedLinkState(arguments: ["-uitest-linkstate", "connected:oops"]) == .connected(latencyMs: 0))
    }

    @Test func unknownStateYieldsNil() {
        #expect(ConnectionDebugSeam.forcedLinkState(arguments: ["-uitest-linkstate", "bogus"]) == nil)
    }
}
