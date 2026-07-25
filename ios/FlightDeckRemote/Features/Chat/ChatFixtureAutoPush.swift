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

    /// How many turns to keep re-trying the push, and the gap between them.
    /// ~2s total is far longer than the transition needs, and every attempt
    /// after the first one that sticks is a cheap `isEmpty` check.
    private let maxAttempts = 20
    private let retryGap = Duration.milliseconds(100)

    func body(content: Content) -> some View {
        content.task {
            guard ProcessInfo.processInfo.arguments.contains("-uitest-fixture-transcript")
            else { return }
            // Retry rather than push once. UI tests reach this by flipping the
            // DEBUG pairing toggle, which swaps the app root from PairingView to
            // MainTabView — and an append that lands in the same transaction
            // that installs the tab's `NavigationStack` is silently dropped, so
            // a single `onAppear` push left the chat route unreachable for the
            // rest of the run (~20% of ChatTranscriptUITests runs, which then
            // timed out waiting for `AgentChatView` — remote-control-7lo).
            // A dropped append leaves `path` empty, so re-checking it both
            // detects the drop and makes a successful push idempotent.
            for _ in 0..<maxAttempts {
                if !path.isEmpty { return }
                path.append(.chat(projectId: "fixture-project",
                                  sessionId: "fixture-session",
                                  pairingId: nil))
                try? await Task.sleep(for: retryGap)
            }
        }
    }
}
#endif
