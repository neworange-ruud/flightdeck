//
//  SpeechLanguage.swift
//  FlightDeckRemote
//
//  The user-selectable INPUT language for push-to-talk dictation (PRD §7).
//  `SFSpeechRecognizer` is locale-bound: the no-argument initializer follows
//  the device locale, which is why dictation effectively defaulted to English.
//  This lets the user pin a specific recognition language independent of the
//  device's own locale.
//
//  Split to mirror `NotificationPreferences`:
//   - `SpeechLanguage` — the closed set of offered languages (+ their locale).
//   - `SpeechLanguageStoring` — the persistence seam (in-memory double in tests).
//   - `SpeechLanguagePreference` — the `@Observable`, persisted store the
//     Settings screen binds to. `DictationController` reads the persisted value
//     when it builds its recognizer, so a Settings change takes effect on the
//     next hold without recreating anything.
//

import Foundation
import Observation

/// The languages offered for speech-to-text. The raw value is the BCP-47 locale
/// identifier handed to `SFSpeechRecognizer(locale:)`.
enum SpeechLanguage: String, CaseIterable, Sendable, Identifiable {
    case english = "en-US"
    case dutch = "nl-NL"

    var id: String { rawValue }

    /// The recognizer locale for this language.
    var locale: Locale { Locale(identifier: rawValue) }

    /// The label shown in Settings, in the language's own name.
    var displayName: String {
        switch self {
        case .english: return "English"
        case .dutch: return "Nederlands"
        }
    }

    /// The default when nothing is persisted yet — English, matching the prior
    /// implicit behavior so existing users see no change until they choose.
    static let fallback: SpeechLanguage = .english
}

/// Where `SpeechLanguagePreference` durably persists the chosen language. A tiny
/// seam (mirrors `NotificationSettingsStoring`) so tests inject an in-memory
/// double instead of touching real `UserDefaults`.
protocol SpeechLanguageStoring {
    func load() -> SpeechLanguage
    func save(_ language: SpeechLanguage)
}

/// `UserDefaults`-backed persistence for the chosen dictation language.
struct UserDefaultsSpeechLanguageStore: SpeechLanguageStoring {
    private let defaults: UserDefaults
    private static let key = "agency.neworange.flightdeck.remote.speech.language"

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func load() -> SpeechLanguage {
        guard let raw = defaults.string(forKey: Self.key),
              let language = SpeechLanguage(rawValue: raw) else {
            // Absent or unrecognized (e.g. a removed language) → fall back.
            return .fallback
        }
        return language
    }

    func save(_ language: SpeechLanguage) {
        defaults.set(language.rawValue, forKey: Self.key)
    }
}

/// The app's live, observable dictation-language preference. The Settings screen
/// binds to it; every mutation persists through the injected store. The
/// recognizer reads the persisted value lazily (see `DictationController`), so
/// this deliberately holds no reference to the Voice feature.
@MainActor
@Observable
final class SpeechLanguagePreference {
    var language: SpeechLanguage { didSet { store.save(language) } }

    private let store: SpeechLanguageStoring

    init(store: SpeechLanguageStoring = UserDefaultsSpeechLanguageStore()) {
        self.store = store
        self.language = store.load()
    }
}
