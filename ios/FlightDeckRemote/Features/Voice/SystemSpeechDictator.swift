//
//  SystemSpeechDictator.swift
//  FlightDeckRemote
//
//  The real `SpeechDictating` conformer: `SFSpeechRecognizer` fed by a tap on
//  `AVAudioEngine`'s input node (PRD §7). It manages the `AVAudioSession`
//  lifecycle (activate on start, deactivate on stop) and streams partial
//  transcripts through `onTranscript`.
//
//  Authorization covers BOTH speech recognition and the microphone — either
//  denial makes voice unavailable and the compose bar falls back to keyboard
//  dictation. The Info.plist declares `NSSpeechRecognitionUsageDescription` and
//  `NSMicrophoneUsageDescription`.
//
//  Not exercised by tests / previews (real STT is unavailable in the simulator):
//  the DEBUG launch-argument seam injects a scripted recognizer instead. This
//  type exists so the production build has a working recognizer.
//

import Foundation
import AVFoundation
import Speech

@MainActor
final class SystemSpeechDictator: SpeechDictating {

    var onTranscript: ((String) -> Void)?
    var onFailure: ((String) -> Void)?

    /// The recognition locale for the next hold, resolved fresh each time so a
    /// language change in Settings takes effect without recreating this object.
    private let localeProvider: () -> Locale
    private var recognizer: SFSpeechRecognizer?
    private var recognizerLocaleId: String?
    private let audioEngine = AVAudioEngine()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?

    /// - Parameter locale: the recognition locale provider. Defaults to the
    ///   device locale (the historical implicit behavior); the app injects the
    ///   user's chosen `SpeechLanguage`. Called on each authorization/record so
    ///   a switch applies on the next hold.
    init(locale: @escaping () -> Locale = { Locale.current }) {
        self.localeProvider = locale
    }

    /// The recognizer for the currently-selected language, (re)built lazily when
    /// the language changes. `SFSpeechRecognizer(locale:)` returns `nil` for a
    /// locale the device can't recognize, which surfaces as `.unavailable`.
    private func currentRecognizer() -> SFSpeechRecognizer? {
        let wanted = localeProvider()
        if recognizer == nil || recognizerLocaleId != wanted.identifier {
            recognizer = SFSpeechRecognizer(locale: wanted)
            recognizerLocaleId = wanted.identifier
        }
        return recognizer
    }

    func requestAuthorization() async -> DictationAuthorization {
        // Speech recognition first, then the microphone — either denial fails.
        let speech = await withCheckedContinuation { continuation in
            SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0) }
        }
        guard speech == .authorized else {
            return speech == .notDetermined ? .unavailable : .denied
        }
        guard currentRecognizer()?.isAvailable == true else { return .unavailable }

        let mic = await withCheckedContinuation { continuation in
            AVAudioApplication.requestRecordPermission { continuation.resume(returning: $0) }
        }
        return mic ? .authorized : .denied
    }

    func startRecording() throws {
        // Fresh request/task per hold.
        task?.cancel()
        task = nil

        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.record, mode: .measurement, options: .duckOthers)
        try session.setActive(true, options: .notifyOthersOnDeactivation)

        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        self.request = request

        let input = audioEngine.inputNode
        let format = input.outputFormat(forBus: 0)
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak request] buffer, _ in
            request?.append(buffer)
        }

        audioEngine.prepare()
        try audioEngine.start()

        guard let recognizer = currentRecognizer() else {
            onFailure?("recognizer unavailable")
            return
        }
        task = recognizer.recognitionTask(with: request) { [weak self] result, error in
            guard let self else { return }
            Task { @MainActor in
                if let result {
                    self.onTranscript?(result.bestTranscription.formattedString)
                }
                if error != nil, result?.isFinal != true {
                    self.onFailure?("couldn’t hear that")
                }
            }
        }
    }

    func stopRecording() {
        teardown()
    }

    func cancel() {
        task?.cancel()
        teardown()
    }

    private func teardown() {
        if audioEngine.isRunning {
            audioEngine.stop()
            audioEngine.inputNode.removeTap(onBus: 0)
        }
        request?.endAudio()
        request = nil
        task = nil
        // Best-effort session deactivation; ignore the "still in use" race.
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }
}
