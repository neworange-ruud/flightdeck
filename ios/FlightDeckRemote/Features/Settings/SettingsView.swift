//
//  SettingsView.swift
//  FlightDeckRemote
//
//  Placeholder for PRD §5.6 Settings — connected device, notification
//  toggles, Face ID / unpair. The Settings feature team fills this in.
//

import SwiftUI

struct SettingsView: View {
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "gearshape")
                .font(.system(size: 48))
                .foregroundStyle(Theme.textMutedDark)
            Text("Settings")
                .font(.title2.bold())
                .foregroundStyle(Theme.text)
            Text("Settings placeholder")
                .foregroundStyle(Theme.textMuted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("SettingsView")
    }
}

#Preview {
    SettingsView()
}
