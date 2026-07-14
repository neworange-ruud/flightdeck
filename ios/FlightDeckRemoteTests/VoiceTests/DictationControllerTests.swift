//
//  DictationControllerTests.swift
//  FlightDeckRemoteTests
//
//  Unit tests for `DictationController`: it drives the recognizer, applies the
//  real-time hold duration against the min-hold guard, and routes a completed
//  dictation to `onCommit` (editable text — never sent) or `onMistap` (fall
//  back to focusing the field). Authorization outcomes gate recording.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@MainActor
@Suite struct DictationControllerTests {

    private let minHold = DictationStateMachine.minimumHoldDuration

    private func make(authorization: DictationAuthorization = .authorized)
        -> (DictationController, FakeSpeechDictator, MutableClock) {
        let fake = FakeSpeechDictator()
        fake.authorization = authorization
        let clock = MutableClock()
        let controller = DictationController(recognizer: fake, now: { clock.now })
        return (controller, fake, clock)
    }

    // MARK: - Happy path

    @Test func holdEmitsAndCommitsEditableText() async {
        let (controller, fake, clock) = make()
        await controller.prepare()

        var committed: String?
        controller.onCommit = { committed = $0 }

        controller.beginHold()
        #expect(controller.isListening)
        #expect(fake.startCount == 1)

        fake.emit("run the tests")
        #expect(controller.state.transcript == "run the tests")

        clock.advance(minHold + 0.5)
        controller.endHold()

        #expect(fake.stopCount == 1)
        #expect(committed == "run the tests")
        #expect(!controller.isListening)
    }

    // MARK: - Release before the minimum → mis-tap, no commit

    @Test func quickReleaseDoesNotCommitAndSignalsMistap() async {
        let (controller, fake, clock) = make()
        await controller.prepare()

        var committed: String?
        var mistap = false
        controller.onCommit = { committed = $0 }
        controller.onMistap = { mistap = true }

        controller.beginHold()
        fake.emit("half")
        clock.advance(minHold - 0.1)   // released too soon
        controller.endHold()

        #expect(committed == nil)
        #expect(mistap)
    }

    // MARK: - Empty transcript commits nothing

    @Test func heldLongEnoughButSilentCommitsNothing() async {
        let (controller, _, clock) = make()
        await controller.prepare()

        var committed: String?
        controller.onCommit = { committed = $0 }

        controller.beginHold()
        clock.advance(minHold + 1)     // nothing emitted
        controller.endHold()

        #expect(committed == nil)
        #expect(controller.state.phase == .idle)
    }

    // MARK: - Authorization denied

    @Test func deniedAuthorizationBlocksRecording() async {
        let (controller, fake, _) = make(authorization: .denied)
        await controller.prepare()
        #expect(controller.isVoiceUnavailable)

        var mistap = false
        controller.onMistap = { mistap = true }

        controller.beginHold()
        #expect(fake.startCount == 0)              // never opened the mic
        #expect(controller.state.phase == .denied)
        #expect(mistap)                            // fall back to the field
    }

    @Test func unavailableRecognizerBlocksRecording() async {
        let (controller, fake, _) = make(authorization: .unavailable)
        await controller.prepare()

        controller.beginHold()
        #expect(fake.startCount == 0)
        #expect(controller.state.phase == .unavailable)
    }

    // MARK: - Mid-capture failure

    @Test func recognizerFailureCancelsAndReportsFailed() async {
        let (controller, fake, _) = make()
        await controller.prepare()

        controller.beginHold()
        fake.fail("couldn’t hear that")

        #expect(controller.state.phase == .failed(reason: "couldn’t hear that"))
        #expect(fake.cancelCount == 1)
    }

    // MARK: - Cancel

    @Test func cancelStopsWithoutCommitting() async {
        let (controller, fake, _) = make()
        await controller.prepare()

        var committed: String?
        controller.onCommit = { committed = $0 }

        controller.beginHold()
        fake.emit("in progress")
        controller.cancel()

        #expect(fake.cancelCount == 1)
        #expect(controller.state.phase == .idle)
        #expect(committed == nil)
    }
}
