//
//  AppTab.swift
//  FlightDeckRemote
//
//  The bottom tab bar (PRD §5.7): Projects · Activity · Shell · Settings, plus
//  the multi-pairing unified Feed (remote-control-b8d.8). The center [+] FAB
//  is deliberately *not* a case here — it never becomes "selected content",
//  it presents the New-agent flow on top of whatever tab is showing (see
//  `CustomTabBar` / `MainTabView`).
//
//  `.feed` (remote-control-b8d.8) is ADDITIVE: the interleaved-by-recency,
//  multi-machine view (`FeedView`) sits alongside the existing single-machine
//  `.projects` tab rather than replacing it — `AppRouter.selectedTab` still
//  defaults to `.projects` (pinned by `AppRouterTests.startsOnProjectsTab`),
//  so existing navigation/deep-link/UI-test behavior is unchanged. A user
//  paired with 2+ Macs reaches the aggregated feed via this new leading tab.
//

import Foundation

enum AppTab: String, CaseIterable, Identifiable, Hashable {
    case feed
    case projects
    case activity
    case shell
    case settings

    var id: String { rawValue }

    var title: String {
        switch self {
        case .feed: "Feed"
        case .projects: "Projects"
        case .activity: "Activity"
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
        case .activity: "bell"
        case .shell: "terminal"
        case .settings: "gearshape"
        }
    }
}
