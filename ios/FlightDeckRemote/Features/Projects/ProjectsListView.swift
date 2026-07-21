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
//  This screen is ALSO today's stand-in for the unified feed's toolbar
//  (remote-control-b8d.8 owns the real one): its header's "+" presents
//  `AddMachineSheet` (remote-control-b8d.7), reachable while already paired.
//

import SwiftUI

struct ProjectsListView: View {
    var transportStore: TransportStore
    /// When set, the list AGGREGATES projects across every paired instance
    /// (matching the Feed), instead of showing only `transportStore`'s single
    /// machine — otherwise the Projects tab silently hides every project on all
    /// but one machine (remote-control-aj2). `nil` in previews/tests keeps the
    /// simple single-store behaviour.
    var coordinator: TransportCoordinator? = nil
    var router: AppRouter
    var nav: ProjectsNavModel

    @State private var isSearching = false
    @State private var searchQuery = ""
    // Add-machine (remote-control-b8d.7) is reached from the Feed toolbar
    // (feed-add-machine-button) and Settings (settings-add-machine-button);
    // the earlier stand-in "+" here was removed because a second
    // `.sheet(isPresented:)` in this NavigationStack silently swallowed the
    // shared New-Agent sheet (ProjectsSessionsUITests.testNewAgentCTAOpens…).
    @FocusState private var searchFieldFocused: Bool

    /// One project row, tagged with the machine (pairing) it belongs to so a tap
    /// binds the detail screens to that machine's store. `pairingId == nil` in
    /// the single-store fallback (no coordinator), where the route ignores it.
    private struct Row: Identifiable {
        let project: Wire.ProjectState
        let pairingId: String?
        var id: String { (pairingId ?? "-") + "\u{1f}" + project.projectId.rawValue }
    }

    private var allRows: [Row] {
        // Aggregate across paired machines when a coordinator with live handles
        // is present. With no handles (previews, UI-test fixtures, and the
        // recordless single-store fallback all seed `transportStore` directly),
        // fall back to the single store so those paths keep working.
        if let coordinator, !coordinator.handles.isEmpty {
            return coordinator.handles.flatMap { handle in
                (handle.store.snapshot?.projects ?? [])
                    .map { Row(project: $0, pairingId: handle.pairingId) }
            }
        }
        return (transportStore.snapshot?.projects ?? [])
            .map { Row(project: $0, pairingId: nil) }
    }

    private var visibleRows: [Row] {
        let trimmed = searchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return allRows }
        return allRows.filter {
            $0.project.name.range(of: trimmed, options: .caseInsensitive) != nil
        }
    }

    private var allProjects: [Wire.ProjectState] { allRows.map(\.project) }

    /// Whether any paired machine has delivered a snapshot yet (drives the
    /// waiting/empty state across all instances, not just one).
    private var hasAnySnapshot: Bool {
        if let coordinator, !coordinator.handles.isEmpty {
            return coordinator.handles.contains { $0.store.snapshot != nil }
        }
        return transportStore.snapshot != nil
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
        if !hasAnySnapshot {
            emptyState
        } else if visibleRows.isEmpty {
            noSearchResultsState
        } else {
            projectList
        }
    }

    private var projectList: some View {
        ScrollView {
            LazyVStack(spacing: Theme.Spacing.md) {
                ForEach(visibleRows) { row in
                    projectCard(row)
                }
            }
            .padding(Theme.Spacing.lg)
        }
        .refreshable { await refresh() }
    }

    /// Pull-to-refresh: force a fresh snapshot from every paired desktop so a
    /// stale/last-known list is replaced (remote-control-aj2). These screens
    /// previously had no refresh at all, so a pull-down did nothing.
    private func refresh() async {
        if let coordinator, !coordinator.handles.isEmpty {
            for handle in coordinator.handles { handle.store.requestSnapshot() }
        } else {
            transportStore.requestSnapshot()
        }
        // Let the request round-trip so the spinner reflects real work.
        try? await Task.sleep(for: .milliseconds(600))
    }

    private func projectCard(_ row: Row) -> some View {
        let project = row.project
        let vm = RollupModel.viewModel(for: project)
        return Button {
            nav.path.append(.sessions(projectId: project.projectId.rawValue, pairingId: row.pairingId))
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

    /// The link state to phrase the waiting copy against: with a coordinator,
    /// the most-alive state across all paired machines (any connected wins, then
    /// any mid-handshake) so the copy isn't pinned to one machine; otherwise the
    /// single store's state.
    private var effectiveLinkState: RemoteLinkState {
        guard let coordinator, !coordinator.handles.isEmpty else {
            return transportStore.linkState
        }
        let states = coordinator.handles.map(\.store.linkState)
        if states.contains(where: { if case .connected = $0 { true } else { false } }) {
            return .connected(latencyMs: 0)
        }
        if states.contains(where: { $0 == .connecting || $0 == .authenticating }) {
            return .connecting
        }
        return .disconnected
    }

    /// Phrased honestly against the real link state — never claims to be
    /// "connected" or "waiting" when the truth is different.
    private var waitingTitle: String {
        switch effectiveLinkState {
        case .disconnected: "Waiting for desktop…"
        case .connecting: "Connecting…"
        case .authenticating: "Connecting…"
        case .connected: "Waiting for desktop…"
        }
    }

    private var waitingSubtitle: String {
        switch effectiveLinkState {
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
