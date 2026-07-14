//
//  ChatViewModelSendTests.swift
//  FlightDeckRemoteTests
//
//  Unit tests for `ChatViewModel`'s send path: optimistic append, delivery
//  reconciliation, paused gating, retry command-id semantics (both cases), and
//  the optimistic-message dedup against the authoritative transcript.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@MainActor
@Suite struct ChatViewModelSendTests {

    private func makeModel(link: RemoteLinkState = .connected(latencyMs: 5),
                           now: @escaping () -> Int64 = { 1_000 })
        -> (ChatViewModel, FakeChatSender, FakeConnectionSource) {
        let sender = FakeChatSender()
        let source = FakeConnectionSource(link)
        let gate = CommandsPausedGate(source: source, launchArguments: [])
        let model = ChatViewModel(projectId: Wire.ProjectId("p1"),
                                  sessionId: Wire.SessionId("s1"), now: now)
        model.configureSend(sender: sender, pausedGate: gate)
        return (model, sender, source)
    }

    // MARK: - Optimistic append + delivery

    @Test func sendAppendsOptimisticPendingAndSendsReply() {
        let (model, sender, _) = makeModel()
        model.draft = "run the tests"
        model.send()

        // Optimistic message appended, marked sending, field cleared.
        #expect(model.outgoing.count == 1)
        #expect(model.outgoing.first?.state == .sending)
        #expect(model.draft.isEmpty)

        // The reply command was sent with the right body.
        #expect(sender.sends.count == 1)
        if case let .reply(sessionId, text) = sender.sends.first?.body {
            #expect(sessionId == Wire.SessionId("s1"))
            #expect(text == "run the tests")
        } else {
            Issue.record("expected a .reply command")
        }
    }

    @Test func deliveredClearsPending() {
        let (model, sender, _) = makeModel()
        model.draft = "hello"
        model.send()
        let cmd = sender.lastCommandId!

        model.applyDelivery(commandId: cmd, state: .delivered(.applied))
        #expect(model.outgoing.first?.state == .sent)
    }

    @Test func failedMarksNotDelivered() {
        let (model, sender, _) = makeModel()
        model.draft = "hello"
        model.send()
        let cmd = sender.lastCommandId!

        model.applyDelivery(commandId: cmd, state: .failed(reason: "timed out"))
        #expect(model.outgoing.first?.state == .failed(reason: "timed out", retryReusesId: true))
    }

    @Test func rejectedMarksFailedNewId() {
        let (model, sender, _) = makeModel()
        model.draft = "hello"
        model.send()
        model.applyDelivery(commandId: sender.lastCommandId!, state: .delivered(.rejected))
        guard case let .failed(_, reusesId) = model.outgoing.first?.state else {
            Issue.record("expected .failed"); return
        }
        #expect(reusesId == false)
    }

    // MARK: - Paused gating

    @Test func sendBlockedWhenPaused() {
        let (model, sender, _) = makeModel(link: .disconnected)
        #expect(model.commandsPaused)
        model.draft = "won't go"
        model.send()
        #expect(model.outgoing.isEmpty)
        #expect(sender.sends.isEmpty)
        #expect(model.draft == "won't go") // preserved, not cleared
    }

    @Test func emptyDraftDoesNotSend() {
        let (model, sender, _) = makeModel()
        model.draft = "   \n "
        model.send()
        #expect(model.outgoing.isEmpty)
        #expect(sender.sends.isEmpty)
    }

    // MARK: - Retry id semantics (both cases)

    @Test func retryAfterTimeoutReusesId() {
        let (model, sender, _) = makeModel()
        model.draft = "deploy"
        model.send()
        let originalId = sender.lastCommandId!
        model.applyDelivery(commandId: originalId, state: .failed(reason: "timed out"))

        model.retryOutgoing(model.outgoing.first!.localId)

        // A second send went out, reusing the ORIGINAL command id (dedup-safe).
        #expect(sender.sends.count == 2)
        #expect(sender.sends[1].commandId == originalId)
        #expect(model.outgoing.first?.state == .sending)
        #expect(model.outgoing.first?.commandId == originalId)
    }

    @Test func retryAfterRejectionMintsNewId() {
        let (model, sender, _) = makeModel()
        model.draft = "deploy"
        model.send()
        let originalId = sender.lastCommandId!
        model.applyDelivery(commandId: originalId, state: .delivered(.rejected))

        model.retryOutgoing(model.outgoing.first!.localId)

        #expect(sender.sends.count == 2)
        #expect(sender.sends[1].commandId != originalId)
        #expect(model.outgoing.first?.commandId != originalId)
        // Delivery on the NEW id reconciles the same message.
        model.applyDelivery(commandId: sender.sends[1].commandId, state: .delivered(.applied))
        #expect(model.outgoing.first?.state == .sent)
    }

    @Test func retryNoOpWhenNotFailed() {
        let (model, sender, _) = makeModel()
        model.draft = "hi"
        model.send()
        // Still sending, not failed → retry is a no-op.
        model.retryOutgoing(model.outgoing.first!.localId)
        #expect(sender.sends.count == 1)
    }

    // MARK: - Optimistic reconciliation against the authoritative feed

    @Test func optimisticRowVisibleUntilEchoed() {
        // `now` == fixture base so the echoed fixture user message reconciles.
        let base: Int64 = 1_720_000_000_000
        let (model, _, _) = makeModel(now: { base })
        // No store bound → items come from the fixture localItems.
        model.loadFixture()

        let echoedText = "Can you fix the login redirect? It loops back to /login after a token refresh."
        let before = model.displayItems.count
        model.draft = echoedText
        model.send()

        // The optimistic copy matches an existing authoritative user message, so
        // it is reconciled immediately and NOT shown twice.
        #expect(model.outgoing.count == 1)
        #expect(model.displayItems.count == before)

        // A brand-new message that isn't echoed shows as an extra row.
        model.draft = "something entirely new"
        model.send()
        #expect(model.displayItems.count == before + 1)
    }
}
