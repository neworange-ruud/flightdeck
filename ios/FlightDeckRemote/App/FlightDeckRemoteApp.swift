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
    @State private var pairingStore = PairingStore()

    var body: some Scene {
        WindowGroup {
            AppRouter(pairingStore: pairingStore)
                .preferredColorScheme(.dark)
                .componentGalleryDebugEntry()
        }
    }
}
