//
//  DictationStateMachineTests.swift
//  FlightDeckRemoteTests
//
//  Unit tests for the pure push-to-talk dictation reducer (PRD §7): the
//  idle → listening → transcribed → editable happy path, plus the
//  release-too-soon, empty-transcript, and authorization edge cases.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@Suite struct DictationStateMachineTests {

    private let minHold = DictationStateMachine.minimumHoldDuration

    private func reduce(_ state: DictationState, _ events: [DictationEvent]) -> DictationState {
        events.reduce(state) { DictationStateMachine.reduce($0, $1) }
    }

    // MARK: - Happy path

    @Test func holdListensThenCommitsEditableTranscript() {
        let end = reduce(DictationState(), [
            .holdBegan,
            .transcriptUpdated("Yes, run it"),
            .transcriptUpdated("Yes, run it. Then rebuild."),
            .holdEnded(heldFor: minHold + 0.5),
        ])
        #expect(end.phase == .idle)
        #expect(end.transcript.isEmpty)              // partial cleared on commit
        #expect(end.outcome == .committed("Yes, run it. Then rebuild."))
    }

    @Test func holdBeganEntersListening() {
        let s = DictationStateMachine.reduce(DictationState(), .holdBegan)
        #expect(s.phase == .listening)
    }

    @Test func commitTrimsWhitespace() {
        let end = reduce(DictationState(), [
            .holdBegan,
            .transcriptUpdated("  ship it  "),
            .holdEnded(heldFor: minHold + 0.1),
        ])
        #expect(end.outcome == .committed("ship it"))
    }

    // MARK: - Release before minimum hold → mis-tap, discarded

    @Test func releaseBeforeMinimumDurationDiscards() {
        let end = reduce(DictationState(), [
            .holdBegan,
            .transcriptUpdated("half a word"),
            .holdEnded(heldFor: minHold - 0.1),
        ])
        #expect(end.phase == .idle)
        #expect(end.outcome == .tooShort)   // never committed
    }

    // MARK: - Empty transcript

    @Test func heldLongEnoughButEmptyCommitsNothing() {
        let end = reduce(DictationState(), [
            .holdBegan,
            .holdEnded(heldFor: minHold + 1),
        ])
        #expect(end.outcome == .empty)
    }

    @Test func whitespaceOnlyTranscriptIsEmpty() {
        let end = reduce(DictationState(), [
            .holdBegan,
            .transcriptUpdated("   \n "),
            .holdEnded(heldFor: minHold + 1),
        ])
        #expect(end.outcome == .empty)
    }

    // MARK: - Authorization / availability

    @Test func authorizationDeniedEntersDeniedPhase() {
        let s = DictationStateMachine.reduce(DictationState(), .authorizationDenied)
        #expect(s.phase == .denied)
    }

    @Test func recognizerUnavailableEntersUnavailablePhase() {
        let s = DictationStateMachine.reduce(DictationState(), .recognizerUnavailable)
        #expect(s.phase == .unavailable)
    }

    // MARK: - Robustness

    @Test func transcriptIgnoredWhenNotListening() {
        let s = DictationStateMachine.reduce(DictationState(), .transcriptUpdated("stray"))
        #expect(s.phase == .idle)
        #expect(s.transcript.isEmpty)
    }

    @Test func holdEndedIgnoredWhenNotListening() {
        let s = DictationStateMachine.reduce(DictationState(), .holdEnded(heldFor: 5))
        #expect(s.phase == .idle)
        #expect(s.outcome == nil)
    }

    @Test func failureEntersFailedPhaseAndClearsPartial() {
        let end = reduce(DictationState(), [
            .holdBegan,
            .transcriptUpdated("mid sentence"),
            .failed(reason: "mic unavailable"),
        ])
        #expect(end.phase == .failed(reason: "mic unavailable"))
        #expect(end.transcript.isEmpty)
    }

    @Test func newHoldClearsPriorOutcome() {
        var s = reduce(DictationState(), [
            .holdBegan,
            .transcriptUpdated("first"),
            .holdEnded(heldFor: minHold + 1),
        ])
        #expect(s.outcome == .committed("first"))
        s = DictationStateMachine.reduce(s, .holdBegan)
        #expect(s.phase == .listening)
        #expect(s.outcome == nil)
    }
}
