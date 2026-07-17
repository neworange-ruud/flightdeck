//
//  ReconnectingBannerTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the reconnecting banner's rules (PRD §5.6/§8):
//   - the paired × linkState visibility matrix (hidden unpaired; hidden only
//     while fully `.connected`; visible for connecting/authenticating/
//     disconnected while paired),
//   - the 30s "still trying" escalation, driven by an injected clock so the
//     test never waits on a real timer,
//   - `hasSignal`/no-source behavior (hidden with no signal at all), and
//   - the DEBUG `-uitest-linkstate` seam forcing a state regardless of the
//     underlying source.
//

import Foundation
import Testing
@testable import FlightDeckRemote

@MainActor
@Suite struct ReconnectingBannerTests {

    // MARK: - Pure visibility matrix

    @Test func hiddenWhenUnpairedRegardlessOfLinkState() {
        #expect(ReconnectingBannerModel.isVisible(isPaired: false, linkState: .disconnected) == false)
        #expect(ReconnectingBannerModel.isVisible(isPaired: false, linkState: .connecting) == false)
        #expect(ReconnectingBannerModel.isVisible(isPaired: false, linkState: .authenticating) == false)
        #expect(ReconnectingBannerModel.isVisible(isPaired: false, linkState: .connected(latencyMs: 1)) == false)
    }

    @Test func hiddenWhenPairedAndFullyConnected() {
        #expect(ReconnectingBannerModel.isVisible(isPaired: true, linkState: .connected(latencyMs: 5)) == false)
    }

    @Test func visibleWhenPairedAndNotFullyConnected() {
        #expect(ReconnectingBannerModel.isVisible(isPaired: true, linkState: .disconnected) == true)
        #expect(ReconnectingBannerModel.isVisible(isPaired: true, linkState: .connecting) == true)
        #expect(ReconnectingBannerModel.isVisible(isPaired: true, linkState: .authenticating) == true)
    }

    // MARK: - No signal → hidden

    @Test func hiddenWithNoSourceAndNoDebugOverride() {
        let model = ReconnectingBannerModel(source: nil, launchArguments: [])
        #expect(model.hasSignal == false)
        #expect(model.isVisible(isPaired: true) == false)
    }

    @Test func visibleOnceALiveSourceReportsAnOutageWhilePaired() {
        let source = FakeConnectionStatusSource(.disconnected)
        let model = ReconnectingBannerModel(source: source, launchArguments: [])
        #expect(model.hasSignal == true)
        #expect(model.isVisible(isPaired: true) == true)

        source.linkState = .connected(latencyMs: 3)
        #expect(model.isVisible(isPaired: true) == false)
    }

    // MARK: - 30s escalation via injected clock

    @Test func stillTryingLineHiddenBeforeThirtySeconds() {
        let source = FakeConnectionStatusSource(.disconnected)
        let model = ReconnectingBannerModel(source: source, launchArguments: [])
        let start = Date(timeIntervalSince1970: 0)

        model.tick(now: start)
        #expect(model.showsStillTrying(now: start) == false)
        #expect(model.showsStillTrying(now: start.addingTimeInterval(29)) == false)
    }

    @Test func stillTryingLineAppearsAtThirtySeconds() {
        let source = FakeConnectionStatusSource(.disconnected)
        let model = ReconnectingBannerModel(source: source, launchArguments: [])
        let start = Date(timeIntervalSince1970: 0)

        model.tick(now: start)
        #expect(model.showsStillTrying(now: start.addingTimeInterval(30)) == true)
        #expect(model.showsStillTrying(now: start.addingTimeInterval(120)) == true)
    }

    @Test func stillTryingLineClearsAndOutageClockResetsOnReconnect() {
        let source = FakeConnectionStatusSource(.disconnected)
        let model = ReconnectingBannerModel(source: source, launchArguments: [])
        let start = Date(timeIntervalSince1970: 0)

        model.tick(now: start)
        #expect(model.showsStillTrying(now: start.addingTimeInterval(45)) == true)

        source.linkState = .connected(latencyMs: 1)
        model.tick(now: start.addingTimeInterval(46))
        #expect(model.disconnectedSince == nil)
        #expect(model.showsStillTrying(now: start.addingTimeInterval(46)) == false)
    }

    @Test func outageClockRestartsOnANewOutageAfterReconnecting() {
        let source = FakeConnectionStatusSource(.disconnected)
        let model = ReconnectingBannerModel(source: source, launchArguments: [])
        let start = Date(timeIntervalSince1970: 0)

        model.tick(now: start)
        #expect(model.showsStillTrying(now: start.addingTimeInterval(31)) == true)

        source.linkState = .connected(latencyMs: 1)
        model.tick(now: start.addingTimeInterval(32))

        source.linkState = .disconnected
        model.tick(now: start.addingTimeInterval(33))
        #expect(model.showsStillTrying(now: start.addingTimeInterval(33)) == false, "New outage shouldn't inherit the previous one's elapsed time")
        #expect(model.showsStillTrying(now: start.addingTimeInterval(63)) == true)
    }

    // MARK: - DEBUG launch-argument seam

    @Test func debugSeamForcesLinkStateOverAnyLiveSource() {
        let source = FakeConnectionStatusSource(.connected(latencyMs: 1))
        let model = ReconnectingBannerModel(source: source, launchArguments: ["-uitest-linkstate", "disconnected"])
        #expect(model.hasSignal == true)
        #expect(model.linkState == .disconnected)
        #expect(model.isVisible(isPaired: true) == true)
    }

    @Test func debugSeamCanForceConnectedWithLatency() {
        let model = ReconnectingBannerModel(source: nil, launchArguments: ["-uitest-linkstate", "connected:250"])
        #expect(model.linkState == .connected(latencyMs: 250))
        #expect(model.isVisible(isPaired: true) == false)
    }
}
