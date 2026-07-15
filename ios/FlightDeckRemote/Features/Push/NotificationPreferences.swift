//
//  NotificationPreferences.swift
//  FlightDeckRemote
//
//  The notification settings model (PRD §5.6/§9.2): three INDEPENDENT global
//  toggles — *Agent needs input*, *Agent finished*, *Completion chime* — plus
//  per-project mute. Persisted so they survive relaunch, and consulted by the
//  push layer to decide whether (and how loudly) an incoming `Wire.AgentEvent`
//  is presented as a notification.
//
//  Split into three pieces so the decision logic is unit-testable without any
//  UIKit / UserNotifications host:
//   - `NotificationSettings` — a plain, `Equatable` value snapshot of the prefs.
//   - `NotificationPolicy` — the pure gate: given an event + settings, decide
//     `suppressed` vs. `present(sound:)`.
//   - `NotificationPreferences` — the `@Observable`, persisted store the
//     Settings screen binds to and the push layer reads (mirrors
//     `AppLockController`'s injectable-provider shape).
//

import Foundation
import Observation

/// A snapshot of the user's notification preferences. Defaults are "everything
/// on" (the app is useless silent), matching a fresh install.
struct NotificationSettings: Equatable, Sendable {
    /// Present *needs input* events (the urgent, most important case, PRD §5.2).
    var agentNeedsInput: Bool = true
    /// Present *finished* (and *error*, the other turn-ending) events.
    var agentFinished: Bool = true
    /// Whether a *finished*/*error* notification plays the completion chime.
    /// Independent of `agentFinished`: you can be notified silently.
    var completionChime: Bool = true
    /// Project ids whose notifications are entirely suppressed (per-project
    /// mute, PRD §9.2). Stored as raw id strings so it round-trips through
    /// `UserDefaults` and is agnostic of `Wire.ProjectId`.
    var mutedProjectIds: Set<String> = []
}

/// The outcome of deciding whether to present a notification for an event.
enum NotificationOutcome: Equatable, Sendable {
    /// Do not present anything (toggle off, or the project is muted).
    case suppressed
    /// Present it; `sound` says whether to play a sound.
    case present(sound: Bool)
}

/// The pure decision gate. No UIKit, no I/O — just the rules, so it is fully
/// unit-testable.
enum NotificationPolicy {
    /// Decide how to handle `event` under `settings`.
    ///
    /// A muted project suppresses everything. Otherwise: *needs input* is gated
    /// by `agentNeedsInput` and always sounds when shown (its distinct, urgent
    /// tone); *finished* and *error* are gated by `agentFinished` (both are
    /// turn-ending) and sound only when `completionChime` is on.
    static func outcome(for event: Wire.AgentEvent, settings: NotificationSettings) -> NotificationOutcome {
        if settings.mutedProjectIds.contains(event.deepLink.projectId.rawValue) {
            return .suppressed
        }
        switch event.kind {
        case .needsInput:
            return settings.agentNeedsInput ? .present(sound: true) : .suppressed
        case .finished, .error:
            return settings.agentFinished ? .present(sound: settings.completionChime) : .suppressed
        }
    }
}

/// Where `NotificationPreferences` durably persists its settings. A tiny seam
/// (mirrors `ActivityEventPersisting` / `AppLockSettingsProviding`) so tests
/// inject an in-memory double instead of touching real `UserDefaults`.
protocol NotificationSettingsStoring {
    func load() -> NotificationSettings
    func save(_ settings: NotificationSettings)
}

/// `UserDefaults`-backed persistence for the notification prefs.
struct UserDefaultsNotificationSettingsStore: NotificationSettingsStoring {
    private let defaults: UserDefaults
    private enum Key {
        static let needsInput = "agency.neworange.flightdeck.remote.notif.needsInput"
        static let finished = "agency.neworange.flightdeck.remote.notif.finished"
        static let chime = "agency.neworange.flightdeck.remote.notif.chime"
        static let muted = "agency.neworange.flightdeck.remote.notif.mutedProjects"
    }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func load() -> NotificationSettings {
        NotificationSettings(
            // Absent keys default to `true` (fresh install = everything on);
            // `object(forKey:)` distinguishes "unset" from an explicit `false`.
            agentNeedsInput: defaults.object(forKey: Key.needsInput) as? Bool ?? true,
            agentFinished: defaults.object(forKey: Key.finished) as? Bool ?? true,
            completionChime: defaults.object(forKey: Key.chime) as? Bool ?? true,
            mutedProjectIds: Set(defaults.stringArray(forKey: Key.muted) ?? [])
        )
    }

    func save(_ settings: NotificationSettings) {
        defaults.set(settings.agentNeedsInput, forKey: Key.needsInput)
        defaults.set(settings.agentFinished, forKey: Key.finished)
        defaults.set(settings.completionChime, forKey: Key.chime)
        defaults.set(Array(settings.mutedProjectIds).sorted(), forKey: Key.muted)
    }
}

/// The app's live, observable notification preferences. The Settings screen
/// binds its toggles here (via `@Bindable`) and the push layer reads
/// `settings` to gate presentation. Every mutation persists through the
/// injected `NotificationSettingsStoring`.
@MainActor
@Observable
final class NotificationPreferences {
    var agentNeedsInput: Bool { didSet { persist() } }
    var agentFinished: Bool { didSet { persist() } }
    var completionChime: Bool { didSet { persist() } }
    private(set) var mutedProjectIds: Set<String> { didSet { persist() } }

    private let store: NotificationSettingsStoring

    init(store: NotificationSettingsStoring = UserDefaultsNotificationSettingsStore()) {
        self.store = store
        let loaded = store.load()
        self.agentNeedsInput = loaded.agentNeedsInput
        self.agentFinished = loaded.agentFinished
        self.completionChime = loaded.completionChime
        self.mutedProjectIds = loaded.mutedProjectIds
    }

    /// The current settings as a plain value (what `NotificationPolicy` and the
    /// content mapper consume).
    var settings: NotificationSettings {
        NotificationSettings(
            agentNeedsInput: agentNeedsInput,
            agentFinished: agentFinished,
            completionChime: completionChime,
            mutedProjectIds: mutedProjectIds
        )
    }

    /// Whether a project's notifications are muted.
    func isMuted(projectId: String) -> Bool {
        mutedProjectIds.contains(projectId)
    }

    /// Mute or unmute a project (per-project mute, PRD §9.2).
    func setMuted(_ muted: Bool, projectId: String) {
        if muted {
            guard !mutedProjectIds.contains(projectId) else { return }
            mutedProjectIds.insert(projectId)
        } else {
            guard mutedProjectIds.contains(projectId) else { return }
            mutedProjectIds.remove(projectId)
        }
    }

    private func persist() {
        store.save(settings)
    }
}
