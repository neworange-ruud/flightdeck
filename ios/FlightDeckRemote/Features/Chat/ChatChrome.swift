//
//  ChatChrome.swift
//  FlightDeckRemote
//
//  The non-transcript chrome around the agent chat (PRD §5.3):
//   - `ChatSurfaceSwitcher` — the per-session `Agent · Shell` segmented
//     control. Both segments are selectable; `Shell` mounts the minimal
//     terminal (`ShellView`, PRD §5.4) in `AgentChatView`.
//
//  The compose field + mic (`ChatComposeBar`) now lives in its own file
//  (`ChatComposeBar.swift`).
//

import SwiftUI

/// The `Agent · Shell` surface switcher. Both are selectable; `shell` mounts
/// the minimal terminal (`ShellView`, PRD §5.4).
struct ChatSurfaceSwitcher: View {
    @Binding var surface: ChatSurface

    var body: some View {
        HStack(spacing: 0) {
            segment(title: "Agent", surface: .agent, enabled: true)
            // Shell surface is live (PRD §5.4 minimal terminal); `AgentChatView`
            // mounts `ShellView` for `.shell`.
            segment(title: "Shell", surface: .shell, enabled: true)
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
