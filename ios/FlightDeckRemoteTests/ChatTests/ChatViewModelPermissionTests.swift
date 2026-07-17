//
//  ChatViewModelPermissionTests.swift
//  FlightDeckRemoteTests
//
//  Unit tests for `ChatViewModel`'s inline permission-decision state machine:
//  actionability gating (current pending prompt × link up), the sending →
//  resolved / stale / failed transitions, stale rejection, and retry id
//  semantics.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@MainActor
@Suite struct ChatViewModelPermissionTests {

    /// The fixture's pending permission prompt id.
    private let promptId = Wire.PromptId("fx-prompt-1")

    private func makeModel(link: RemoteLinkState = .connected(latencyMs: 5))
        -> (ChatViewModel, FakeChatSender) {
        let sender = FakeChatSender()
        let source = FakeConnectionSource(link)
        let gate = CommandsPausedGate(source: source, launchArguments: [])
        let model = ChatViewModel(projectId: Wire.ProjectId("p1"),
                                  sessionId: Wire.SessionId("s1"))
        model.loadFixture()
        model.configureSend(sender: sender, pausedGate: gate)
        return (model, sender)
    }

    // MARK: - Actionability

    @Test func currentPromptActionableWhenConnected() {
        let (model, _) = makeModel()
        #expect(model.currentPendingPromptId == promptId)
        #expect(model.isPermissionActionable(promptId))
    }

    @Test func notActionableWhenPaused() {
        let (model, _) = makeModel(link: .disconnected)
        #expect(model.isPermissionActionable(promptId) == false)
    }

    @Test func unknownPromptNotActionable() {
        let (model, _) = makeModel()
        #expect(model.isPermissionActionable(Wire.PromptId("nope")) == false)
    }

    // MARK: - Decision flow

    @Test func decideSendsPermissionDecisionAndShowsSpinner() {
        let (model, sender) = makeModel()
        model.decidePermission(promptId: promptId, choice: .allowOnce)

        #expect(model.permissionActionState(promptId) == .sending(.allowOnce))
        #expect(sender.sends.count == 1)
        if case let .permissionDecision(sessionId, pid, choice) = sender.sends.first?.body {
            #expect(sessionId == Wire.SessionId("s1"))
            #expect(pid == promptId)
            #expect(choice == .allowOnce)
        } else {
            Issue.record("expected .permissionDecision")
        }
        // A decision in flight is no longer actionable (buttons lock).
        #expect(model.isPermissionActionable(promptId) == false)
    }

    @Test func appliedCollapsesToResolved() {
        let (model, sender) = makeModel()
        model.decidePermission(promptId: promptId, choice: .deny)
        model.applyDelivery(commandId: sender.lastCommandId!, state: .delivered(.applied))
        #expect(model.permissionActionState(promptId) == .resolved(.deny))
    }

    @Test func rejectedIsStale() {
        let (model, sender) = makeModel()
        model.decidePermission(promptId: promptId, choice: .allowOnce)
        model.applyDelivery(commandId: sender.lastCommandId!, state: .delivered(.rejected))
        #expect(model.permissionActionState(promptId) == .stale)
        // A stale prompt offers no further action.
        #expect(model.isPermissionActionable(promptId) == false)
    }

    @Test func transportFailureIsRetryableAndReusesId() {
        let (model, sender) = makeModel()
        model.decidePermission(promptId: promptId, choice: .allowOnce)
        let originalId = sender.lastCommandId!
        model.applyDelivery(commandId: originalId, state: .failed(reason: "timed out"))

        guard case let .failed(_, choice, reusesId) = model.permissionActionState(promptId) else {
            Issue.record("expected .failed"); return
        }
        #expect(choice == .allowOnce)
        #expect(reusesId)
        // A failed decision is actionable again (retry).
        #expect(model.isPermissionActionable(promptId))

        model.retryPermission(promptId)
        #expect(sender.sends.count == 2)
        #expect(sender.sends[1].commandId == originalId) // dedup-safe reuse
        #expect(model.permissionActionState(promptId) == .sending(.allowOnce))
    }

    @Test func decideBlockedWhenPaused() {
        let (model, sender) = makeModel(link: .disconnected)
        model.decidePermission(promptId: promptId, choice: .allowOnce)
        #expect(sender.sends.isEmpty)
        #expect(model.permissionActionState(promptId) == .idle)
    }
}
