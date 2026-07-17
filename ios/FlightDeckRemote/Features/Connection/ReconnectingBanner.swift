//
//  ReconnectingBanner.swift
//  FlightDeckRemote
//
//  The visible half of connection honesty (PRD §5.6/§8): "lost link pauses
//  commands loudly, nothing sent blind." `TransportClient` already refuses
//  to send while not connected (delivery honesty) — this banner is the
//  loud, visible companion, mounted as a top overlay in `MainTabView`.
//
//  Visibility rule: shown whenever the device is paired *and* the link
//  isn't fully `.connected` (so during `.connecting`/`.authenticating`/
//  `.disconnected` alike — the user doesn't need to know which phase, only
//  that commands are paused right now). Hidden while unpaired — the Pairing
//  screen has its own states for that. After 30s in the same outage, an
//  extra honesty line invites the user to check the Mac.
//

import Observation
import SwiftUI

/// Drives `ReconnectingBanner`'s visibility + 30s escalation. Kept
/// SwiftUI-free (aside from `@Observable`) so the rules are unit testable
/// with an injected clock instead of a real `Timer`.
@MainActor
@Observable
final class ReconnectingBannerModel {
    /// How long the *same* outage must persist before the "still trying"
    /// honesty line appears (PRD §5.6).
    static let stillTryingThreshold: TimeInterval = 30

    private let source: (any ConnectionStatusSource)?

    #if DEBUG
    private let forced: RemoteLinkState?
    #endif

    /// When the current outage started, or `nil` while connected. Exposed
    /// `private(set)` for tests; drive it forward with `tick(now:)`.
    private(set) var disconnectedSince: Date?

    init(
        source: (any ConnectionStatusSource)?,
        launchArguments: [String] = ProcessInfo.processInfo.arguments
    ) {
        self.source = source
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

    /// Whether the banner should be visible right now, given the app's
    /// pairing state.
    func isVisible(isPaired: Bool) -> Bool {
        guard hasSignal else { return false }
        return Self.isVisible(isPaired: isPaired, linkState: linkState)
    }

    /// Pure visibility rule (paired × linkState), unit tested directly.
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

/// Top-of-screen banner: "Reconnecting to desktop…" headline + the
/// commands-paused honesty line, with a subtle spinner. Safe-area aware
/// (placed by its parent, not `.ignoresSafeArea()`'d) and animates in/out.
struct ReconnectingBanner: View {
    var model: ReconnectingBannerModel
    var isPaired: Bool

    var body: some View {
        TimelineView(.periodic(from: .now, by: 1)) { context in
            let visible = model.isVisible(isPaired: isPaired)
            ZStack {
                if visible {
                    content(now: context.date)
                        .transition(.move(edge: .top).combined(with: .opacity))
                }
            }
            .onAppear { model.tick(now: context.date) }
            .onChange(of: context.date) { _, now in model.tick(now: now) }
            .animation(.easeInOut(duration: 0.25), value: visible)
        }
    }

    @ViewBuilder
    private func content(now: Date) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            HStack(spacing: Theme.Spacing.sm) {
                WorkingSpinner(size: 14, lineWidth: 2, color: Theme.textPrimary)
                Text("Reconnecting to desktop…")
                    .typography(Typography.headline)
                    .foregroundStyle(Theme.textPrimary)
                    .accessibilityIdentifier("reconnecting-banner-headline")
            }
            Text("Commands are paused until the link is back. Nothing is sent blind.")
                .typography(Typography.callout)
                .foregroundStyle(Theme.textMuted)
                .accessibilityIdentifier("reconnecting-banner-body")

            if model.showsStillTrying(now: now) {
                Text("Still trying — is FlightDeck running on your Mac?")
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textMuted)
                    .accessibilityIdentifier("reconnecting-banner-still-trying")
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
