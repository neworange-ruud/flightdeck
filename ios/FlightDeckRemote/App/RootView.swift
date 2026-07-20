//
//  RootView.swift
//  FlightDeckRemote
//
//  Renders `AppRouter.route`: the Pairing (onboarding) screen (full-screen,
//  no tab bar) with ZERO paired instances, or the main tab container with ONE
//  OR MORE (PRD §5.8, multi-pairing remote-control-b8d.7 — `AppRouter.route`
//  is count-based off `PairingStore.hasAnyPairing`, not a binary paired flag).
//  `MainTabView` is today's stand-in main container; the real unified feed
//  (remote-control-b8d.8) replaces its content without touching this switch.
//  Also owns the app's deep-link entry point — `onOpenURL` hands every
//  `flightdeck-remote://` URL to `router.handleDeepLink(url:)`.
//
//  Also owns the Face-ID app-open gate (PRD §5.6/§9): `AppLockView` overlays
//  *everything* below — pairing and main alike — whenever `appLock.lockState
//  != .unlocked` while the gate is enabled, and `appLock.lockIfEnabled()`
//  arms the lock on the scenePhase → `.background` transition.
//
//  `appLock` is also published into the SwiftUI environment (`.environment(appLock)`
//  below) so the Settings screen's "Require Face ID to open" toggle
//  (Features/Settings/SettingsView.swift) can bind straight to
//  `AppLockController.isLockEnabled` via `@Environment(AppLockController.self)`,
//  without MainTabView threading it through every tab case as an explicit
//  parameter.
//

import SwiftUI

struct RootView: View {
    var router: AppRouter

    @State private var appLock = AppLockController()
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        ZStack {
            Group {
                switch router.route {
                case .pairing:
                    PairingView(pairingStore: router.pairingStore)
                case .main:
                    MainTabView(router: router)
                }
            }
            .onOpenURL { url in
                router.handleDeepLink(url: url)
            }

            if appLock.isLockEnabled && appLock.lockState != .unlocked {
                AppLockView(appLock: appLock)
            }
        }
        .onChange(of: scenePhase) { _, newPhase in
            if newPhase == .background {
                appLock.lockIfEnabled()
            }
        }
        .task(id: router.route) {
            // Request notification authorization + APNs registration once the
            // device is paired (PRD §9: pairing is the trust step; asking for
            // pushes before there's a Mac to hear from is premature). Idempotent
            // — the system only prompts once per install.
            if router.route == .main {
                PushCoordinator.shared.requestAuthorizationAndRegister()
            }
        }
        .environment(appLock)
    }
}

#Preview("Pairing") {
    RootView(router: AppRouter(pairingStore: PairingStore()))
}
