//
//  ChatChrome.swift
//  FlightDeckRemote
//
//  The non-transcript chrome around the agent chat (PRD §5.3):
//   - `ChatSurfaceSwitcher` — the per-session `Agent · Shell` segmented
//     control. `Shell` is rendered with a subtle "soon" treatment and is
//     disabled (the real terminal is PRD §5.4, a later task).
//   - `ChatComposeBar` — an inert placeholder for the compose field + mic.
//     Compose is a separate task; this is the seam it replaces. It is
//     visually present but non-functional (disabled) here.
//

import SwiftUI

/// The `Agent · Shell` surface switcher. `agent` is selectable; `shell` is
/// disabled with a "soon" chip until the terminal surface ships.
struct ChatSurfaceSwitcher: View {
    @Binding var surface: ChatSurface

    var body: some View {
        HStack(spacing: 0) {
            segment(title: "Agent", surface: .agent, enabled: true)
            segment(title: "Shell", surface: .shell, enabled: false, showsSoon: true)
        }
        .padding(Theme.Spacing.xxs)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.pill, style: .continuous)
                .fill(Theme.bgField)
        )
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("surface-switcher")
    }

    @ViewBuilder
    private func segment(title: String, surface target: ChatSurface,
                         enabled: Bool, showsSoon: Bool = false) -> some View {
        let selected = surface == target
        Button {
            guard enabled else { return }
            surface = target
        } label: {
            HStack(spacing: Theme.Spacing.xs) {
                Text(title)
                    .typography(Typography.callout)
                if showsSoon {
                    Text("soon")
                        .typography(Typography.captionBold)
                        .textCase(.uppercase)
                        .padding(.horizontal, Theme.Spacing.xs)
                        .padding(.vertical, 1)
                        .background(
                            Capsule().fill(Theme.bgRaised)
                        )
                }
            }
            .foregroundStyle(segmentForeground(selected: selected, enabled: enabled))
            .frame(maxWidth: .infinity)
            .padding(.vertical, Theme.Spacing.sm)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.pill, style: .continuous)
                    .fill(selected ? Theme.bgRaised : Color.clear)
            )
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .accessibilityIdentifier(target == .agent ? "surface-agent" : "surface-shell")
    }

    private func segmentForeground(selected: Bool, enabled: Bool) -> Color {
        if !enabled { return Theme.textDim }
        return selected ? Theme.textPrimary : Theme.textMuted
    }
}

/// Inert compose-bar placeholder (PRD §5.3 "Reply to fix-login…" + mic). The
/// compose task replaces this with the real editable field + hold-to-talk mic;
/// here it is visually present but disabled.
struct ChatComposeBar: View {
    /// Session name for the placeholder prompt ("Reply to fix-login…").
    let sessionName: String

    var body: some View {
        HStack(spacing: Theme.Spacing.sm) {
            Text("Reply to \(sessionName)…")
                .typography(Typography.body)
                .foregroundStyle(Theme.textDim)
                .lineLimit(1)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, Theme.Spacing.md)
                .padding(.vertical, Theme.Spacing.md)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                        .fill(Theme.bgField)
                )

            Image(systemName: "mic.fill")
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(Theme.textDim)
                .frame(width: 44, height: 44)
                .background(Circle().fill(Theme.bgField))
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.vertical, Theme.Spacing.sm)
        .background(Theme.bgDeep)
        .allowsHitTesting(false) // inert seam — compose task wires this up
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("chat-compose-bar")
    }
}
