//
//  ChatFixtureAutoPush.swift
//  FlightDeckRemote
//
//  DEBUG-only test seam: when the app is launched with
//  `-uitest-fixture-transcript`, push the fixture-backed chat route onto the
//  Projects navigation stack on appear, so `AgentChatView` is reachable in UI
//  tests without depending on the (sibling-owned, still-placeholder) Projects
//  and Sessions navigation. A no-op in Release builds.
//
//  Kept in the Chat feature (rather than baked into Navigation) so the chat
//  task owns its own test entry point; the `MainTabView` wiring is a single
//  `.chatFixtureAutoPush(path:)` call.
//

import SwiftUI

extension View {
    /// DEBUG-only: auto-push the fixture chat route when launched under the
    /// transcript-fixture UI-test arg. No-op otherwise.
    func chatFixtureAutoPush(path: Binding<[ProjectsRoute]>) -> some View {
        #if DEBUG
        modifier(ChatFixtureAutoPushModifier(path: path))
        #else
        self
        #endif
    }
}

#if DEBUG
private struct ChatFixtureAutoPushModifier: ViewModifier {
    @Binding var path: [ProjectsRoute]

    func body(content: Content) -> some View {
        content.onAppear {
            guard ProcessInfo.processInfo.arguments.contains("-uitest-fixture-transcript"),
                  path.isEmpty
            else { return }
            path.append(.chat(projectId: "fixture-project", sessionId: "fixture-session"))
        }
    }
}
#endif
