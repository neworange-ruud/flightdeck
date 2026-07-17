//
//  ConnectionIndicator.swift
//  FlightDeckRemote
//
//  Reusable connection-honesty indicator (PRD §5.8 "no SLA; always show
//  honest connection state + latency"): a colored dot + label reporting the
//  live relay link state. Settings' "Connected · low latency" row (PRD §5.6)
//  is this component in `.full` size — that screen's task wires the real
//  `TransportStore` in; this task only builds the reusable piece.
//
//  Label/latency-phrase rules:
//   - `.connected(latencyMs:)` → "Connected · <ms>ms · <phrase>", where the
//     phrase is "low latency" (<100ms), "ok" (<400ms), or "slow" (>=400ms).
//   - `.connecting` / `.authenticating` → "Connecting…" (both render
//     identically — the phone can't tell the user apart which handshake
//     step it's on in any way that helps them).
//   - `.disconnected` → "Offline".
//

import SwiftUI

/// Pure latency → phrase mapping (PRD §5.8), factored out so it's unit
/// testable without instantiating any view.
enum ConnectionLatencyPhrase {
    /// - `<100ms` → "low latency"
    /// - `<400ms` → "ok"
    /// - otherwise → "slow"
    static func phrase(forMs ms: Int) -> String {
        switch ms {
        case ..<100: "low latency"
        case ..<400: "ok"
        default: "slow"
        }
    }
}

/// Compact colored dot + label reporting the live link state. Reused by the
/// reconnecting banner's spirit (though the banner has its own copy) and by
/// Settings' connected-device row.
struct ConnectionIndicator: View {
    /// `.compact` — dot only (plus an accessibility label / `.help` tooltip
    /// for pointer/VoiceOver users). `.full` — dot + visible text label.
    enum Size {
        case compact
        case full
    }

    var linkState: RemoteLinkState
    var size: Size = .full

    var body: some View {
        HStack(spacing: Theme.Spacing.xs) {
            Circle()
                .fill(Self.color(for: linkState))
                .frame(width: 8, height: 8)

            if size == .full {
                Text(Self.label(for: linkState))
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textMuted)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Self.label(for: linkState))
        .accessibilityIdentifier("connection-indicator")
        .help(Self.label(for: linkState))
    }

    /// The full honest label for a given link state (PRD §5.8). Static +
    /// pure so it's unit testable without a view.
    static func label(for linkState: RemoteLinkState) -> String {
        switch linkState {
        case .disconnected:
            "Offline"
        case .connecting, .authenticating:
            "Connecting…"
        case let .connected(latencyMs):
            "Connected · \(latencyMs)ms · \(ConnectionLatencyPhrase.phrase(forMs: latencyMs))"
        }
    }

    /// The dot/status color for a given link state: idle-green connected,
    /// orange connecting/authenticating, dim/muted disconnected.
    static func color(for linkState: RemoteLinkState) -> Color {
        switch linkState {
        case .connected:
            Theme.statusIdle
        case .connecting, .authenticating:
            Theme.statusNeedsInput
        case .disconnected:
            Theme.textMutedDark
        }
    }
}

#Preview {
    VStack(alignment: .leading, spacing: 20) {
        ConnectionIndicator(linkState: .connected(latencyMs: 42))
        ConnectionIndicator(linkState: .connected(latencyMs: 220))
        ConnectionIndicator(linkState: .connected(latencyMs: 900))
        ConnectionIndicator(linkState: .connecting)
        ConnectionIndicator(linkState: .authenticating)
        ConnectionIndicator(linkState: .disconnected)
        HStack(spacing: 16) {
            ConnectionIndicator(linkState: .connected(latencyMs: 42), size: .compact)
            ConnectionIndicator(linkState: .connecting, size: .compact)
            ConnectionIndicator(linkState: .disconnected, size: .compact)
        }
    }
    .padding(40)
    .background(Theme.bgDeep)
}
