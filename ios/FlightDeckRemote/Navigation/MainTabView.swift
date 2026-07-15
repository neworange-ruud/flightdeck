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
//  - the FAB's sheet (`NewAgentView`, Features/Control) — the real "type +
//    name + base + first task" flow (PRD §5.5); the presentation plumbing
//    (`isPresentingNewAgentSheet`)
//    stays the same. The Sessions screen's "New agent session" CTA also
//    presents this same sheet (via the binding passed down to it) rather
//    than rebuilding its own.
//  - `connectionSource` (`ConnectionStatusSource`, `Features/Connection`) —
//    an externally-supplied override for the reconnecting banner (e.g. a
//    test double); defaults to `nil`, in which case the banner reads
//    `transportStore` below — the app's single live `TransportStore`
//    (`TransportStoreFactory`), which the Projects/Sessions screens also
//    bind to.
//  - `activityStore` also bridges `transportStore.agentEvents` in (see the
//    `.onChange` below) so it stays the Activity tab's real, always-live data
//    source regardless of which tab is currently showing (PRD §5.7).
//  - the `StaleBanner` overlay (PRD §9.2) shows whenever the store's data is
//    cache-seeded (`TransportStore.isCacheStale`) and the link isn't a live
//    `.connected` session — mounted below the (louder) `ReconnectingBanner`.
//    `isCacheStaleOffline` is also published into the environment so
//    sibling screens can read it later without further plumbing.
//

import SwiftUI

struct MainTabView: View {
    var router: AppRouter
    var connectionSource: (any ConnectionStatusSource)?

    @State private var projectsNav = ProjectsNavModel()
    @State private var activityStore = ActivityStore.makeDefault()
    @State private var isPresentingNewAgentSheet = false
    @State private var connectionBanner: ReconnectingBannerModel
    @State private var transportStore: TransportStore

    init(router: AppRouter, connectionSource: (any ConnectionStatusSource)? = nil) {
        self.router = router
        self.connectionSource = connectionSource
        let store = TransportStoreFactory.makeDefault()
        _transportStore = State(initialValue: store)
        _connectionBanner = State(initialValue: ReconnectingBannerModel(source: connectionSource ?? store))
    }

    var body: some View {
        ZStack(alignment: .top) {
            ZStack {
                tabContent
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .environment(\.isCacheStaleOffline, isStaleBannerVisible)
                    // `.safeAreaInset` (rather than overlaying the tab bar as a
                    // ZStack sibling drawn on top) folds the bar's own height
                    // into `tabContent`'s bottom safe area. That matters for
                    // tabs with a fixed, non-scrolling bottom control — e.g.
                    // the Shell tab's `ShellKeyBar` (mounted via its own
                    // `.safeAreaInset(edge: .bottom)` in `ShellView`) — since
                    // nested safe-area insets stack: without this, the tab
                    // bar's opaque background + buttons render *on top of*
                    // whatever sits at the bottom of `tabContent`, at the same
                    // z-order priority a plain overlay would occupy, making
                    // that control unhittable. Hide the inset entirely while a
                    // chat conversation is open: the chat screen's compose bar
                    // (PRD §5.3) owns the bottom edge there instead.
                    .safeAreaInset(edge: .bottom) {
                        if !isChatRouteActive {
                            CustomTabBar(
                                selectedTab: router.selectedTab,
                                unreadActivityCount: activityStore.unreadCount,
                                onSelectTab: { router.selectedTab = $0 },
                                onTapFAB: { isPresentingNewAgentSheet = true }
                            )
                        }
                    }
            }
            .background(Theme.bgDeep)
            .sheet(isPresented: $isPresentingNewAgentSheet) {
                NewAgentView(store: transportStore)
            }
            .onChange(of: router.selectedTab) { _, newTab in
                if newTab == .activity {
                    activityStore.markViewed()
                }
            }
            .onChange(of: transportStore.agentEvents) { _, newEvents in
                activityStore.ingest(newEvents, tabSelected: router.selectedTab == .activity)
            }
            // `.contain` first: an accessibility identifier applied to a plain
            // container view propagates onto every accessibility element inside
            // it, clobbering the tab buttons' own identifiers. Making the view a
            // container element scopes the identifier to the container itself.
            // The reconnecting banner sits *outside* this scope (below) so its
            // own identifiers aren't swallowed by "MainTabView" either.
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("MainTabView")
            .task {
                await transportStore.start()
            }

            VStack(spacing: 0) {
                ReconnectingBanner(model: connectionBanner, isPaired: router.pairingStore.isPaired)
                if isStaleBannerVisible {
                    StaleBanner()
                        .transition(.move(edge: .top).combined(with: .opacity))
                }
            }
            .animation(.easeInOut(duration: 0.25), value: isStaleBannerVisible)
        }
    }

    /// Whether the Projects tab currently has a chat route pushed (the tab bar
    /// hides there — see the comment at the `CustomTabBar` mount).
    private var isChatRouteActive: Bool {
        router.selectedTab == .projects && projectsNav.path.contains {
            if case .chat = $0 { return true }
            return false
        }
    }

    /// The link state driving the stale banner: the DEBUG `-uitest-linkstate`
    /// forced state wins (mirrors `ReconnectingBannerModel`/`CommandsPausedGate`'s
    /// own DEBUG seam), so UI tests can drive it deterministically without a
    /// real relay connection.
    private var effectiveLinkStateForStaleBanner: RemoteLinkState {
        #if DEBUG
        if let forced = ConnectionDebugSeam.forcedLinkState() { return forced }
        #endif
        return transportStore.linkState
    }

    /// PRD §9.2: cache-seeded data, shown only while the link isn't a live
    /// `.connected` session — reuses `ReconnectingBannerModel.isDown` rather
    /// than redefining "down".
    private var isStaleBannerVisible: Bool {
        transportStore.isCacheStale && ReconnectingBannerModel.isDown(effectiveLinkStateForStaleBanner)
    }

    @ViewBuilder
    private var tabContent: some View {
        switch router.selectedTab {
        case .projects:
            NavigationStack(path: $projectsNav.path) {
                ProjectsListView(transportStore: transportStore, router: router, nav: projectsNav)
                    .navigationDestination(for: ProjectsRoute.self) { route in
                        switch route {
                        case let .sessions(projectId):
                            SessionsListView(
                                projectId: Wire.ProjectId(projectId),
                                transportStore: transportStore,
                                nav: projectsNav,
                                isPresentingNewAgentSheet: $isPresentingNewAgentSheet
                            )
                        case let .chat(projectId, sessionId):
                            // Not passing `store:` here (yet) is deliberate: Chat's
                            // own `-uitest-fixture-transcript` UI tests rely on a
                            // `nil` store to fall back to `loadFixtureIfRequested()`
                            // (see `ChatViewModel.swift`/`ChatFixtureAutoPush.swift`).
                            // Wiring the live `transportStore` through here is
                            // Chat's own integration seam to complete.
                            AgentChatView(projectId: projectId, sessionId: sessionId)
                        }
                    }
                    .chatFixtureAutoPush(path: $projectsNav.path)
            }
        case .activity:
            ActivityFeedView(transportStore: transportStore, activityStore: activityStore, router: router)
        case .shell:
            ShellTabView(transportStore: transportStore)
        case .settings:
            SettingsView(router: router, transportStore: transportStore)
        }
    }
}

#Preview {
    MainTabView(router: AppRouter(pairingStore: PairingStore()))
}
