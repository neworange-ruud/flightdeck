//
//  CommandsPausedGateTests.swift
//  FlightDeckRemoteTests
//
//  Verifies `CommandsPausedGate.commandsPaused` (PRD §5.6/§8: lost link
//  pauses commands loudly) tracks the live source's link state, that only a
//  fully-established `.connected` session un-pauses commands, and that the
//  DEBUG `-uitest-linkstate` seam can force a state for tests/previews.
//

import Testing
@testable import FlightDeckRemote

@MainActor
@Suite struct CommandsPausedGateTests {

    @Test func pausedWhileDisconnectedConnectingOrAuthenticating() {
        for state: RemoteLinkState in [.disconnected, .connecting, .authenticating] {
            let gate = CommandsPausedGate(source: FakeConnectionStatusSource(state), launchArguments: [])
            #expect(gate.commandsPaused == true)
        }
    }

    @Test func notPausedOnceFullyConnected() {
        let gate = CommandsPausedGate(source: FakeConnectionStatusSource(.connected(latencyMs: 12)), launchArguments: [])
        #expect(gate.commandsPaused == false)
    }

    @Test func tracksLiveChangesOnTheUnderlyingSource() {
        let source = FakeConnectionStatusSource(.connected(latencyMs: 5))
        let gate = CommandsPausedGate(source: source, launchArguments: [])
        #expect(gate.commandsPaused == false)

        source.linkState = .disconnected
        #expect(gate.commandsPaused == true)

        source.linkState = .connected(latencyMs: 5)
        #expect(gate.commandsPaused == false)
    }

    @Test func debugSeamForcesPausedStateOverALiveConnectedSource() {
        let source = FakeConnectionStatusSource(.connected(latencyMs: 5))
        let gate = CommandsPausedGate(source: source, launchArguments: ["-uitest-linkstate", "disconnected"])
        #expect(gate.commandsPaused == true)
    }

    @Test func debugSeamCanForceUnpausedOverALiveDownSource() {
        let source = FakeConnectionStatusSource(.disconnected)
        let gate = CommandsPausedGate(source: source, launchArguments: ["-uitest-linkstate", "connected:1"])
        #expect(gate.commandsPaused == false)
    }
}
