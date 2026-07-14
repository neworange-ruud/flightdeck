//
//  ChatSendLogicTests.swift
//  FlightDeckRemoteTests
//
//  Unit tests for the pure send / permission / reconciliation state machine
//  (`ChatSendLogic`), incl. the retry command-id semantics (same-id on a
//  transport failure, new-id after an explicit rejection) and the
//  optimistic-message reconciliation heuristic.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@Suite struct ChatSendLogicTests {

    // MARK: - Outgoing message state mapping

    @Test func sendingMapsToPending() {
        #expect(ChatSendLogic.outgoingState(for: .sending) == .sending)
    }

    @Test func acceptedAndAppliedAndDuplicateMapToSent() {
        #expect(ChatSendLogic.outgoingState(for: .delivered(.accepted)) == .sent)
        #expect(ChatSendLogic.outgoingState(for: .delivered(.applied)) == .sent)
        #expect(ChatSendLogic.outgoingState(for: .delivered(.duplicate)) == .sent)
    }

    @Test func transportFailureMapsToFailedWithSameIdRetry() {
        let state = ChatSendLogic.outgoingState(for: .failed(reason: "timed out"))
        #expect(state == .failed(reason: "timed out", retryReusesId: true))
    }

    @Test func rejectedOutcomeMapsToFailedWithNewIdRetry() {
        let state = ChatSendLogic.outgoingState(for: .delivered(.rejected))
        guard case let .failed(_, reusesId) = state else {
            Issue.record("expected .failed"); return
        }
        #expect(reusesId == false)
    }

    // MARK: - Retry id semantics (the crux)

    @Test func timeoutRetryReusesId() {
        // A transport-level failure may already have applied → dedup-safe reuse.
        #expect(ChatSendLogic.retryReusesId(for: .failed(reason: "timed out")))
        #expect(ChatSendLogic.retryReusesId(for: .failed(reason: "link down")))
        #expect(ChatSendLogic.retryReusesId(for: .failed(reason: "peer unavailable")))
    }

    @Test func rejectedRetryMintsNewId() {
        // A definitive desktop-side negative → a retry is a fresh attempt.
        #expect(ChatSendLogic.retryReusesId(for: .delivered(.rejected)) == false)
        #expect(ChatSendLogic.retryReusesId(for: .delivered(.failed)) == false)
    }

    // MARK: - Permission action state mapping

    @Test func permissionSendingShowsSpinnerForTappedChoice() {
        let state = ChatSendLogic.permissionState(for: .sending, choice: .allowOnce)
        #expect(state == .sending(.allowOnce))
    }

    @Test func permissionAppliedResolves() {
        #expect(ChatSendLogic.permissionState(for: .delivered(.applied), choice: .deny)
                == .resolved(.deny))
        #expect(ChatSendLogic.permissionState(for: .delivered(.duplicate), choice: .allowOnce)
                == .resolved(.allowOnce))
    }

    @Test func permissionRejectedIsStale() {
        // A rejected ack means the prompt was already answered on the desktop.
        #expect(ChatSendLogic.permissionState(for: .delivered(.rejected), choice: .allowOnce)
                == .stale)
    }

    @Test func permissionTransportFailureIsRetryableWithSameId() {
        let state = ChatSendLogic.permissionState(for: .failed(reason: "timed out"),
                                                  choice: .allowOnce)
        #expect(state == .failed(reason: "timed out", choice: .allowOnce, retryReusesId: true))
    }

    @Test func permissionOutcomeFailureIsRetryableWithNewId() {
        let state = ChatSendLogic.permissionState(for: .delivered(.failed), choice: .deny)
        guard case let .failed(_, choice, reusesId) = state else {
            Issue.record("expected .failed"); return
        }
        #expect(choice == .deny)
        #expect(reusesId == false)
    }

    // MARK: - Reconciliation heuristic

    private func msg(_ text: String, at ms: Int64) -> OutgoingMessage {
        OutgoingMessage(localId: Wire.ItemId("local-1"), text: text, issuedAtMs: ms,
                        commandId: Wire.CommandId("c1"), state: .sent)
    }

    @Test func echoedUserMessageReconciles() {
        let out = msg("run the tests", at: 1_000)
        let items: [Wire.TranscriptItem] = [
            .agentMessage(itemId: Wire.ItemId("a"), text: "ok", atMs: 900),
            .userMessage(itemId: Wire.ItemId("srv"), text: "run the tests", atMs: 1_050),
        ]
        #expect(ChatSendLogic.isReconciled(out, against: items))
        #expect(ChatSendLogic.visibleOutgoing([out], against: items).isEmpty)
    }

    @Test func unechoedMessageStaysVisible() {
        let out = msg("run the tests", at: 1_000)
        let items: [Wire.TranscriptItem] = [
            .agentMessage(itemId: Wire.ItemId("a"), text: "ok", atMs: 900),
        ]
        #expect(ChatSendLogic.isReconciled(out, against: items) == false)
        #expect(ChatSendLogic.visibleOutgoing([out], against: items).count == 1)
    }

    @Test func whitespaceInsensitiveMatch() {
        let out = msg("  hello  ", at: 1_000)
        let items: [Wire.TranscriptItem] = [
            .userMessage(itemId: Wire.ItemId("srv"), text: "hello", atMs: 1_010),
        ]
        #expect(ChatSendLogic.isReconciled(out, against: items))
    }

    @Test func olderIdenticalMessageDoesNotReconcile() {
        // An identical message far in the past must not swallow a fresh send.
        let out = msg("deploy", at: 10_000_000)
        let items: [Wire.TranscriptItem] = [
            .userMessage(itemId: Wire.ItemId("old"), text: "deploy", atMs: 1_000),
        ]
        #expect(ChatSendLogic.isReconciled(out, against: items) == false)
    }
}
