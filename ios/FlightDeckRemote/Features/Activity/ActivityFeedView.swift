//
//  ActivityFeedView.swift
//  FlightDeckRemote
//
//  Placeholder for PRD §5.7 Activity tab — chronological feed of status
//  events (finished / needs input / errors), each deep-linking to the
//  relevant agent. The Activity feature team fills this in.
//

import SwiftUI

struct ActivityFeedView: View {
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "bell.badge")
                .font(.system(size: 48))
                .foregroundStyle(Theme.statusRed)
            Text("Activity")
                .font(.title2.bold())
                .foregroundStyle(Theme.text)
            Text("Activity feed placeholder")
                .foregroundStyle(Theme.textMuted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("ActivityFeedView")
    }
}

#Preview {
    ActivityFeedView()
}
