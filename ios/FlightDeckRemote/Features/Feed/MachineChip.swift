//
//  MachineChip.swift
//  FlightDeckRemote
//
//  The unified feed's per-row machine tag (remote-control-b8d.8): a small,
//  legible pill carrying a `FeedItem.displayName` (override > desktop-
//  reported name > generic fallback — see `PairedInstance.displayName`), so a
//  row is attributable to its owning machine at a glance in an interleaved,
//  multi-machine list. Styled from the same DesignSystem tokens as
//  `StatusPill` (capsule, hairline border) but deliberately neutral (no
//  status color) — a machine isn't a status.
//
//  Light/dark legibility: the app is dark-only (`UIUserInterfaceStyle` =
//  Dark, see `Theme`'s doc comment) — there is no light-appearance branch to
//  add here; the chip uses the same raised-surface + hairline-border tokens
//  every other card/pill in the app already relies on for contrast against
//  `Theme.bgDeep`/`Theme.bgCard`.
//

import SwiftUI

struct MachineChip: View {
    var displayName: String

    var body: some View {
        Text(displayName)
            .typography(Typography.captionBold)
            .foregroundStyle(Theme.textMuted)
            .lineLimit(1)
            .padding(.horizontal, Theme.Spacing.sm)
            .padding(.vertical, Theme.Spacing.xxs)
            .background(Theme.bgRaised, in: Capsule(style: .continuous))
            .overlay(
                Capsule(style: .continuous)
                    .strokeBorder(Theme.text.opacity(0.14), lineWidth: 1)
            )
            // A single element so the call site's own `.accessibilityIdentifier`
            // (scoped per feed row — see `FeedView.rowContent`) attaches
            // cleanly rather than fragmenting across the `Text`'s children.
            .accessibilityElement()
            .accessibilityLabel(Text("Machine: \(displayName)"))
    }
}

#Preview {
    HStack(spacing: 12) {
        MachineChip(displayName: "Ruud's MacBook Pro")
        MachineChip(displayName: "Paired Mac")
    }
    .padding(40)
    .background(Theme.bgDeep)
}
