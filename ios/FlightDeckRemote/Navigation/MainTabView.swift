//
//  MainTabView.swift
//  FlightDeckRemote
//
//  The paired app's tab container (PRD §5.7): Projects · Activity ·
//  [+ FAB] · Shell · Settings, rendered as a bottom-bar overlay + switch on
//  `router.selectedTab` (see `CustomTabBar`'s doc comment for why this isn't
//  a plain `TabView`).
//
//  Hook points for later feature tasks:
//  - `projectsNav.path` (`ProjectsNavModel`, typed `[ProjectsRoute]`) — the
//    Projects tab's `NavigationStack` path, for pushing sessions/chat.
//  - `activityStore.unreadCount` — the Activity tab's unread badge, cleared
//    here when the tab is selected.
//  - the FAB's sheet (`NewAgentPlaceholderSheet`) — the New-Agent feature
//    task replaces its content with the real "type + name + base + first
//    task" flow (PRD §5.5); the presentation plumbing (`isPresentingNewAgentSheet`)
//    stays the same.
//

import SwiftUI

struct MainTabView: View {
    var router: AppRouter

    @State private var projectsNav = ProjectsNavModel()
    @State private var activityStore = ActivityStore()
    @State private var isPresentingNewAgentSheet = false

    var body: some View {
        ZStack {
            tabContent
                .frame(maxWidth: .infinity, maxHeight: .infinity)

            VStack(spacing: 0) {
                Spacer()
                CustomTabBar(
                    selectedTab: router.selectedTab,
                    unreadActivityCount: activityStore.unreadCount,
                    onSelectTab: { router.selectedTab = $0 },
                    onTapFAB: { isPresentingNewAgentSheet = true }
                )
            }
        }
        .background(Theme.bgDeep)
        .sheet(isPresented: $isPresentingNewAgentSheet) {
            NewAgentPlaceholderSheet()
        }
        .onChange(of: router.selectedTab) { _, newTab in
            if newTab == .activity {
                activityStore.markViewed()
            }
        }
        // `.contain` first: an accessibility identifier applied to a plain
        // container view propagates onto every accessibility element inside
        // it, clobbering the tab buttons' own identifiers. Making the view a
        // container element scopes the identifier to the container itself.
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("MainTabView")
    }

    @ViewBuilder
    private var tabContent: some View {
        switch router.selectedTab {
        case .projects:
            NavigationStack(path: $projectsNav.path) {
                ProjectsListView()
                    .navigationDestination(for: ProjectsRoute.self) { route in
                        switch route {
                        case .sessions:
                            SessionsListView()
                        case .chat:
                            AgentChatView()
                        }
                    }
            }
        case .activity:
            ActivityFeedView()
        case .shell:
            ShellTerminalView()
        case .settings:
            SettingsView()
        }
    }
}

/// Placeholder presented by the center FAB (PRD §5.5 "New agent" screen).
/// The New-Agent feature task replaces this with the real flow (agent type,
/// name, base branch, first task).
private struct NewAgentPlaceholderSheet: View {
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            VStack(spacing: Theme.Spacing.lg) {
                Image(systemName: "plus.circle.fill")
                    .font(.system(size: 48))
                    .foregroundStyle(Theme.accent)
                Text("New agent session")
                    .typography(Typography.title)
                    .foregroundStyle(Theme.textPrimary)
                Text("New-agent flow placeholder")
                    .typography(Typography.body)
                    .foregroundStyle(Theme.textMuted)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Theme.bgDeep)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Close") { dismiss() }
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("NewAgentPlaceholderSheet")
    }
}

#Preview {
    MainTabView(router: AppRouter(pairingStore: PairingStore()))
}
