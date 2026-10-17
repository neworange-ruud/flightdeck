//
//  NotificationSettings.swift
//  FlightDeckRemote + FlightDeckNotificationService
//
//  The notification settings model (PRD §5.6/§9.2): three INDEPENDENT global
//  toggles — *Agent needs input*, *Agent finished*, *Completion chime* — plus
//  per-project mute, and the pure policy that applies them.
//
//  Shared with the Notification Service Extension (remote-control-bza0), which
//  must apply exactly the same rules to a decrypted push as the app applies to
//  a live event. The settings therefore persist in the App Group's
//  `UserDefaults` suite (`SharedContainer.defaults`); the app copies any values
//  an older build left in `.standard` across once at launch
//  (`NotificationSettingsMigration`).
//
//  The `@Observable` store the Settings screen binds to stays app-only
//  (`NotificationPreferences.swift`).
//

import Foundation

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

/// Which of the three notifying event kinds an event is. Spelled like the wire
/// `PushClass` (spec §7.2: `needs_input` | `error` | `done`), which the sealed
/// push carries; the app maps `Wire.EventKind` onto it (`finished` → `done`).
///
/// This is the ONE push-class type for both targets: the wire layer's
/// `Wire.PushClass` (the `push_notify` frame, Rust `common::PushClass`) is a
/// typealias of it, so the frame, the sealed payload and the notification
/// policy can never disagree on spelling.
enum PushEventClass: String, Codable, Hashable, Sendable {
    case needsInput = "needs_input"
    case error
    case done
}

/// The pure decision gate. No UIKit, no I/O — just the rules, so it is fully
/// unit-testable.
enum NotificationPolicy {
    /// Decide how to handle an event of `eventClass` in `projectId`.
    ///
    /// A muted project suppresses everything. Otherwise: *needs input* is gated
    /// by `agentNeedsInput` and always sounds when shown (its distinct, urgent
    /// tone); *done* and *error* are gated by `agentFinished` (both are
    /// turn-ending) and sound only when `completionChime` is on.
    static func outcome(
        eventClass: PushEventClass,
        projectId: String,
        settings: NotificationSettings
    ) -> NotificationOutcome {
        if settings.mutedProjectIds.contains(projectId) {
            return .suppressed
        }
        switch eventClass {
        case .needsInput:
            return settings.agentNeedsInput ? .present(sound: true) : .suppressed
        case .done, .error:
            return settings.agentFinished ? .present(sound: settings.completionChime) : .suppressed
        }
    }
}

/// Where the notification settings durably persist. A tiny seam (mirrors
/// `ActivityEventPersisting` / `AppLockSettingsProviding`) so tests inject an
/// in-memory double instead of touching real `UserDefaults`.
protocol NotificationSettingsStoring {
    func load() -> NotificationSettings
    func save(_ settings: NotificationSettings)
}

/// `UserDefaults`-backed persistence for the notification prefs. Defaults to
/// the App Group suite so the Notification Service Extension reads what the
/// Settings screen writes.
struct UserDefaultsNotificationSettingsStore: NotificationSettingsStoring {
    private let defaults: UserDefaults
    enum Key {
        static let needsInput = "agency.neworange.flightdeck.remote.notif.needsInput"
        static let finished = "agency.neworange.flightdeck.remote.notif.finished"
        static let chime = "agency.neworange.flightdeck.remote.notif.chime"
        static let muted = "agency.neworange.flightdeck.remote.notif.mutedProjects"
        static let all = [needsInput, finished, chime, muted]
    }

    init(defaults: UserDefaults = SharedContainer.defaults) {
        self.defaults = defaults
    }

    func load() -> NotificationSettings {
        #if DEBUG
        // UI-test hook (mirrors `-uitest-reset-pairing` / `-uitest-reset-applock`):
        // clear persisted prefs so a scenario that flips a toggle doesn't leak
        // into the next launch. `load()` runs once at init, so this resets to
        // the "everything on" defaults for the run.
        if ProcessInfo.processInfo.arguments.contains("-uitest-reset-notif-prefs") {
            Key.all.forEach { defaults.removeObject(forKey: $0) }
        }
        #endif
        return NotificationSettings(
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

/// One-time copy of the notification settings from the app's own
/// `UserDefaults.standard` (where builds before remote-control-bza0 kept them)
/// into the App Group suite. App-only: the extension's `.standard` is its own,
/// empty domain, so running this there would mark the migration done without
/// copying anything.
enum NotificationSettingsMigration {
    /// Set in the destination once the copy has run.
    static let markerKey = "agency.neworange.flightdeck.remote.notif.migratedToAppGroup"

    /// Copy every settings key present in `source` into `destination` (keys the
    /// destination already holds win — they are newer), then set the marker so
    /// later launches skip it. Returns whether it copied this time. The source
    /// is left intact, so a downgrade still finds its settings.
    @discardableResult
    static func migrateIfNeeded(from source: UserDefaults, to destination: UserDefaults) -> Bool {
        guard source !== destination, !destination.bool(forKey: markerKey) else { return false }
        for key in UserDefaultsNotificationSettingsStore.Key.all {
            guard destination.object(forKey: key) == nil, let value = source.object(forKey: key) else {
                continue
            }
            destination.set(value, forKey: key)
        }
        destination.set(true, forKey: markerKey)
        return true
    }
}
