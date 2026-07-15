//
//  FlightDeckRemoteApp.swift
//  FlightDeckRemote
//
//  App entry point. The app is dark-only (see UIUserInterfaceStyle in
//  Info.plist) — `.preferredColorScheme(.dark)` reinforces this in-process
//  (e.g. for SwiftUI previews and any Info.plist override edge cases).
//
//  `.componentGalleryDebugEntry()` (DesignSystem/Gallery) adds a DEBUG-only
//  floating button that opens the DesignSystem's ComponentGallery — the
//  design-system task's acceptance surface — regardless of pairing state.
//  It's a no-op in Release builds.
//

import SwiftUI

@main
struct FlightDeckRemoteApp: App {
    // Adopt a UIKit delegate purely for the push-notification callbacks SwiftUI
    // doesn't surface (registration, wake pushes, tap handling) — see
    // `AppDelegate` / `PushCoordinator`.
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var router = AppRouter(pairingStore: PairingStore())

    var body: some Scene {
        WindowGroup {
            RootView(router: router)
                .preferredColorScheme(.dark)
                .componentGalleryDebugEntry()
                .task {
                    // Give the push tap-router the live router as soon as the
                    // scene mounts (safe before pairing; a tap can only arrive
                    // once notifications exist).
                    PushCoordinator.shared.attach(router: router)
                }
        }
    }
}
