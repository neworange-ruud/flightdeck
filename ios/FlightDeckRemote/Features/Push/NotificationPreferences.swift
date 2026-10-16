//
//  NotificationPreferences.swift
//  FlightDeckRemote
//
//  The app's live, persisted notification preferences (PRD §5.6/§9.2) and the
//  `Wire.AgentEvent` entry point into the shared policy.
//
//  The value model (`NotificationSettings`), the pure gate
//  (`NotificationPolicy`) and the `UserDefaults` persistence live in
//  `Shared/NotificationSettings.swift`, compiled into the Notification Service
//  Extension too, so a decrypted push and a live event obey identical rules
//  (remote-control-bza0). This file adds:
//   - `NotificationPolicy.outcome(for:settings:)` — the event-typed overload.
//   - `NotificationPreferences` — the `@Observable`, persisted store the
//     Settings screen binds to and the push layer reads (mirrors
//     `AppLockController`'s injectable-provider shape).
//

import Foundation
import Observation

extension PushEventClass {
    /// The notifying class of a live agent event (`finished` is the wire's `done`).
    init(_ kind: Wire.EventKind) {
        switch kind {
        case .needsInput: self = .needsInput
        case .finished: self = .done
        case .error: self = .error
        }
    }
}

extension NotificationPolicy {
    /// Decide how to handle `event` under `settings` — see
    /// `outcome(eventClass:projectId:settings:)` for the rules.
    static func outcome(for event: Wire.AgentEvent, settings: NotificationSettings) -> NotificationOutcome {
        outcome(
            eventClass: PushEventClass(event.kind),
            projectId: event.deepLink.projectId.rawValue,
            settings: settings)
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
