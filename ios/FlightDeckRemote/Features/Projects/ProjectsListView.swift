//
//  ProjectsListView.swift
//  FlightDeckRemote
//
//  Placeholder for PRD §5.2 Projects list (title "Projects", roll-up
//  subtitle, one card per project). The Projects feature team fills this in.
//

import SwiftUI

struct ProjectsListView: View {
    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "square.stack.3d.up")
                .font(.system(size: 48))
                .foregroundStyle(Theme.statusGreen)
            Text("Projects")
                .font(.title2.bold())
                .foregroundStyle(Theme.text)
            Text("Projects list placeholder")
                .foregroundStyle(Theme.textMuted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.background)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("ProjectsListView")
    }
}

#Preview {
    ProjectsListView()
}
