//
//  SpeechDictating.swift
//  FlightDeckRemote
//
//  The speech-recognition seam for push-to-talk voice compose (PRD §7). It
//  abstracts `SFSpeechRecognizer` + `AVAudioEngine` behind a tiny protocol so
//  the dictation state machine / controller can be unit-tested against a fake,
//  and so the DEBUG launch-argument seam can inject a scripted transcript in UI
//  tests (real STT is unavailable in the simulator).
//
//  `SystemSpeechDictator` (the real implementation) lives in its own file; unit
//  tests and the UI-test seam inject a fake / scripted conformer.
//

import Foundation

/// The result of requesting speech-recognition authorization.
enum DictationAuthorization: Equatable, Sendable {
    /// Authorized (and the recognizer is available) — recording may start.
    case authorized
    /// The user (or restrictions) denied speech recognition / the microphone.
    case denied
    /// The recognizer isn't available for this device/locale.
    case unavailable
}

/// The minimal capture surface the Voice feature needs. A conformer streams
/// partial transcripts through `onTranscript` while recording, and stops
/// cleanly on `stopRecording()` / `cancel()`.
@MainActor
protocol SpeechDictating: AnyObject {
    /// Called with the latest (partial or final) transcript while recording.
    var onTranscript: ((String) -> Void)? { get set }
    /// Called if the audio engine / recognition task errors mid-capture.
    var onFailure: ((String) -> Void)? { get set }

    /// Request microphone + speech-recognition authorization. Idempotent; the
    /// system prompts at most once, then returns the cached decision.
    func requestAuthorization() async -> DictationAuthorization

    /// Start capturing. Activates the audio session and the recognition task.
    /// Throws if the audio engine can't start (surfaced as `.failed`).
    func startRecording() throws

    /// Stop capturing and deactivate the audio session, flushing a final
    /// transcript through `onTranscript` if one is pending.
    func stopRecording()

    /// Abort capture without committing (e.g. a mis-tap release).
    func cancel()
}
