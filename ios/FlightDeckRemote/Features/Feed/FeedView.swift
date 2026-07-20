//
//  FeedView.swift
//  FlightDeckRemote
//
//  The unified multi-pairing feed (remote-control-b8d.8): one flat `List`
//  bound to `FeedStore.items` (remote-control-b8d.6), interleaved by recency
//  across every paired machine. Each row shows the project's status dot +
//  name + roll-up summary, tagged with a small `MachineChip` (the owning
//  machine's resolved display name); an offline machine's rows are dimmed
//  with an "Offline" badge and a separate tap-to-retry control that
//  restarts just that machine's client (`TransportCoordinator.start(pairingId:)`).
//
//  Follows `ProjectsListView`'s visual language (same header shape, card
//  style, typography, empty-state phrasing) so the new screen reads as part
//  of the same app rather than a bolt-on — see that file for the pattern
//  this one adapts to a multi-machine, `List`-based layout instead of a
//  single-machine `ScrollView`/`LazyVStack`.
//
//  Toolbar: "Add machine" (`AddMachineSheet`, remote-control-b8d.7 — always
//  tappable even at the cap; `PairingView.isBlockedByCap` shows the blocked
//  state with `PairingLimits.capReachedMessage` rather than this button
//  silently doing nothing) and an overflow entry to Settings (switches
//  `AppRouter.selectedTab`, since Settings is already a full tab rather than
//  a screen this feed would push/present).
//
//  Pull-to-refresh (`.refreshable`) resyncs every live client and retries
//  every offline one in one gesture — see `FeedRefreshPlan` for the pure
//  per-machine decision.
//
//  Navigation seam (remote-control-b8d.12): `MainTabView`'s `.feed` tab case
//  owns this screen's `NavigationStack`/`navigationDestination`, pushing the
//  SAME `ProjectsRoute` shape (and reusing `SessionsListView`/`AgentChatView`
//  verbatim) the Projects tab already does. Those detail views aren't
//  parameterized by `pairingId` yet (that's b8d.12's job), so a tapped row
//  records its `pairingId` into `activePairingId` — set here, read by
//  `MainTabView.feedDetailStore` to resolve which machine's `TransportStore`
//  the pushed destination binds to (falling back to `coordinator.primaryStore`,
//  the SAME transitional single-store bridge `TransportCoordinator.primaryStore`'s
//  own doc comment describes). b8d.12 replaces this with true
//  per-pairingId-parameterized detail views and the seam goes away.
//

import SwiftUI

struct FeedView: View {
    var feedStore: FeedStore
    var coordinator: TransportCoordinator
    var router: AppRouter
    var nav: ProjectsNavModel
    // TODO(remote-control-b8d.12): transitional per-tap seam — see file header.
    @Binding var activePairingId: String?

    @State private var isPresentingAddMachine = false

    var body: some View {
        VStack(spacing: 0) {
            header
            feedList
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("FeedView")
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            HStack(alignment: .firstTextBaseline) {
                Text("Feed")
                    .typography(Typography.largeTitle)
                    .foregroundStyle(Theme.textPrimary)
                Spacer()
                addMachineButton
                settingsButton
            }
            Text(RollupModel.aggregateSubtitle(projects: feedStore.items.map(\.project)))
                .typography(Typography.callout)
                .foregroundStyle(Theme.textMuted)
                .accessibilityIdentifier("feed-subtitle")
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.top, Theme.Spacing.lg)
        .padding(.bottom, Theme.Spacing.md)
    }

    /// "Add machine" (remote-control-b8d.7): reachable straight from the
    /// feed's own toolbar (this is the real one — `ProjectsListView`'s "+" was
    /// only today's stand-in until this screen existed). Presents the SAME
    /// shared `router.pairingStore`, so a completed add reflows the feed the
    /// moment the new machine's client connects.
    private var addMachineButton: some View {
        Button {
            isPresentingAddMachine = true
        } label: {
            Image(systemName: "plus")
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(Theme.textPrimary)
                .frame(width: 36, height: 36)
        }
        .accessibilityIdentifier("feed-add-machine-button")
        .accessibilityLabel("Add machine")
        .sheet(isPresented: $isPresentingAddMachine) {
            AddMachineSheet(pairingStore: router.pairingStore)
        }
    }

    /// Overflow entry to Settings — a tab switch (not a push/present) since
    /// Settings is already a first-class tab in `MainTabView`.
    private var settingsButton: some View {
        Button {
            router.selectedTab = .settings
        } label: {
            Image(systemName: "gearshape")
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(Theme.textPrimary)
                .frame(width: 36, height: 36)
        }
        .accessibilityIdentifier("feed-settings-button")
        .accessibilityLabel("Settings")
    }

    // MARK: - List

    private var feedList: some View {
        List {
            if feedStore.items.isEmpty {
                emptyState
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.clear)
                    .listRowSeparator(.hidden)
            } else {
                ForEach(feedStore.items) { item in
                    row(item)
                }
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .background(Theme.bgDeep)
        .refreshable {
            await resyncAll()
        }
    }

    private var emptyState: some View {
        VStack(spacing: Theme.Spacing.md) {
            Image(systemName: "antenna.radiowaves.left.and.right.slash")
                .font(.system(size: 40))
                .foregroundStyle(Theme.textDim)
            Text("Waiting for your Macs…")
                .typography(Typography.headline)
                .foregroundStyle(Theme.textPrimary)
            Text("Open FlightDeck on a paired Mac to see your projects here.")
                .typography(Typography.callout)
                .foregroundStyle(Theme.textMuted)
                .multilineTextAlignment(.center)
        }
        .padding(Theme.Spacing.xxl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityIdentifier("feed-empty-state")
    }

    // MARK: - Row

    private func row(_ item: FeedItem) -> some View {
        let vm = RollupModel.viewModel(for: item.project)
        return HStack(spacing: Theme.Spacing.sm) {
            Button {
                activePairingId = item.pairingId
                nav.path.append(.sessions(projectId: item.project.projectId.rawValue))
            } label: {
                rowContent(item: item, vm: vm)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(Text("\(item.project.name), \(item.displayName), \(vm.summary)"))

            // A SIBLING of the (dimmed, when offline) navigate button — never
            // nested inside it, so tapping retry never also navigates and
            // stays at full brightness even while the row around it dims
            // (mirrors `SettingsView.machineRow`'s name-vs-mute button split).
            if item.isOffline {
                retryButton(item)
            }
        }
        .padding(Theme.Spacing.lg)
        .cardStyle(accent: FeedRowPresentation.accentColor(dot: vm.dot, isOffline: item.isOffline))
        .listRowInsets(EdgeInsets(top: Theme.Spacing.xs, leading: Theme.Spacing.lg,
                                  bottom: Theme.Spacing.xs, trailing: Theme.Spacing.lg))
        .listRowBackground(Color.clear)
        .listRowSeparator(.hidden)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("feed-row-\(item.id)")
    }

    private func rowContent(item: FeedItem, vm: ProjectRollupViewModel) -> some View {
        HStack(spacing: Theme.Spacing.md) {
            StatusDot(status: vm.dot.agentStatus, size: .large)

            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: Theme.Spacing.xs) {
                    Text(item.project.name)
                        .typography(Typography.headline)
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)
                    MachineChip(displayName: item.displayName)
                        .accessibilityIdentifier("feed-machine-chip-\(item.id)")
                }
                HStack(spacing: Theme.Spacing.xs) {
                    Text(vm.summary)
                        .typography(Typography.callout)
                        .foregroundStyle(Theme.textMuted)
                        .lineLimit(1)
                    if item.isOffline {
                        offlineBadge(item)
                    }
                }
            }

            Spacer(minLength: Theme.Spacing.sm)

            Image(systemName: "chevron.right")
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(Theme.textDim)
        }
        .opacity(FeedRowPresentation.contentOpacity(isOffline: item.isOffline))
        .contentShape(Rectangle())
    }

    private func offlineBadge(_ item: FeedItem) -> some View {
        Text("Offline")
            .typography(Typography.captionBold)
            .foregroundStyle(Theme.textMutedDark)
            .padding(.horizontal, Theme.Spacing.sm)
            .padding(.vertical, 2)
            .overlay(
                Capsule(style: .continuous)
                    .strokeBorder(Theme.textMutedDark.opacity(0.5), lineWidth: 1)
            )
            .accessibilityIdentifier("feed-offline-badge-\(item.id)")
    }

    /// Tap-to-retry for an offline row: restarts JUST that machine's client
    /// (`TransportCoordinator.start(pairingId:)`) — the other machines' live
    /// clients are untouched (mirrors the coordinator's own per-pairing
    /// start/stop, remote-control-b8d.5).
    private func retryButton(_ item: FeedItem) -> some View {
        Button {
            Task { await coordinator.start(pairingId: item.pairingId) }
        } label: {
            Image(systemName: "arrow.clockwise")
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(Theme.accent)
                .frame(width: 36, height: 36)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Retry connecting to \(item.displayName)")
        .accessibilityIdentifier("feed-retry-button-\(item.id)")
    }

    // MARK: - Pull-to-refresh

    /// Resync every live client and retry every offline one in one gesture
    /// (issue: "Pull-to-refresh triggers a resync across live clients") — see
    /// `FeedRefreshPlan` for the pure per-machine decision this applies.
    private func resyncAll() async {
        for handle in coordinator.handles {
            switch FeedRefreshPlan.action(for: handle.store.linkState) {
            case .resync:
                handle.store.requestSnapshot()
            case .reconnect:
                await coordinator.start(pairingId: handle.pairingId)
            }
        }
    }
}

#Preview {
    let pairingStore = PairingStore()
    let coordinator = TransportStoreFactory.makeCoordinator(pairingStore: pairingStore)
    let feedStore = FeedStore(coordinator: coordinator, pairingStore: pairingStore)
    return NavigationStack {
        FeedView(
            feedStore: feedStore,
            coordinator: coordinator,
            router: AppRouter(pairingStore: pairingStore),
            nav: ProjectsNavModel(),
            activePairingId: .constant(nil)
        )
    }
}
