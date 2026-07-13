//
//  AppTab.swift
//  FlightDeckRemote
//
//  The four real tabs in the bottom tab bar (PRD §5.7): Projects · Activity ·
//  Shell · Settings. The center [+] FAB is deliberately *not* a case here —
//  it never becomes "selected content", it presents the New-agent flow on
//  top of whatever tab is showing (see `CustomTabBar` / `MainTabView`).
//

import Foundation

enum AppTab: String, CaseIterable, Identifiable, Hashable {
    case projects
    case activity
    case shell
    case settings

    var id: String { rawValue }

    var title: String {
        switch self {
        case .projects: "Projects"
        case .activity: "Activity"
        case .shell: "Shell"
        case .settings: "Settings"
        }
    }

    /// SF Symbol per PRD §5.7 styling notes (Shell uses a terminal-style
    /// glyph).
    var systemImage: String {
        switch self {
        case .projects: "square.stack.3d.up"
        case .activity: "bell"
        case .shell: "terminal"
        case .settings: "gearshape"
        }
    }
}
