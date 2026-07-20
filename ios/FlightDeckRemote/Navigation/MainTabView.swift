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
    // The Feed tab's own `NavigationStack` path (remote-control-b8d.8) —
    // reuses `ProjectsNavModel`/`ProjectsRoute` rather than inventing a
    // parallel type, since a feed row pushes into the SAME `SessionsListView`/
    // `AgentChatView` surfaces the Projects tab already does (see `tabContent`'s
    // `.feed` case). Each pushed route now carries its OWN `pairingId`
    // (remote-control-b8d.12), resolved to a store per-destination via
    // `coordinator.detailStore(for:)` — there is no separate "which machine
    // is active" state to go stale as the stack grows or a different row is
    // tapped later.
    @State private var feedNav = ProjectsNavModel()
    @State private var activityStore = ActivityStore.makeDefault()
    @State private var isPresentingNewAgentSheet = false
    // Measured height of the custom tab bar, published into `tabContent`'s
    // environment (`\.tabBarHeight`) so screens pushed inside the
    // Projects/Feed `NavigationStack`s — which do NOT inherit the tab bar's
    // `.safeAreaInset` — can reserve matching bottom space and keep their
    // bottom-pinned controls above (and hittable, not under) the bar.
    @State private var tabBarHeight: CGFloat = 0
    @State private var connectionBanner: ReconnectingBannerModel
    // Multi-pairing transport (remote-control-b8d.5): the coordinator owns one
    // live client+store per paired machine and is driven foreground→connect-all
    // / background→teardown by the `scenePhase` observer below. `transportStore`
    // is the transitional single-store bridge (`coordinator.primaryStore` — the
    // first paired instance, or a recordless fallback when unpaired) that the
    // Projects/Activity/Shell/Settings tabs deliberately KEEP binding to
    // (single-store, transitional — out of scope for remote-control-b8d.12,
    // which only finalizes the FEED tab's per-pairingId navigation below).
    @State private var coordinator: TransportCoordinator
    @State private var transportStore: TransportStore
    // Unified multi-pairing feed (remote-control-b8d.8): the aggregation over
    // the SAME coordinator's handles, folded with `router.pairingStore` for
    // display-name/online-flag resolution (remote-control-b8d.6). Owned here
    // (not by `FeedView` itself) so it's built exactly once, alongside
    // `coordinator`, rather than re-built on every `.feed` tab selection.
    @State private var feedStore: FeedStore
    @Environment(\.scenePhase) private var scenePhase
    // Push wiring (PRD §5.2/§9.1): the notification prefs the Settings screen
    // binds to (also gating presentation), the local-notification scheduler fed
    // by the same `agentEvents` stream as the Activity feed, and the shared push
    // coordinator whose APNs token we register with the transport.
    @State private var notificationPreferences = NotificationPreferences()
    @State private var notificationScheduler = NotificationScheduler()
    @State private var pushCoordinator = PushCoordinator.shared

    init(router: AppRouter, connectionSource: (any ConnectionStatusSource)? = nil) {
        self.router = router
        self.connectionSource = connectionSource
        let coordinator = TransportStoreFactory.makeCoordinator(pairingStore: router.pairingStore)
        _coordinator = State(initialValue: coordinator)
        let store = coordinator.primaryStore
        _transportStore = State(initialValue: store)
        _feedStore = State(initialValue: FeedStore(coordinator: coordinator, pairingStore: router.pairingStore))
        _connectionBanner = State(initialValue: ReconnectingBannerModel(source: connectionSource ?? store))
    }

    var body: some View {
        ZStack(alignment: .top) {
            ZStack {
                tabContent
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .environment(\.isCacheStaleOffline, isStaleBannerVisible)
                    .environment(\.tabBarHeight, isChatRouteActive ? 0 : tabBarHeight)
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
                            .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { height in
                                tabBarHeight = height
                            }
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
                // Same stream drives local notifications (deduped by event_id,
                // gated by the user's toggles + per-project mute). Stamp the
                // originating machine so a tap deep-links to it (multi-pairing
                // push, remote-control-b8d.10). The transitional single-store
                // bridge feeds the primary (first) machine's events until
                // remote-control-b8d.12 fans notifications per-instance.
                notificationScheduler.ingest(
                    newEvents,
                    settings: notificationPreferences.settings,
                    pairingId: coordinator.activePairingIds.first)
            }
            .onChange(of: pushCoordinator.deviceTokenHex) { _, token in
                if let token {
                    registerPushTokenEverywhere(token)
                }
            }
            // Foreground → connect every paired machine; background → tear them
            // all down (cancel supervisors, close sockets). APNs push takes over
            // while backgrounded (remote-control-b8d.5 / epic connectivity model).
            .onChange(of: scenePhase) { _, phase in
                Task { await coordinator.setForeground(phase == .active) }
            }
            // Runtime add/remove: pairing a new machine spins up only its client;
            // unpairing one stops+disposes only that client (remote-control-b8d.7/.11).
            .onChange(of: router.pairingStore.instances) { _, instances in
                Task { await coordinator.reconcile(with: instances) }
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
                // Connect every paired machine on mount (scenePhase is `.active`
                // here); the `scenePhase` observer drives subsequent transitions.
                await coordinator.setForeground(true)
                // Register any token that already arrived before the transport
                // started (onChange covers tokens that arrive afterwards).
                if let token = pushCoordinator.deviceTokenHex {
                    registerPushTokenEverywhere(token)
                }
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

    /// Register the APNs token with every live per-machine client, applying
    /// each machine's mute preference (remote-control-b8d.10): an unmuted
    /// machine registers its own per-pairing token; a muted one is (kept)
    /// deregistered. Idempotent — safe to call on every token refresh / mount.
    /// No-op when nothing is paired.
    private func registerPushTokenEverywhere(_ token: String) {
        coordinator.registerPushToken(token, environment: pushCoordinator.environment)
    }

    /// Whether the currently-selected tab has a chat route pushed (the tab
    /// bar hides there — see the comment at the `CustomTabBar` mount). Checks
    /// both stacks that can push `.chat` — Projects' own and the Feed tab's
    /// (remote-control-b8d.8) — since either can land on `AgentChatView`.
    private var isChatRouteActive: Bool {
        let path: [ProjectsRoute]
        switch router.selectedTab {
        case .projects: path = projectsNav.path
        case .feed: path = feedNav.path
        case .activity, .shell, .settings: return false
        }
        return path.contains {
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
        case .feed:
            // Unified multi-pairing feed (remote-control-b8d.8): its own
            // `NavigationStack`/`navigationDestination`, structured exactly
            // like the Projects tab's below (same `ProjectsRoute` shape,
            // same `SessionsListView`/`AgentChatView` destinations) — the
            // only difference is which store those destinations bind to,
            // resolved per-route via `coordinator.detailStore(for:)` (see
            // its doc comment) rather than always `transportStore`.
            NavigationStack(path: $feedNav.path) {
                FeedView(
                    feedStore: feedStore,
                    coordinator: coordinator,
                    router: router,
                    nav: feedNav
                )
                .navigationDestination(for: ProjectsRoute.self) { route in
                    switch route {
                    case let .sessions(projectId, pairingId):
                        SessionsListView(
                            projectId: Wire.ProjectId(projectId),
                            transportStore: coordinator.detailStore(for: pairingId),
                            nav: feedNav,
                            isPresentingNewAgentSheet: $isPresentingNewAgentSheet,
                            pairingId: pairingId
                        )
                    case let .chat(projectId, sessionId, pairingId):
                        AgentChatView(projectId: projectId, sessionId: sessionId,
                                      store: coordinator.detailStore(for: pairingId))
                    }
                }
            }
        case .projects:
            NavigationStack(path: $projectsNav.path) {
                ProjectsListView(transportStore: transportStore, router: router, nav: projectsNav)
                    .navigationDestination(for: ProjectsRoute.self) { route in
                        switch route {
                        case let .sessions(projectId, _):
                            // Projects tab stays single-store/transitional
                            // (remote-control-b8d.12 scope is the Feed tab) —
                            // the route's `pairingId` is always `nil` here
                            // (see every push site on this tab) and ignored;
                            // always binds `transportStore`.
                            SessionsListView(
                                projectId: Wire.ProjectId(projectId),
                                transportStore: transportStore,
                                nav: projectsNav,
                                isPresentingNewAgentSheet: $isPresentingNewAgentSheet
                            )
                        case let .chat(projectId, sessionId, _):
                            // Thread the live store so Chat binds its commands-paused
                            // gate + transcript to the real (connected) transport —
                            // without it, `ChatViewModel.bind` never runs and the gate
                            // defaults to paused, showing "paused — reconnecting" and
                            // disabling send even while the link is up (remote-control-9yv).
                            // The `-uitest-fixture-transcript` tests are unaffected:
                            // `loadFixtureIfRequested()` wins and returns before the
                            // store is bound (see `AgentChatView.task`).
                            AgentChatView(projectId: projectId, sessionId: sessionId,
                                          store: transportStore)
                        }
                    }
                    .chatFixtureAutoPush(path: $projectsNav.path)
            }
        case .activity:
            ActivityFeedView(transportStore: transportStore, activityStore: activityStore, router: router)
        case .shell:
            ShellTabView(transportStore: transportStore)
        case .settings:
            SettingsView(
                router: router,
                transportStore: transportStore,
                coordinator: coordinator,
                notificationPreferences: notificationPreferences)
        }
    }
}

#Preview {
    MainTabView(router: AppRouter(pairingStore: PairingStore()))
}
