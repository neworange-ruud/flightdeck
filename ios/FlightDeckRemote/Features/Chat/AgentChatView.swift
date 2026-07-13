//
//  AgentChatView.swift
//  FlightDeckRemote
//
//  Placeholder for PRD §5.3 Agent chat — the cleaned-transcript surface with
//  inline permission asks, activity pills, and voice compose. The Chat
//  feature team fills this in.
//

import SwiftUI

struct AgentChatView: View {
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "bubble.left.and.bubble.right")
                .font(.system(size: 48))
                .foregroundStyle(Theme.orange)
            Text("Agent Chat")
                .font(.title2.bold())
                .foregroundStyle(Theme.text)
            Text("Agent chat placeholder")
                .foregroundStyle(Theme.textMuted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("AgentChatView")
    }
}

#Preview {
    AgentChatView()
}
