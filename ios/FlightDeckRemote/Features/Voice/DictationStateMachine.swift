//
//  DictationStateMachine.swift
//  FlightDeckRemote
//
//  The push-to-talk dictation lifecycle (PRD §7 Voice). Pure: a phase + a
//  reducer over hold / transcript / authorization inputs, so the
//  idle → listening → transcribed → editable flow (and the release-too-soon,
//  empty-transcript, and auth-denied edge cases) is unit-tested without a
//  microphone, `SFSpeechRecognizer`, or a view.
//
//  Design constraints mirrored here (PRD §7):
//   * push-to-talk: HOLD to record, RELEASE to stop — deliberate, interruptible;
//   * edit-before-send, ALWAYS: a completed dictation only ever produces a
//     `committed` string for the compose field — it is never sent from here;
//   * a release before `minimumHoldDuration` is treated as a mis-tap and
//     discarded (the compose bar falls back to focusing the field so the system
//     keyboard's dictation key is reachable — the v1 affordance);
//   * an empty / whitespace-only transcript commits nothing.
//

import Foundation

/// The observable phase of a push-to-talk dictation.
enum DictationPhase: Equatable, Sendable {
    /// Not recording — the mic is idle.
    case idle
    /// Holding: the mic is hot and partial transcripts stream in ("Listening…").
    case listening
    /// Speech authorization was denied — the mic falls back to keyboard dictation.
    case denied
    /// The recognizer isn't available (locale/device) — keyboard dictation only.
    case unavailable
    /// The audio engine / recognition task errored; carries a short reason.
    case failed(reason: String)
}

/// The outcome of the most recently completed hold, for the compose field.
enum DictationOutcome: Equatable, Sendable {
    /// A usable transcript to drop into the field (edit-before-send).
    case committed(String)
    /// Released before `minimumHoldDuration` — treated as a mis-tap, discarded.
    case tooShort
    /// Held long enough but nothing intelligible was heard.
    case empty
}

/// Inputs that drive the dictation phase. Real-time concerns (the actual hold
/// duration, the live audio) live in `DictationController`; this reducer only
/// takes the resolved facts so it stays pure and testable.
enum DictationEvent: Equatable, Sendable {
    /// Authorization resolved to denied (from any phase).
    case authorizationDenied
    /// The recognizer is unavailable on this device/locale.
    case recognizerUnavailable
    /// The user pressed and held the mic — begin recording.
    case holdBegan
    /// A partial (or final) transcript arrived while listening.
    case transcriptUpdated(String)
    /// The user released the mic after holding `heldFor` seconds.
    case holdEnded(heldFor: TimeInterval)
    /// The audio engine / recognition task failed.
    case failed(reason: String)
}

/// The full dictation state: the phase, the live partial transcript, and the
/// last completed outcome (consumed once by the controller, then cleared).
struct DictationState: Equatable, Sendable {
    var phase: DictationPhase = .idle
    /// The live partial transcript while `.listening` (drives the on-screen
    /// preview); reset when a hold begins or completes.
    var transcript: String = ""
    /// The outcome of the last completed hold. `nil` until a hold ends; the
    /// controller reads it, acts (drops text / focuses field), and clears it.
    var outcome: DictationOutcome?
}

/// Pure reducer for the push-to-talk dictation lifecycle. Deliberately total:
/// unexpected inputs for a phase leave it unchanged rather than trap.
enum DictationStateMachine {

    /// A release before this hold duration is a mis-tap, not a dictation — the
    /// transcript is discarded and the compose bar focuses the field instead.
    static let minimumHoldDuration: TimeInterval = 0.35

    static func reduce(_ state: DictationState, _ event: DictationEvent) -> DictationState {
        var next = state
        switch event {
        case .authorizationDenied:
            next.phase = .denied
            next.transcript = ""

        case .recognizerUnavailable:
            next.phase = .unavailable
            next.transcript = ""

        case .holdBegan:
            // A fresh hold clears the last outcome + partial and starts listening,
            // regardless of the prior phase (a retry after a transient failure is
            // allowed; denied/unavailable are gated by the controller upstream).
            next.phase = .listening
            next.transcript = ""
            next.outcome = nil

        case let .transcriptUpdated(text):
            guard next.phase == .listening else { break }
            next.transcript = text

        case let .holdEnded(heldFor):
            guard next.phase == .listening else { break }
            next.phase = .idle
            let trimmed = next.transcript.trimmingCharacters(in: .whitespacesAndNewlines)
            if heldFor < minimumHoldDuration {
                next.outcome = .tooShort
            } else if trimmed.isEmpty {
                next.outcome = .empty
            } else {
                next.outcome = .committed(trimmed)
            }
            next.transcript = ""

        case let .failed(reason):
            next.phase = .failed(reason: reason)
            next.transcript = ""
        }
        return next
    }
}
