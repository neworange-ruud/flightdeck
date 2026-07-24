//
//  DictationDebugSeam.swift
//  FlightDeckRemote
//
//  DEBUG-only launch-argument seam so UI tests (and simulator developers) can
//  drive push-to-talk dictation deterministically — real speech recognition is
//  unavailable in the simulator. Mirrors the `-uitest-linkstate`
//  (`ConnectionDebugSeam`) / `-uitest-fixture-shell` (`ShellDebugSeam`) pattern:
//  additive and scoped to this feature's own files.
//
//  Usage: `-uitest-dictation-transcript "Yes, run it. Then rebuild."` — when
//  present, `DictationController` uses a `ScriptedSpeechDictator` that emits the
//  given text as the transcript when a hold starts, so a UI test can drive
//  hold → release → edit → send without a microphone.
//  Add `-uitest-dictation-denied` to script an authorization denial instead.
//

#if DEBUG
import Foundation

enum DictationDebugSeam {
    static let transcriptFlag = "-uitest-dictation-transcript"
    static let deniedFlag = "-uitest-dictation-denied"

    /// The scripted transcript, if `-uitest-dictation-transcript <text>` is set.
    static func scriptedTranscript(
        arguments: [String] = ProcessInfo.processInfo.arguments
    ) -> String? {
        guard let i = arguments.firstIndex(of: transcriptFlag),
              arguments.indices.contains(i + 1) else { return nil }
        return arguments[i + 1]
    }

    static func deniedRequested(
        arguments: [String] = ProcessInfo.processInfo.arguments
    ) -> Bool {
        arguments.contains(deniedFlag)
    }

    /// The recognizer `DictationController` should use: a scripted one under the
    /// seam args, otherwise the real `SystemSpeechDictator`.
    ///
    /// - Parameter locale: the recognition locale for the real fallback. This is
    ///   a DEBUG build too (Xcode → device), so the chosen `SpeechLanguage` MUST
    ///   flow through here — otherwise the recognizer silently follows the
    ///   device locale and the language toggle has no effect. The scripted
    ///   recognizers ignore it (they emit canned text, no microphone).
    @MainActor
    static func makeRecognizer(locale: @escaping () -> Locale = { Locale.current }) -> any SpeechDictating {
        if deniedRequested() {
            return ScriptedSpeechDictator(transcript: "", authorization: .denied)
        }
        if let scripted = scriptedTranscript() {
            return ScriptedSpeechDictator(transcript: scripted, authorization: .authorized)
        }
        return SystemSpeechDictator(locale: locale)
    }
}

/// DEBUG-only scripted recognizer for the fixture / UI-test path. It never
/// touches the microphone: it emits its canned transcript when recording starts
/// so a held mic reliably yields text, and reports a fixed authorization.
@MainActor
final class ScriptedSpeechDictator: SpeechDictating {
    var onTranscript: ((String) -> Void)?
    var onFailure: ((String) -> Void)?

    private let transcript: String
    private let authorization: DictationAuthorization

    /// Sends the scripted transcript, if any (exposed for unit tests).
    private(set) var didStart = false
    private(set) var didStop = false

    init(transcript: String, authorization: DictationAuthorization = .authorized) {
        self.transcript = transcript
        self.authorization = authorization
    }

    func requestAuthorization() async -> DictationAuthorization { authorization }

    func startRecording() throws {
        didStart = true
        // Emit synchronously so the transcript is present before the release,
        // keeping the UI test deterministic (no async race with the gesture).
        if !transcript.isEmpty { onTranscript?(transcript) }
    }

    func stopRecording() { didStop = true }

    func cancel() { didStop = true }
}
#endif
