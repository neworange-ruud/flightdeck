//
//  ConnectionIndicatorTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the connection-honesty label/latency-phrase/color rules (PRD
//  §5.8): "low latency" (<100ms), "ok" (<400ms), "slow" (>=400ms); the three
//  label shapes (Connected/Connecting…/Offline); and the status-color
//  mapping (green connected, orange connecting/authenticating, dim
//  disconnected).
//

import Testing
@testable import FlightDeckRemote

@MainActor
@Suite struct ConnectionIndicatorTests {

    // MARK: - Latency phrase thresholds

    @Test func lowLatencyBelow100ms() {
        #expect(ConnectionLatencyPhrase.phrase(forMs: 0) == "low latency")
        #expect(ConnectionLatencyPhrase.phrase(forMs: 99) == "low latency")
    }

    @Test func okBetween100And399ms() {
        #expect(ConnectionLatencyPhrase.phrase(forMs: 100) == "ok")
        #expect(ConnectionLatencyPhrase.phrase(forMs: 399) == "ok")
    }

    @Test func slowAt400msAndAbove() {
        #expect(ConnectionLatencyPhrase.phrase(forMs: 400) == "slow")
        #expect(ConnectionLatencyPhrase.phrase(forMs: 5000) == "slow")
    }

    // MARK: - Labels

    @Test func connectedLabelIncludesLatencyAndPhrase() {
        #expect(ConnectionIndicator.label(for: .connected(latencyMs: 42)) == "Connected · 42ms · low latency")
        #expect(ConnectionIndicator.label(for: .connected(latencyMs: 220)) == "Connected · 220ms · ok")
        #expect(ConnectionIndicator.label(for: .connected(latencyMs: 900)) == "Connected · 900ms · slow")
    }

    @Test func connectingAndAuthenticatingBothLabelAsConnecting() {
        #expect(ConnectionIndicator.label(for: .connecting) == "Connecting…")
        #expect(ConnectionIndicator.label(for: .authenticating) == "Connecting…")
    }

    @Test func disconnectedLabelsAsOffline() {
        #expect(ConnectionIndicator.label(for: .disconnected) == "Offline")
    }

    // MARK: - Colors

    @Test func connectedIsIdleGreen() {
        #expect(ConnectionIndicator.color(for: .connected(latencyMs: 10)) == Theme.statusIdle)
    }

    @Test func connectingAndAuthenticatingAreOrange() {
        #expect(ConnectionIndicator.color(for: .connecting) == Theme.statusNeedsInput)
        #expect(ConnectionIndicator.color(for: .authenticating) == Theme.statusNeedsInput)
    }

    @Test func disconnectedIsDimmedMuted() {
        #expect(ConnectionIndicator.color(for: .disconnected) == Theme.textMutedDark)
    }
}
