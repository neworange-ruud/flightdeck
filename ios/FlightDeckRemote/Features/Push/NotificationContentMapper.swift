//
//  NotificationContentMapper.swift
//  FlightDeckRemote
//
//  Maps a typed `Wire.AgentEvent` to notification content (PRD §5.2 copy),
//  mirroring the relay's Rust `apns::notification_content` so both platforms
//  agree on the wire/display shape. Two layers, so the copy/deep-link logic is
//  testable without a UserNotifications host:
//   - `AgentEventNotification` — a pure, `Equatable` display model (title,
//     body, sound kind, urgency, deep link, event id).
//   - `NotificationContentMapper.content(for:settings:)` — combines that model
//     with `NotificationPolicy` to produce a `UNNotificationContent`, or `nil`
//     when the event is suppressed (toggle off / project muted).
//
//  The `userInfo` payload it stamps (`event_id` + `deep_link`) is byte-shape
//  identical to the APNs alert payload the relay builds, so the one
//  `PushPayload` parser handles both a tapped local notification and a tapped
//  remote push (push-deeplink).
//

import Foundation
import UserNotifications

/// The pure display model of a notification for an agent event.
struct AgentEventNotification: Equatable {
    /// Which sound a notification should carry.
    enum Sound: Equatable {
        /// The distinct, urgent *needs input* tone (PRD §5.2).
        case needsInput
        /// The standard completion chime.
        case chime
        /// No sound (settings suppressed it).
        case silent
    }

    /// How urgently to present (maps to `UNNotificationInterruptionLevel`).
    enum Urgency: Equatable {
        /// Break-through, time-sensitive (needs input).
        case timeSensitive
        /// Standard (finished / error).
        case active
    }

    let title: String
    let body: String
    let sound: Sound
    let urgency: Urgency
    let eventId: String
    let deepLink: Wire.DeepLink
}

enum NotificationContentMapper {
    /// The display model for an event, before settings are applied. `sound` is
    /// the event's *preferred* sound; the final sound (incl. silencing) is
    /// resolved in `content(for:settings:)` via `NotificationPolicy`.
    static func displayModel(for event: Wire.AgentEvent) -> AgentEventNotification {
        switch event.kind {
        case let .needsInput(preview):
            return AgentEventNotification(
                title: event.title,
                body: preview,
                sound: .needsInput,
                urgency: .timeSensitive,
                eventId: event.eventId.rawValue,
                deepLink: event.deepLink)
        case let .finished(summary, filesChanged, readyToPush):
            return AgentEventNotification(
                title: event.title,
                body: finishedBody(summary: summary, filesChanged: filesChanged, readyToPush: readyToPush),
                sound: .chime,
                urgency: .active,
                eventId: event.eventId.rawValue,
                deepLink: event.deepLink)
        case let .error(message):
            return AgentEventNotification(
                title: event.title,
                body: message,
                sound: .chime,
                urgency: .active,
                eventId: event.eventId.rawValue,
                deepLink: event.deepLink)
        }
    }

    /// "18 files changed · ready to push · <summary>" (PRD §5.2). Mirrors the
    /// relay's Rust copy exactly.
    static func finishedBody(summary: String, filesChanged: UInt32, readyToPush: Bool) -> String {
        let files = "\(filesChanged) file\(filesChanged == 1 ? "" : "s") changed"
        let ready = readyToPush ? " · ready to push" : ""
        return summary.isEmpty ? "\(files)\(ready)" : "\(files)\(ready) · \(summary)"
    }

    /// Build the `UNNotificationContent` for an event under the current
    /// settings, or `nil` if it should not be presented at all. `pairingId`,
    /// when known, is stamped into the payload so a tap deep-links to the
    /// originating machine (multi-pairing push, remote-control-b8d.10).
    static func content(
        for event: Wire.AgentEvent,
        settings: NotificationSettings,
        pairingId: String? = nil
    ) -> UNNotificationContent? {
        guard case let .present(withSound) = NotificationPolicy.outcome(for: event, settings: settings) else {
            return nil
        }
        let model = displayModel(for: event)
        let content = UNMutableNotificationContent()
        content.title = model.title
        content.body = model.body
        content.interruptionLevel = model.urgency == .timeSensitive ? .timeSensitive : .active
        content.threadIdentifier = model.deepLink.sessionId.rawValue
        content.userInfo = PushPayload.userInfo(
            eventId: model.eventId, deepLink: model.deepLink, pairingId: pairingId)

        if withSound {
            switch model.sound {
            case .needsInput:
                // A distinct, urgent tone (PRD §5.2). Falls back to the system
                // default if the bundled sound file is absent.
                content.sound = UNNotificationSound(named: UNNotificationSoundName("needs_input.caf"))
            case .chime:
                content.sound = .default
            case .silent:
                break
            }
        }
        return content
    }
}
