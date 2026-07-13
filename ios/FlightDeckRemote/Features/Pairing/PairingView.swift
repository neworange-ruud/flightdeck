//
//  PairingView.swift
//  FlightDeckRemote
//
//  Placeholder for PRD §5.6 pairing flow ("Pair with your Mac" — QR / 4-digit
//  code, Face ID gate). The Pairing feature team fills this in.
//

import SwiftUI

struct PairingView: View {
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
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("PairingView")
    }
}

#Preview {
    PairingView()
}
