//
//  AppTab.swift
//  FlightDeckRemote
//
//  The bottom tab bar: Feed · Projects · Shell · Settings. The center [+] FAB
//  is deliberately *not* a case here — it never becomes "selected content",
//  it presents the New-agent flow on top of whatever tab is showing (see
//  `CustomTabBar` / `MainTabView`).
//
//  The separate Activity tab was folded into the unified `.feed`
//  (remote-control-fa8): the Feed is now the ONE surface, carrying Activity's
//  value (per-row unread tracking, attention highlighting, error surfacing,
//  event-level deep-link) across every paired machine. `AppRouter.selectedTab`
//  still defaults to `.projects` (pinned by `AppRouterTests.startsOnProjectsTab`).
//

import Foundation

enum AppTab: String, CaseIterable, Identifiable, Hashable {
    case feed
    case projects
    case shell
    case settings

    var id: String { rawValue }

    var title: String {
        switch self {
        case .feed: "Feed"
        case .projects: "Projects"
        case .shell: "Shell"
        case .settings: "Settings"
        }
    }

    /// SF Symbol per PRD §5.7 styling notes (Shell uses a terminal-style
    /// glyph; Feed uses a list glyph distinct from Projects' stack glyph).
    var systemImage: String {
        switch self {
        case .feed: "list.bullet.rectangle.portrait"
        case .projects: "square.stack.3d.up"
        case .shell: "terminal"
        case .settings: "gearshape"
        }
    }
}
