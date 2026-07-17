//
//  StaleBanner.swift
//  FlightDeckRemote
//
//  Read-only honesty about cached, offline data (PRD §9.2: "when
//  disconnected, show cached last-known transcript and status, read-only and
//  clearly marked stale; no actions offline"). Actions themselves are
//  already gated elsewhere by `CommandsPausedGate` — this is purely the
//  visible note.
//
//  Two pieces:
//   - `EnvironmentValues.isCacheStaleOffline` — a lightweight flag
//     `MainTabView` sets once, above the whole tab content, so sibling
//     screens (Projects/Sessions/Chat) can read
//     `@Environment(\.isCacheStaleOffline)` to adjust their own presentation
//     later without any further plumbing changes on their part.
//   - `StaleBanner` — the actual muted, non-alarming bar `MainTabView` mounts
//     as a top overlay, below the (louder) `ReconnectingBanner`.
//
//  Visibility (computed by `MainTabView`, not this file): cache-seeded
//  (`TransportStore.isCacheStale`) *and* the link isn't a live `.connected`
//  session — the same "down" definition `ReconnectingBannerModel.isDown`
//  already uses, reused rather than redefined.
//

import SwiftUI

private struct IsCacheStaleOfflineKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    /// Whether the screen's data is currently cache-seeded, offline
    /// "last-known state" rather than a live feed (PRD §9.2).
    var isCacheStaleOffline: Bool {
        get { self[IsCacheStaleOfflineKey.self] }
        set { self[IsCacheStaleOfflineKey.self] = newValue }
    }
}

/// Muted "showing last-known state" bar. Visually distinct from
/// `ReconnectingBanner`: no spinner, no urgency — this is an honest label on
/// existing content, not an active problem to resolve.
struct StaleBanner: View {
    var body: some View {
        HStack(spacing: Theme.Spacing.sm) {
            Image(systemName: "clock.arrow.circlepath")
                .font(.system(size: 13, weight: .semibold))
            Text("Showing last-known state — offline")
                .typography(Typography.caption)
        }
        .foregroundStyle(Theme.textMuted)
        .padding(.horizontal, Theme.Spacing.md)
        .padding(.vertical, Theme.Spacing.sm)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Theme.bgCard.opacity(0.9), in: RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
        .padding(.horizontal, Theme.Spacing.md)
        .padding(.top, Theme.Spacing.xs)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("stale-banner")
    }
}

#Preview {
    ZStack(alignment: .top) {
        Theme.bgDeep.ignoresSafeArea()
        StaleBanner()
    }
}
