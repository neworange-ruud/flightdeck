//
//  RootView.swift
//  FlightDeckRemote
//
//  Renders `AppRouter.route`: the Pairing screen (full-screen, no tab bar)
//  when unpaired, or the main tab container when paired (PRD §5.8). Also
//  owns the app's deep-link entry point — `onOpenURL` hands every
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
        .environment(appLock)
    }
}

#Preview("Pairing") {
    RootView(router: AppRouter(pairingStore: PairingStore()))
}
