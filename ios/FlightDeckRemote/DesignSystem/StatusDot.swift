//
//  StatusDot.swift
//  FlightDeckRemote
//
//  Glowing status dot (PRD §11: "glowing status dots"). Used on session rows
//  (small) and project cards (large, roll-up). "Needs input" pulses by
//  default to pull the user's eye — the most urgent state.
//

import SwiftUI

struct StatusDot: View {

    /// Row-scale dot (session rows) vs. card-scale dot (project roll-up).
    enum Size {
        case small
        case large

        var diameter: CGFloat {
            switch self {
            case .small: 8
            case .large: 14
            }
        }
    }

    var status: AgentStatus
    var size: Size = .small

    /// Overrides whether the dot pulses. `nil` (default) defers to
    /// `AgentStatus.pulsesByDefault` (only `.needsInput` pulses).
    var pulsing: Bool?

    @State private var isPulsing = false

    private var shouldPulse: Bool { pulsing ?? status.pulsesByDefault }

    var body: some View {
        Circle()
            .fill(status.color)
            .frame(width: size.diameter, height: size.diameter)
            .shadow(color: status.color.opacity(0.9), radius: size.diameter * 0.5)
            .shadow(color: status.color.opacity(0.55), radius: size.diameter * (isPulsing ? 1.6 : 1.0))
            .scaleEffect(isPulsing ? 1.2 : 1.0)
            .onAppear {
                guard shouldPulse else { return }
                withAnimation(.easeInOut(duration: 0.9).repeatForever(autoreverses: true)) {
                    isPulsing = true
                }
            }
            .accessibilityLabel(Text(status.label))
            .accessibilityIdentifier("status-dot-\(status.identifier)")
    }
}

#Preview {
    VStack(spacing: 20) {
        HStack(spacing: 16) {
            StatusDot(status: .working)
            StatusDot(status: .idle)
            StatusDot(status: .needsInput)
            StatusDot(status: .manual())
        }
        HStack(spacing: 16) {
            StatusDot(status: .working, size: .large)
            StatusDot(status: .idle, size: .large)
            StatusDot(status: .needsInput, size: .large)
            StatusDot(status: .manual(), size: .large)
        }
    }
    .padding(40)
    .background(Theme.bgDeep)
}
