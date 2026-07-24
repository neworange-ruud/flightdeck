//
//  DictationController.swift
//  FlightDeckRemote
//
//  The `@Observable` glue between the push-to-talk UI (the compose bar's mic /
//  the focus-mode reply control) and the recognizer (PRD §7). It owns the pure
//  `DictationState`, drives a `SpeechDictating` recognizer, and applies the
//  real-time hold duration against the state machine's `minimumHoldDuration`.
//
//  Edit-before-send, ALWAYS (PRD §7): a completed dictation only ever calls
//  `onCommit` with the transcript — it drops into the compose field as editable
//  text and is never sent from here.
//
//  Authorization is requested once, eagerly (`prepare()` on view appear), so a
//  hold can start recording synchronously without racing an async prompt — the
//  scripted UI-test recognizer resolves it instantly, the real one resolves it
//  before the user's first hold.
//

import Foundation
import Observation

@MainActor
@Observable
final class DictationController {

    /// The pure dictation state (phase + live partial transcript).
    private(set) var state = DictationState()

    /// The resolved authorization, once `prepare()` has run. `nil` until then.
    private(set) var authorization: DictationAuthorization?

    /// Called with a completed, non-empty transcript to drop into the compose
    /// field (edit-before-send). Never sends.
    var onCommit: ((String) -> Void)?

    /// Called when a hold is released too quickly to be a real dictation — the
    /// compose bar uses this to fall back to focusing the field (v1 keyboard
    /// dictation). Optional; harmless if unset.
    var onMistap: (() -> Void)?

    private let recognizer: any SpeechDictating
    private let now: () -> Date
    private var holdStart: Date?

    /// Whether the mic is actively recording (drives the "Listening…" indicator).
    var isListening: Bool { state.phase == .listening }

    /// Whether authorization is denied or the recognizer is unavailable — the
    /// mic then behaves as the v1 keyboard-dictation affordance only.
    var isVoiceUnavailable: Bool {
        switch authorization {
        case .denied, .unavailable: return true
        case .authorized, nil: return false
        }
    }

    /// - Parameter locale: the recognition locale for the real recognizer,
    ///   resolved fresh on each hold. Defaults to the user's persisted
    ///   `SpeechLanguage` (English until they choose otherwise), so a Settings
    ///   change applies on the next hold without recreating this controller.
    ///   Ignored when a `recognizer` is injected (tests / the DEBUG seam).
    init(recognizer: (any SpeechDictating)? = nil,
         locale: @escaping () -> Locale = { UserDefaultsSpeechLanguageStore().load().locale },
         now: @escaping () -> Date = Date.init) {
        #if DEBUG
        self.recognizer = recognizer ?? DictationDebugSeam.makeRecognizer(locale: locale)
        #else
        self.recognizer = recognizer ?? SystemSpeechDictator(locale: locale)
        #endif
        self.now = now
        self.recognizer.onTranscript = { [weak self] text in
            self?.apply(.transcriptUpdated(text))
        }
        self.recognizer.onFailure = { [weak self] reason in
            self?.stopEngineQuietly()
            self?.apply(.failed(reason: reason))
        }
    }

    /// Eagerly resolve authorization so the first hold starts cleanly. Safe to
    /// call more than once (the system prompts at most once).
    func prepare() async {
        guard authorization == nil else { return }
        authorization = await recognizer.requestAuthorization()
    }

    /// Begin a push-to-talk hold: start recording (or reflect denied/unavailable).
    func beginHold() {
        switch authorization {
        case .denied:
            apply(.authorizationDenied)
            onMistap?() // let the field take over for keyboard dictation
            return
        case .unavailable:
            apply(.recognizerUnavailable)
            onMistap?()
            return
        case .authorized, .none:
            break
        }
        holdStart = now()
        apply(.holdBegan)
        do {
            try recognizer.startRecording()
        } catch {
            apply(.failed(reason: "mic unavailable"))
        }
    }

    /// Release the hold: stop recording, apply the min-duration guard, and route
    /// the outcome (commit editable text / fall back to focusing the field).
    func endHold() {
        guard state.phase == .listening else { return }
        recognizer.stopRecording()
        let held = holdStart.map { now().timeIntervalSince($0) } ?? 0
        holdStart = nil
        apply(.holdEnded(heldFor: held))
        consumeOutcome()
    }

    /// Cancel any in-flight hold without committing (screen dismissed, etc.).
    func cancel() {
        guard state.phase == .listening else { return }
        recognizer.cancel()
        holdStart = nil
        state.phase = .idle
        state.transcript = ""
        state.outcome = nil
    }

    // MARK: - Internals

    private func apply(_ event: DictationEvent) {
        state = DictationStateMachine.reduce(state, event)
    }

    /// Act on (and clear) a completed hold's outcome.
    private func consumeOutcome() {
        guard let outcome = state.outcome else { return }
        state.outcome = nil
        switch outcome {
        case let .committed(text):
            onCommit?(text)
        case .tooShort:
            onMistap?()
        case .empty:
            break // nothing heard — silently return to idle
        }
    }

    private func stopEngineQuietly() {
        recognizer.cancel()
        holdStart = nil
    }
}
