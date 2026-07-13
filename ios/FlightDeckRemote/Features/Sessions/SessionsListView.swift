//
//  SessionsListView.swift
//  FlightDeckRemote
//
//  Placeholder for PRD §5.2 Agent sessions list (per project: name, agent
//  type, status, git indicators, running time). The Sessions feature team
//  fills this in.
//

import SwiftUI

struct SessionsListView: View {
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "list.bullet.rectangle")
                .font(.system(size: 48))
                .foregroundStyle(Theme.statusCyan)
            Text("Agent Sessions")
                .font(.title2.bold())
                .foregroundStyle(Theme.text)
            Text("Sessions list placeholder")
                .foregroundStyle(Theme.textMuted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("SessionsListView")
    }
}

#Preview {
    SessionsListView()
}
