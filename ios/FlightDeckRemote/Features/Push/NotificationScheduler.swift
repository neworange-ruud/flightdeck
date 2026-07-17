//
//  NotificationScheduler.swift
//  FlightDeckRemote
//
//  Posts local notifications for incoming `Wire.AgentEvent`s, deduped by
//  `event_id` (PRD §5.8, spec §6.4: a queued-then-replayed event must not
//  double-fire). Because the relay is zero-knowledge it can only send a silent
//  *wake* push (see `remote/relay/src/apns.rs`); the phone reconnects,
//  `resume`s the queued envelope, and this scheduler renders the real, typed,
//  deep-linked notification locally from the decrypted event.
//
//  The `UNUserNotificationCenter` dependency is behind a tiny seam so `ingest`
//  is unit-testable (dedup + settings gating) without a notification host.
//

import Foundation
import Observation
import UserNotifications

/// Where scheduled notification requests go. Seam for tests.
protocol UserNotificationScheduling {
    func add(_ request: UNNotificationRequest)
}

/// Real scheduler: hands requests to the system notification center.
struct SystemUserNotificationScheduler: UserNotificationScheduling {
    func add(_ request: UNNotificationRequest) {
        UNUserNotificationCenter.current().add(request)
    }
}

@MainActor
@Observable
final class NotificationScheduler {
    /// Event ids already turned into a notification request this launch — the
    /// dedup guard. (The request identifier is also the event id, so even a
    /// cross-launch repeat coalesces at the UN layer.)
    private var scheduledEventIds: Set<String> = []
    private let scheduler: UserNotificationScheduling

    init(scheduler: UserNotificationScheduling = SystemUserNotificationScheduler()) {
        self.scheduler = scheduler
    }

    /// Present notifications for any not-yet-seen events, honoring `settings`
    /// (toggles + per-project mute). Deduped by `event_id`; suppressed events
    /// still count as "seen" so flipping a toggle later doesn't retro-fire them.
    func ingest(_ events: [Wire.AgentEvent], settings: NotificationSettings) {
        for event in events where scheduledEventIds.insert(event.eventId.rawValue).inserted {
            guard let content = NotificationContentMapper.content(for: event, settings: settings) else {
                continue
            }
            let request = UNNotificationRequest(
                identifier: event.eventId.rawValue,
                content: content,
                trigger: nil) // deliver immediately
            scheduler.add(request)
        }
    }
}
