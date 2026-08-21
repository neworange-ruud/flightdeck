//
//  VoiceTestSupport.swift
//  FlightDeckRemoteTests
//
//  Shared doubles for the push-to-talk dictation tests: a controllable
//  `SpeechDictating` fake (manual transcript emission + scripted authorization)
//  and a mutable clock so the min-hold-duration guard can be driven precisely.
//

import Foundation
@testable import FlightDeckRemote

/// A `SpeechDictating` fake: records lifecycle calls, emits transcripts on
/// demand, and returns a scripted authorization — no microphone involved.
@MainActor
final class FakeSpeechDictator: SpeechDictating {
    var onTranscript: ((String) -> Void)?
    var onFailure: ((String) -> Void)?

    /// The authorization `requestAuthorization()` returns.
    var authorization: DictationAuthorization = .authorized

    private(set) var startCount = 0
    private(set) var stopCount = 0
    private(set) var cancelCount = 0

    func requestAuthorization() async -> DictationAuthorization { authorization }
    func startRecording() throws { startCount += 1 }
    func stopRecording() { stopCount += 1 }
    func cancel() { cancelCount += 1 }

    /// Push a (partial or final) transcript, as the recognizer would mid-capture.
    func emit(_ text: String) { onTranscript?(text) }
    /// Simulate an audio-engine / recognition failure.
    func fail(_ reason: String) { onFailure?(reason) }
}

/// A mutable wall clock for driving the hold-duration guard deterministically.
final class MutableClock {
    var now: Date
    init(_ start: Date = Date(timeIntervalSince1970: 1_000)) { self.now = start }
    func advance(_ seconds: TimeInterval) { now = now.addingTimeInterval(seconds) }
}
