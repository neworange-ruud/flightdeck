//
//  ReconnectingBanner.swift
//  FlightDeckRemote
//
//  The visible half of connection honesty (PRD §5.6/§8): "lost link pauses
//  commands loudly, nothing sent blind." `TransportClient` already refuses
//  to send while not connected (delivery honesty) — this banner is the
//  loud, visible companion, mounted as a top overlay in `MainTabView`.
//
//  Visibility rule: shown whenever the device is paired *and* there is an
//  active connection failure — the phone↔relay link is down, OR the link is up
//  but the desktop peer is known-absent (remote-control-seo). Hidden while
//  unpaired (the Pairing screen has its own states) and while fully live with
//  the desktop present.
//
//  Failure-mode honesty (remote-control-seo): the copy points at the RIGHT
//  culprit. When the phone can't reach the relay (offline, or relay
//  unreachable — e.g. ingress-blocked on 5G) the banner points the user at
//  their OWN connection; only when the relay IS reachable but the desktop is
//  absent does it ask "is FlightDeck running on your Mac?". After 30s an extra
//  honesty line escalates in the same voice.
//
//  A "Retry now" button (remote-control-0ef.21) lets the user force an
//  immediate reconnect (resetting the backoff, which is otherwise capped at
//  60s) rather than waiting out the current attempt.
//

import Observation
import SwiftUI

/// Why the connection is currently failing (remote-control-seo). Drives which
/// culprit the banner names.
enum ConnectionFailureMode: Equatable, Sendable {
    /// The phone has no usable network path at all (NWPathMonitor unsatisfied).
    case offline
    /// A network path exists, but the phone↔relay link isn't up — the WebSocket
    /// won't open / was ingress-rejected / dropped. The phone's OWN link to the
    /// relay is the suspect, NOT the Mac.
    case unreachableRelay
    /// The phone↔relay link is fully up, but the desktop peer is known-absent —
    /// the Mac is the suspect ("is FlightDeck running on your Mac?").
    case desktopAbsent
}

/// Drives `ReconnectingBanner`'s visibility, failure-mode classification, and
/// 30s escalation. Kept SwiftUI-free (aside from `@Observable`) so the rules are
/// unit testable with injected inputs instead of a real relay / `Timer` /
/// `NWPathMonitor`.
@MainActor
@Observable
final class ReconnectingBannerModel {
    /// How long the *same* outage must persist before the "still trying"
    /// honesty line appears (PRD §5.6).
    static let stillTryingThreshold: TimeInterval = 30

    private let source: (any ConnectionStatusSource)?
    /// Whether the phone currently has a usable network path (remote-control-0ef.22
    /// / -seo). Read as a closure so the banner (which re-renders once a second)
    /// polls the live value without the model needing to observe `NWPathMonitor`.
    private let hasNetworkPath: () -> Bool
    /// Forces an immediate reconnect (remote-control-0ef.21 "Retry now"). `nil`
    /// disables the button (e.g. previews / tests with no live transport).
    private let onRetry: (@MainActor () async -> Void)?

    #if DEBUG
    private let forced: RemoteLinkState?
    #endif

    /// When the current outage started, or `nil` while connected. Exposed
    /// `private(set)` for tests; drive it forward with `tick(now:)`.
    private(set) var disconnectedSince: Date?

    init(
        source: (any ConnectionStatusSource)?,
        hasNetworkPath: @escaping () -> Bool = { true },
        onRetry: (@MainActor () async -> Void)? = nil,
        launchArguments: [String] = ProcessInfo.processInfo.arguments
    ) {
        self.source = source
        self.hasNetworkPath = hasNetworkPath
        self.onRetry = onRetry
        #if DEBUG
        self.forced = ConnectionDebugSeam.forcedLinkState(arguments: launchArguments)
        #endif
    }

    /// Whether this model has *any* real signal to show — a live source, or
    /// (DEBUG) a forced launch-argument state. With neither, there's nothing
    /// honest to say yet, so the banner stays hidden rather than guessing.
    var hasSignal: Bool {
        #if DEBUG
        if forced != nil { return true }
        #endif
        return source != nil
    }

    /// The link state driving the banner (DEBUG forced state wins, for UI
    /// tests/previews).
    var linkState: RemoteLinkState {
        #if DEBUG
        if let forced { return forced }
        #endif
        return source?.linkState ?? .disconnected
    }

    /// The desktop peer's presence, or `nil` if unknown / no source.
    var peerConnected: Bool? { source?.peerConnected }

    /// Whether a "Retry now" action is available.
    var canRetry: Bool { onRetry != nil }

    /// Force an immediate reconnect (remote-control-0ef.21).
    func retryNow() {
        guard let onRetry else { return }
        Task { await onRetry() }
    }

    /// Whether the banner should be visible right now, given the app's
    /// pairing state — i.e. paired AND there is an active failure mode.
    func isVisible(isPaired: Bool) -> Bool {
        failureMode(isPaired: isPaired) != nil
    }

    /// The active failure mode, or `nil` when the banner should be hidden
    /// (unpaired, no signal, or fully live with the desktop present).
    func failureMode(isPaired: Bool) -> ConnectionFailureMode? {
        guard hasSignal, isPaired else { return nil }
        return Self.failureMode(
            linkState: linkState,
            peerConnected: peerConnected,
            hasNetworkPath: hasNetworkPath())
    }

    /// Pure failure-mode classifier (remote-control-seo), unit tested directly:
    ///  - link fully up + desktop known-absent → `.desktopAbsent` (blame the Mac);
    ///  - link fully up + desktop present/unknown → `nil` (no failure);
    ///  - link not up + no network path → `.offline` (blame the phone's network);
    ///  - link not up + network path exists → `.unreachableRelay` (blame the
    ///    phone's link to the relay, e.g. ingress-blocked on 5G).
    static func failureMode(
        linkState: RemoteLinkState,
        peerConnected: Bool?,
        hasNetworkPath: Bool
    ) -> ConnectionFailureMode? {
        if case .connected = linkState {
            // Phone↔relay link is fully up. Only a KNOWN-absent desktop is a
            // failure worth a banner; `nil`/unknown right after connect is not.
            return peerConnected == false ? .desktopAbsent : nil
        }
        // Phone↔relay link is NOT up — the relay is what we can't reach.
        return hasNetworkPath ? .unreachableRelay : .offline
    }

    /// Pure visibility rule (paired × linkState), unit tested directly. Kept for
    /// the link-state-only matrix and for `MainTabView`'s stale-banner gate;
    /// the full banner uses `failureMode` (which also weighs peer + network).
    static func isVisible(isPaired: Bool, linkState: RemoteLinkState) -> Bool {
        guard isPaired else { return false }
        return isDown(linkState)
    }

    /// Whether a link state counts as "down" for banner/gate purposes —
    /// anything short of a live `.connected` link.
    static func isDown(_ linkState: RemoteLinkState) -> Bool {
        if case .connected = linkState { return false }
        return true
    }

    /// Advances the outage clock: starts `disconnectedSince` on the first
    /// tick observed while down, clears it once reconnected. Idempotent —
    /// safe to call on every UI tick. Tests call this directly with a
    /// controlled `now` instead of waiting on a real clock.
    func tick(now: Date) {
        if Self.isDown(linkState) {
            if disconnectedSince == nil { disconnectedSince = now }
        } else {
            disconnectedSince = nil
        }
    }

    /// Whether the current outage has lasted long enough to show the "still
    /// trying" honesty line.
    func showsStillTrying(now: Date) -> Bool {
        guard let since = disconnectedSince else { return false }
        return now.timeIntervalSince(since) >= Self.stillTryingThreshold
    }
}

/// Copy for each failure mode (remote-control-seo). Pure + static so it's unit
/// testable and so the culprit-naming rule lives in one place.
enum ConnectionBannerCopy {
    static func headline(_ mode: ConnectionFailureMode) -> String {
        switch mode {
        case .offline: "No internet connection"
        case .unreachableRelay: "Reconnecting…"
        case .desktopAbsent: "Your Mac isn't connected"
        }
    }

    static func body(_ mode: ConnectionFailureMode) -> String {
        switch mode {
        case .offline:
            "Your phone appears to be offline. Commands are paused until it's back online."
        case .unreachableRelay:
            "Commands are paused until the link is back. Nothing is sent blind."
        case .desktopAbsent:
            "Connected to the relay, but FlightDeck on your Mac isn't reachable. Commands are paused."
        }
    }

    /// The escalated honesty line after 30s — points at the correct culprit.
    /// Only `.desktopAbsent` blames the Mac (remote-control-seo).
    static func stillTrying(_ mode: ConnectionFailureMode) -> String {
        switch mode {
        case .offline:
            "Still offline — check your Wi-Fi or cellular connection."
        case .unreachableRelay:
            "Still trying — check your phone's internet connection."
        case .desktopAbsent:
            "Still trying — is FlightDeck running on your Mac?"
        }
    }
}

/// Top-of-screen banner: a failure-mode headline + the commands-paused honesty
/// line, a subtle spinner, and a "Retry now" affordance. Safe-area aware
/// (placed by its parent, not `.ignoresSafeArea()`'d) and animates in/out.
struct ReconnectingBanner: View {
    var model: ReconnectingBannerModel
    var isPaired: Bool

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            let mode = model.failureMode(isPaired: isPaired)
            ZStack {
                if let mode {
                    content(mode: mode, now: context.date)
                        .transition(.move(edge: .top).combined(with: .opacity))
                }
            }
            .onAppear { model.tick(now: context.date) }
            .onChange(of: context.date) { _, now in model.tick(now: now) }
            .animation(.easeInOut(duration: 0.25), value: mode)
        }
    }

    @ViewBuilder
    private func content(mode: ConnectionFailureMode, now: Date) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            HStack(spacing: Theme.Spacing.sm) {
                WorkingSpinner(size: 14, lineWidth: 2, color: Theme.textPrimary)
                Text(ConnectionBannerCopy.headline(mode))
                    .typography(Typography.headline)
                    .foregroundStyle(Theme.textPrimary)
                    .accessibilityIdentifier("reconnecting-banner-headline")
            }
            Text(ConnectionBannerCopy.body(mode))
                .typography(Typography.callout)
                .foregroundStyle(Theme.textMuted)
                .accessibilityIdentifier("reconnecting-banner-body")

            if model.showsStillTrying(now: now) {
                Text(ConnectionBannerCopy.stillTrying(mode))
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textMuted)
                    .accessibilityIdentifier("reconnecting-banner-still-trying")
            }

            if model.canRetry {
                Button {
                    model.retryNow()
                } label: {
                    Text("Retry now")
                        .typography(Typography.bodyMedium)
                        .foregroundStyle(Theme.accent)
                }
                .buttonStyle(.plain)
                .padding(.top, Theme.Spacing.xxs)
                .accessibilityIdentifier("reconnecting-banner-retry")
            }
        }
        .padding(Theme.Spacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.bgRaised)
        .clipShape(RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
        .padding(.horizontal, Theme.Spacing.md)
        .padding(.top, Theme.Spacing.sm)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("reconnecting-banner")
    }
}

#Preview {
    ZStack(alignment: .top) {
        Theme.bgDeep.ignoresSafeArea()
        ReconnectingBanner(
            model: ReconnectingBannerModel(source: nil, launchArguments: ["-uitest-linkstate", "connecting"]),
            isPaired: true
        )
    }
}
