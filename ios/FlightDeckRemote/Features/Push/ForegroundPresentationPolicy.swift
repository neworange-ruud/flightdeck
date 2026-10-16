//
//  ForegroundPresentationPolicy.swift
//  FlightDeckRemote
//
//  How a notification is presented while the app is in the foreground
//  (`UNUserNotificationCenterDelegate.willPresent`), so one agent event never
//  produces two banners (remote-control-bza0).
//
//  One event can reach the phone twice: as the desktop's sealed push (rewritten
//  by the Notification Service Extension) and as a live `AgentEvent` the app
//  turns into a local notification. The desktop guarantees both carry the same
//  `event_id`, and both use it as the request identifier (the push via
//  `apns-collapse-id`, the local one in `NotificationScheduler`), so Notification
//  Center already keeps one entry. This policy keeps the banner + sound to the
//  FIRST presentation:
//
//   - a remote push whose event was already presented locally → list only;
//   - a local notification whose event the extension already presented as a
//     push (`PresentedPushLog`, which also covers pushes shown while the app was
//     backgrounded) or that was already presented locally → list only;
//   - anything for the session the user is looking at right now → list only;
//   - a `.passive` (muted / toggled-off) delivery → list only;
//   - otherwise banner + sound + list.
//
//  Pure, so the whole table is unit-tested.
//

import UserNotifications

enum ForegroundPresentationPolicy {
    enum Source: Equatable {
        /// An APNs push (rewritten by the extension, or the generic fallback).
        case remote
        /// A local notification posted by `NotificationScheduler`.
        case local
    }

    enum Presentation: Equatable {
        /// Banner, sound and Notification Center.
        case full
        /// Notification Center only — no banner, no sound.
        case listOnly

        var options: UNNotificationPresentationOptions {
            switch self {
            case .full: return [.banner, .sound, .list]
            case .listOnly: return [.list]
            }
        }
    }

    static func decide(
        source: Source,
        eventId: String?,
        sessionId: String?,
        isPassive: Bool,
        presentedByPush: Bool,
        presentedLocally: Bool,
        visibleSessionId: String?
    ) -> Presentation {
        if isPassive { return .listOnly }
        if let sessionId, let visibleSessionId, sessionId == visibleSessionId { return .listOnly }
        guard let eventId, !eventId.isEmpty else { return .full }
        switch source {
        case .remote:
            return presentedLocally ? .listOnly : .full
        case .local:
            return (presentedByPush || presentedLocally) ? .listOnly : .full
        }
    }
}
