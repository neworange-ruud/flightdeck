//
//  StatusPill.swift
//  FlightDeckRemote
//
//  Pill badge ("NEEDS YOU", "idle", "working", or any custom label + color).
//  PRD §11 shape language: pill badges.
//

import SwiftUI

struct StatusPill: View {

    var label: String
    var color: Color

    /// Solid fill (color background, dark text) vs. the default tinted
    /// style (translucent color background, colored text + border).
    var filled: Bool = false

    var body: some View {
        Text(label.uppercased())
            .typography(Typography.captionBold)
            .foregroundStyle(filled ? Theme.bgDeep : color)
            .padding(.horizontal, Theme.Spacing.md)
            .padding(.vertical, Theme.Spacing.xxs + 2)
            .background(
                Capsule(style: .continuous)
                    .fill(filled ? color : color.opacity(0.16))
            )
            .overlay(
                Capsule(style: .continuous)
                    .strokeBorder(color.opacity(filled ? 0 : 0.55), lineWidth: 1)
            )
            .accessibilityIdentifier("status-pill-\(label.lowercased().replacingOccurrences(of: " ", with: "-"))")
    }
}

extension StatusPill {
    /// Convenience pill for an `AgentStatus`, using its label + color.
    static func status(_ status: AgentStatus, filled: Bool = false) -> StatusPill {
        let label = status.pulsesByDefault ? "needs you" : status.label
        return StatusPill(label: label, color: status.color, filled: filled)
    }
}

#Preview {
    VStack(spacing: 16) {
        HStack(spacing: 10) {
            StatusPill.status(.working)
            StatusPill.status(.idle)
            StatusPill.status(.needsInput)
            StatusPill.status(.manual())
        }
        HStack(spacing: 10) {
            StatusPill.status(.working, filled: true)
            StatusPill.status(.idle, filled: true)
            StatusPill.status(.needsInput, filled: true)
            StatusPill.status(.manual(), filled: true)
        }
        StatusPill(label: "3 agents", color: Theme.textMuted)
    }
    .padding(40)
    .background(Theme.bgDeep)
}
