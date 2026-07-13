//
//  ComponentGalleryDebugEntry.swift
//  FlightDeckRemote
//
//  Makes the ComponentGallery reachable in DEBUG builds without depending on
//  pairing state (the gallery is a design-system acceptance surface, not a
//  real feature — it needs to be reachable whether or not a device is
//  paired). Adds a small floating launcher button that presents the gallery
//  in a sheet; a no-op in Release builds.
//
//  Wired up from FlightDeckRemoteApp.swift via `.componentGalleryDebugEntry()`.
//

import SwiftUI

extension View {
    /// DEBUG-only floating entry point to the component gallery. No-op in
    /// Release builds.
    func componentGalleryDebugEntry() -> some View {
        #if DEBUG
        modifier(ComponentGalleryDebugEntryModifier())
        #else
        self
        #endif
    }
}

#if DEBUG
private struct ComponentGalleryDebugEntryModifier: ViewModifier {
    @State private var isPresented = false

    func body(content: Content) -> some View {
        content
            .overlay(alignment: .bottomTrailing) {
                Button {
                    isPresented = true
                } label: {
                    Image(systemName: "paintpalette.fill")
                        .font(.system(size: 18, weight: .semibold))
                        .foregroundStyle(Theme.bgDeep)
                        .frame(width: 44, height: 44)
                        .background(Theme.accent, in: Circle())
                        .shadow(color: Theme.accent.opacity(0.5), radius: 8)
                }
                .padding(20)
                .accessibilityIdentifier("component-gallery-launcher")
            }
            .sheet(isPresented: $isPresented) {
                ComponentGallery()
            }
            .onAppear {
                // Scripted-screenshot hook: `xcrun simctl launch booted
                // <bundle-id> -showGallery` passes this through as a launch
                // argument, letting tooling open straight to the gallery
                // without a fragile simulated tap. DEBUG-only, same as the
                // rest of this file.
                if ProcessInfo.processInfo.arguments.contains("-showGallery") {
                    isPresented = true
                }
            }
    }
}
#endif
