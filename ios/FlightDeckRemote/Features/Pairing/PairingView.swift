//
//  PairingView.swift
//  FlightDeckRemote
//
//  Placeholder for PRD §5.6 pairing flow ("Pair with your Mac" — QR / 4-digit
//  code, Face ID gate). The Pairing feature team fills this in.
//
//  Carries a DEBUG-only "Toggle Paired" button (navigation task) so the
//  paired/unpaired boundary is manually testable in the simulator, and so UI
//  tests can cross it deterministically without a real pairing flow.
//

import SwiftUI

struct PairingView: View {
    var pairingStore: PairingStore

    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "antenna.radiowaves.left.and.right")
                .font(.system(size: 48))
                .foregroundStyle(Theme.orange)
            Text("Pair with your Mac")
                .font(.title2.bold())
                .foregroundStyle(Theme.text)
            Text("Pairing flow placeholder")
                .foregroundStyle(Theme.textMuted)

            #if DEBUG
            Button("Debug: Toggle Paired") {
                pairingStore.debugTogglePaired()
            }
            .typography(Typography.callout)
            .foregroundStyle(Theme.statusCyan)
            .padding(.top, Theme.Spacing.lg)
            .accessibilityIdentifier("debug-toggle-paired-button")
            #endif
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("PairingView")
    }
}

#Preview {
    PairingView(pairingStore: PairingStore())
}
