//
//  RootView.swift
//  FlightDeckRemote
//
//  Renders `AppRouter.route`: the Pairing screen (full-screen, no tab bar)
//  when unpaired, or the main tab container when paired (PRD §5.8). Also
//  owns the app's deep-link entry point — `onOpenURL` hands every
//  `flightdeck-remote://` URL to `router.handleDeepLink(url:)`.
//

import SwiftUI

struct RootView: View {
    var router: AppRouter

    var body: some View {
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
    }
}

#Preview("Pairing") {
    RootView(router: AppRouter(pairingStore: PairingStore()))
}
