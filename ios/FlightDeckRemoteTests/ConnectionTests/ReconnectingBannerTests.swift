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

    // MARK: - Failure-mode classification (remote-control-seo)

    @Test func offlineWhenLinkDownAndNoNetworkPath() {
        #expect(ReconnectingBannerModel.failureMode(
            linkState: .disconnected, peerConnected: nil, hasNetworkPath: false) == .offline)
        #expect(ReconnectingBannerModel.failureMode(
            linkState: .connecting, peerConnected: nil, hasNetworkPath: false) == .offline)
    }

    @Test func unreachableRelayWhenLinkDownButNetworkPathExists() {
        // The 5G ingress-block incident: network is up, relay unreachable → the
        // phone's OWN link is the suspect, not the Mac.
        #expect(ReconnectingBannerModel.failureMode(
            linkState: .disconnected, peerConnected: nil, hasNetworkPath: true) == .unreachableRelay)
        #expect(ReconnectingBannerModel.failureMode(
            linkState: .authenticating, peerConnected: nil, hasNetworkPath: true) == .unreachableRelay)
    }

    @Test func desktopAbsentOnlyWhenLinkUpButPeerKnownAbsent() {
        #expect(ReconnectingBannerModel.failureMode(
            linkState: .connected(latencyMs: 5), peerConnected: false, hasNetworkPath: true) == .desktopAbsent)
    }

    @Test func noFailureWhenLinkUpAndPeerPresentOrUnknown() {
        #expect(ReconnectingBannerModel.failureMode(
            linkState: .connected(latencyMs: 5), peerConnected: true, hasNetworkPath: true) == nil)
        // Unknown presence right after connect is not yet a failure.
        #expect(ReconnectingBannerModel.failureMode(
            linkState: .connected(latencyMs: 5), peerConnected: nil, hasNetworkPath: true) == nil)
    }

    @Test func onlyDesktopAbsentBlamesTheMac() {
        // The whole point of remote-control-seo: only case (b) points at the Mac.
        #expect(ConnectionBannerCopy.stillTrying(.desktopAbsent).contains("Mac"))
        #expect(!ConnectionBannerCopy.stillTrying(.offline).contains("Mac"))
        #expect(!ConnectionBannerCopy.stillTrying(.unreachableRelay).contains("Mac"))
        // Case (a) copy points at the phone's own connection.
        #expect(ConnectionBannerCopy.stillTrying(.offline).localizedCaseInsensitiveContains("connection"))
        #expect(ConnectionBannerCopy.stillTrying(.unreachableRelay).localizedCaseInsensitiveContains("connection"))
    }

    // MARK: - Visibility via failure mode

    @Test func visibleWhenDesktopAbsentEvenWhileRelayConnected() {
        let source = FakeConnectionStatusSource(.connected(latencyMs: 5), peerConnected: false)
        let model = ReconnectingBannerModel(source: source, hasNetworkPath: { true }, launchArguments: [])
        #expect(model.failureMode(isPaired: true) == .desktopAbsent)
        #expect(model.isVisible(isPaired: true) == true)
        // Hidden while unpaired regardless.
        #expect(model.isVisible(isPaired: false) == false)
    }

    @Test func hiddenWhenConnectedAndPeerPresent() {
        let source = FakeConnectionStatusSource(.connected(latencyMs: 5), peerConnected: true)
        let model = ReconnectingBannerModel(source: source, hasNetworkPath: { true }, launchArguments: [])
        #expect(model.isVisible(isPaired: true) == false)
    }

    @Test func offlineFailureModeWhenNetworkPathIsDown() {
        let source = FakeConnectionStatusSource(.disconnected)
        let model = ReconnectingBannerModel(source: source, hasNetworkPath: { false }, launchArguments: [])
        #expect(model.failureMode(isPaired: true) == .offline)
    }

    // MARK: - Retry now (remote-control-0ef.21)

    @Test func retryNowInvokesTheReconnectAction() async {
        let box = RetryBox()
        let model = ReconnectingBannerModel(
            source: FakeConnectionStatusSource(.disconnected),
            onRetry: { box.mark() },
            launchArguments: [])
        #expect(model.canRetry == true)
        model.retryNow()
        _ = await waitUntilMain { box.fired }
        #expect(box.fired == true)
    }

    @Test func noRetryActionMeansNoRetryButton() {
        let model = ReconnectingBannerModel(
            source: FakeConnectionStatusSource(.disconnected),
            launchArguments: [])
        #expect(model.canRetry == false)
    }
}

/// Records whether the banner's "Retry now" action fired.
@MainActor
final class RetryBox {
    private(set) var fired = false
    func mark() { fired = true }
}
