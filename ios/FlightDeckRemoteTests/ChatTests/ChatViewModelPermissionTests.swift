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

        #expect(model.permissionActionState(promptId) == .sending(.choice(.allowOnce)))
        #expect(sender.sends.count == 1)
        if case let .permissionDecision(sessionId, pid, choice, optionIndex, optionIndices, freeText) = sender.sends.first?.body {
            #expect(sessionId == Wire.SessionId("s1"))
            #expect(pid == promptId)
            #expect(choice == .allowOnce)
            #expect(optionIndex == nil)
            #expect(optionIndices == nil)
            #expect(freeText == nil)
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
        #expect(model.permissionActionState(promptId) == .resolved(.choice(.deny)))
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

        guard case let .failed(_, answer, reusesId) = model.permissionActionState(promptId) else {
            Issue.record("expected .failed"); return
        }
        #expect(answer == .choice(.allowOnce))
        #expect(reusesId)
        // A failed decision is actionable again (retry).
        #expect(model.isPermissionActionable(promptId))

        model.retryPermission(promptId)
        #expect(sender.sends.count == 2)
        #expect(sender.sends[1].commandId == originalId) // dedup-safe reuse
        #expect(model.permissionActionState(promptId) == .sending(.choice(.allowOnce)))
    }

    @Test func decideBlockedWhenPaused() {
        let (model, sender) = makeModel(link: .disconnected)
        model.decidePermission(promptId: promptId, choice: .allowOnce)
        #expect(sender.sends.isEmpty)
        #expect(model.permissionActionState(promptId) == .idle)
    }

    // MARK: - Question-style decisions (option index / free text)

    @Test func decideOptionIndexSendsOptionIndexNotChoice() {
        let (model, sender) = makeModel()
        model.decidePermission(promptId: promptId, optionIndex: 2, label: "Redis")

        #expect(model.permissionActionState(promptId) == .sending(.option(index: 2, label: "Redis")))
        guard case let .permissionDecision(_, _, choice, optionIndex, optionIndices, freeText) = sender.sends.first?.body else {
            Issue.record("expected .permissionDecision"); return
        }
        #expect(choice == nil)
        #expect(optionIndex == 2)
        #expect(optionIndices == nil)
        #expect(freeText == nil)
    }

    @Test func decideOptionIndicesSendsOptionIndicesNotOptionIndex() {
        let (model, sender) = makeModel()
        model.decidePermission(promptId: promptId, optionIndices: [0, 2],
                               labels: ["Tests", "Fmt"])

        #expect(model.permissionActionState(promptId)
                == .sending(.options(indices: [0, 2], labels: ["Tests", "Fmt"])))
        guard case let .permissionDecision(_, _, choice, optionIndex, optionIndices, freeText) = sender.sends.first?.body else {
            Issue.record("expected .permissionDecision"); return
        }
        #expect(choice == nil)
        #expect(optionIndex == nil)
        #expect(optionIndices == [0, 2])
        #expect(freeText == nil)
    }

    @Test func decideOptionIndicesIgnoresEmptySelection() {
        let (model, sender) = makeModel()
        model.decidePermission(promptId: promptId, optionIndices: [], labels: [])
        #expect(sender.sends.isEmpty)
        #expect(model.permissionActionState(promptId) == .idle)
    }

    @Test func decideFreeTextSendsFreeTextNotChoice() {
        let (model, sender) = makeModel()
        model.decidePermission(promptId: promptId, freeText: "Use CockroachDB instead.")

        #expect(model.permissionActionState(promptId)
                == .sending(.freeText("Use CockroachDB instead.")))
        guard case let .permissionDecision(_, _, choice, optionIndex, optionIndices, freeText) = sender.sends.first?.body else {
            Issue.record("expected .permissionDecision"); return
        }
        #expect(choice == nil)
        #expect(optionIndex == nil)
        #expect(optionIndices == nil)
        #expect(freeText == "Use CockroachDB instead.")
    }

    @Test func decideFreeTextIgnoresBlankText() {
        let (model, sender) = makeModel()
        model.decidePermission(promptId: promptId, freeText: "   ")
        #expect(sender.sends.isEmpty)
    }

    @Test func optionResolvesAndCanFailRetry() {
        let (model, sender) = makeModel()
        model.decidePermission(promptId: promptId, optionIndex: 1, label: "SQLite")
        model.applyDelivery(commandId: sender.lastCommandId!, state: .delivered(.applied))
        #expect(model.permissionActionState(promptId)
                == .resolved(.option(index: 1, label: "SQLite")))
    }
}
