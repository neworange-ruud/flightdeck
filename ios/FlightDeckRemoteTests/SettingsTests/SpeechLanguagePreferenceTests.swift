//
//  SpeechLanguagePreferenceTests.swift
//  FlightDeckRemoteTests
//
//  The persisted dictation-language preference (PRD §7): the chosen
//  `SpeechLanguage` round-trips through the injected store, and an
//  absent/unrecognized value falls back to English.
//

import Testing
import Foundation
@testable import FlightDeckRemote

/// In-memory `SpeechLanguageStoring` double (mirrors
/// `InMemoryNotificationSettingsStore`) so these tests never touch real
/// `UserDefaults`.
final class InMemorySpeechLanguageStore: SpeechLanguageStoring {
    private(set) var saved: SpeechLanguage?
    private var current: SpeechLanguage?

    init(language: SpeechLanguage? = nil) {
        self.current = language
    }

    func load() -> SpeechLanguage {
        current ?? .fallback
    }

    func save(_ language: SpeechLanguage) {
        current = language
        saved = language
    }
}

@MainActor
struct SpeechLanguagePreferenceTests {

    @Test func defaultsToEnglishWhenUnset() {
        let prefs = SpeechLanguagePreference(store: InMemorySpeechLanguageStore())
        #expect(prefs.language == .english)
    }

    @Test func loadsPersistedValue() {
        let prefs = SpeechLanguagePreference(store: InMemorySpeechLanguageStore(language: .dutch))
        #expect(prefs.language == .dutch)
    }

    @Test func mutationPersists() {
        let store = InMemorySpeechLanguageStore()
        let prefs = SpeechLanguagePreference(store: store)

        prefs.language = .dutch

        #expect(store.saved == .dutch)
        #expect(SpeechLanguagePreference(store: store).language == .dutch)
    }

    @Test func localeMapsToBcp47Identifier() {
        #expect(SpeechLanguage.english.locale.identifier == "en-US")
        #expect(SpeechLanguage.dutch.locale.identifier == "nl-NL")
    }

    @Test func unrecognizedRawValueFallsBackToEnglish() {
        // A locale identifier for a language no longer offered must not crash
        // or persist as an invalid choice — it reads back as the fallback.
        let defaults = UserDefaults(suiteName: "speech-language-test-\(#function)")!
        defaults.set("xx-YY", forKey: "agency.neworange.flightdeck.remote.speech.language")
        let store = UserDefaultsSpeechLanguageStore(defaults: defaults)
        #expect(store.load() == .english)
        defaults.removePersistentDomain(forName: "speech-language-test-\(#function)")
    }
}
