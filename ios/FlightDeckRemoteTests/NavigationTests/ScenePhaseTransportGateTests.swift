//
//  ScenePhaseTransportGateTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the scene-phase → transport foreground-intent decision
//  (remote-control-0ef.3): `.active` connects, `.background` tears down, and
//  the transient `.inactive` phase is left untouched (no reconnect churn on a
//  Control Center pull / app-switcher glance / incoming call / Face ID prompt).
//

import SwiftUI
import Testing
@testable import FlightDeckRemote

@Suite struct ScenePhaseTransportGateTests {

    @Test func activePhaseConnects() {
        #expect(ScenePhaseTransportGate.foregroundIntent(for: .active) == true)
    }

    @Test func backgroundPhaseTearsDown() {
        #expect(ScenePhaseTransportGate.foregroundIntent(for: .background) == false)
    }

    @Test func inactivePhaseIsLeftUntouched() {
        // nil = "no change": the transient `.inactive` phase must NOT toggle the
        // transport, so a Control Center pull / app-switcher glance / incoming
        // call / Face ID prompt no longer forces a full reconnect
        // (remote-control-0ef.3).
        #expect(ScenePhaseTransportGate.foregroundIntent(for: .inactive) == nil)
    }
}
