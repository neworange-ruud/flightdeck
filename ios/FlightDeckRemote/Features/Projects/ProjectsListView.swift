//
//  ProjectsListView.swift
//  FlightDeckRemote
//
//  PRD §5.2 Projects list: title "Projects" + an aggregate roll-up subtitle
//  ("1 project needs you · 2 working"), one card per project (glowing status
//  dot, colored left accent when it needs input, roll-up summary, agent
//  count, chevron), a top-right search affordance that filters by name, and
//  an honestly-phrased empty state before the first snapshot arrives.
//
//  Binds `TransportStore.snapshot` (Consume: Transport) and also consumes
//  `AppRouter.pendingDeepLink` (PRD §5.2/§5.7): a notification-tap deep link
//  is translated (`ProjectsDeepLinkTranslator`) into a `ProjectsNavModel`
//  path push once the linked project/session is known, then cleared either
//  way — see that file's doc comment for the unknown-id behavior.
//

import SwiftUI

struct ProjectsListView: View {
    var transportStore: TransportStore
    var router: AppRouter
    var nav: ProjectsNavModel

    @State private var isSearching = false
    @State private var searchQuery = ""
    @FocusState private var searchFieldFocused: Bool

    private var allProjects: [Wire.ProjectState] {
        transportStore.snapshot?.projects ?? []
    }

    private var visibleProjects: [Wire.ProjectState] {
        ProjectsSearch.filter(projects: allProjects, query: searchQuery)
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            content
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep)
        .task(id: router.pendingDeepLink) {
            consumeDeepLinkIfNeeded()
        }
        .onChange(of: transportStore.snapshot) { _, _ in
            // A deep link that arrived before the first snapshot couldn't be
            // validated yet — retry now that project/session data exists.
            consumeDeepLinkIfNeeded()
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("ProjectsListView")
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            HStack(alignment: .firstTextBaseline) {
                Text("Projects")
                    .typography(Typography.largeTitle)
                    .foregroundStyle(Theme.textPrimary)
                Spacer()
                searchToggleButton
            }

            if isSearching {
                searchField
            } else {
                Text(RollupModel.aggregateSubtitle(projects: allProjects))
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.textMuted)
                    .accessibilityIdentifier("projects-subtitle")
            }
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.top, Theme.Spacing.lg)
        .padding(.bottom, Theme.Spacing.md)
    }

    private var searchToggleButton: some View {
        Button {
            withAnimation(.easeOut(duration: 0.15)) {
                isSearching.toggle()
                if isSearching {
                    searchFieldFocused = true
                } else {
                    searchQuery = ""
                }
            }
        } label: {
            Image(systemName: isSearching ? "xmark.circle.fill" : "magnifyingglass")
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(Theme.textPrimary)
                .frame(width: 36, height: 36)
        }
        .accessibilityIdentifier("projects-search-toggle")
        .accessibilityLabel(isSearching ? "Close search" : "Search projects")
    }

    private var searchField: some View {
        HStack(spacing: Theme.Spacing.sm) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(Theme.textDim)
            TextField("Search projects", text: $searchQuery)
                .typography(Typography.body)
                .foregroundStyle(Theme.textPrimary)
                .focused($searchFieldFocused)
                .accessibilityIdentifier("projects-search-field")
        }
        .padding(.horizontal, Theme.Spacing.md)
        .padding(.vertical, Theme.Spacing.sm)
        .background(Theme.bgField, in: RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
    }

    // MARK: - Content

    @ViewBuilder
    private var content: some View {
        if transportStore.snapshot == nil {
            emptyState
        } else if visibleProjects.isEmpty {
            noSearchResultsState
        } else {
            projectList
        }
    }

    private var projectList: some View {
        ScrollView {
            LazyVStack(spacing: Theme.Spacing.md) {
                ForEach(visibleProjects, id: \.projectId) { project in
                    projectCard(project)
                }
            }
            .padding(Theme.Spacing.lg)
        }
    }

    private func projectCard(_ project: Wire.ProjectState) -> some View {
        let vm = RollupModel.viewModel(for: project)
        return Button {
            nav.path.append(.sessions(projectId: project.projectId.rawValue))
        } label: {
            HStack(spacing: Theme.Spacing.md) {
                StatusDot(status: vm.dot.agentStatus, size: .large)

                VStack(alignment: .leading, spacing: 4) {
                    Text(project.name)
                        .typography(Typography.headline)
                        .foregroundStyle(Theme.textPrimary)
                    Text(vm.summary)
                        .typography(Typography.callout)
                        .foregroundStyle(Theme.textMuted)
                }

                Spacer(minLength: Theme.Spacing.sm)

                Image(systemName: "chevron.right")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Theme.textDim)
            }
            .padding(Theme.Spacing.lg)
            .cardStyle(accent: vm.dot == .needsInput ? vm.dotColor : nil)
        }
        .buttonStyle(.card)
        .accessibilityIdentifier("project-card-\(project.projectId.rawValue)")
    }

    // MARK: - Empty / no-results states

    private var emptyState: some View {
        VStack(spacing: Theme.Spacing.md) {
            Image(systemName: "antenna.radiowaves.left.and.right.slash")
                .font(.system(size: 40))
                .foregroundStyle(Theme.textDim)
            Text(waitingTitle)
                .typography(Typography.headline)
                .foregroundStyle(Theme.textPrimary)
            Text(waitingSubtitle)
                .typography(Typography.callout)
                .foregroundStyle(Theme.textMuted)
                .multilineTextAlignment(.center)
        }
        .padding(Theme.Spacing.xxl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityIdentifier("projects-empty-state")
    }

    /// Phrased honestly against the real link state — never claims to be
    /// "connected" or "waiting" when the truth is different.
    private var waitingTitle: String {
        switch transportStore.linkState {
        case .disconnected: "Waiting for desktop…"
        case .connecting: "Connecting…"
        case .authenticating: "Connecting…"
        case .connected: "Waiting for desktop…"
        }
    }

    private var waitingSubtitle: String {
        switch transportStore.linkState {
        case .disconnected: "Open FlightDeck on your Mac to see your projects here."
        case .connecting: "Reaching the relay…"
        case .authenticating: "Confirming this device…"
        case .connected: "Fetching your projects…"
        }
    }

    private var noSearchResultsState: some View {
        VStack(spacing: Theme.Spacing.sm) {
            Text("No projects match “\(searchQuery)”")
                .typography(Typography.callout)
                .foregroundStyle(Theme.textMuted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(Theme.Spacing.xxl)
        .accessibilityIdentifier("projects-no-search-results")
    }

    // MARK: - Deep link

    private func consumeDeepLinkIfNeeded() {
        guard let link = router.pendingDeepLink else { return }
        if let path = ProjectsDeepLinkTranslator.path(for: link, in: transportStore.snapshot) {
            nav.path = path
        }
        router.pendingDeepLink = nil
    }
}

#Preview {
    NavigationStack {
        ProjectsListView(
            transportStore: {
                let store = TransportStoreFactory.makeDefault(arguments: [])
                #if DEBUG
                store.debugSeed(snapshot: .uiTestFixture)
                #endif
                return store
            }(),
            router: AppRouter(pairingStore: PairingStore()),
            nav: ProjectsNavModel()
        )
    }
}
