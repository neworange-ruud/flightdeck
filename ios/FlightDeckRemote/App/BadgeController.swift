//
//  BadgeController.swift
//  FlightDeckRemote
//
//  Home-screen badge plumbing (PRD §5.1): the badge shows the count of
//  agents currently waiting for input across all projects.
//
//  This is a pure utility — it does not observe app state or wire itself
//  into the app lifecycle. The push-notification task is expected to call
//  `BadgeController.setBadge(_:)` whenever a "needs input" event arrives
//  (PRD §5.2 "Needs input" notifications) and whenever that count changes
//  (e.g. an agent's question is answered, dropping the count), and to call
//  `BadgeController.requestBadgeAuthorizationIfNeeded()` once during onboarding
//  or pairing, before the first badge update is expected to be visible.
//

import UserNotifications

/// Sets and clears the app's home-screen badge (the running count of agents
/// waiting for input, per PRD §5.1/§5.2).
enum BadgeController {
    /// Sets the home-screen badge to `count`, the number of agents currently
    /// waiting for input.
    ///
    /// Negative counts are clamped to 0 (a badge count can't be negative;
    /// callers computing a delta — e.g. decrementing after an agent's
    /// question is answered — may otherwise underflow past zero).
    ///
    /// Uses `UNUserNotificationCenter.setBadgeCount(_:withCompletionHandler:)`
    /// (iOS 16+), which — unlike the deprecated
    /// `UIApplication.applicationIconBadgeNumber` — does not require the
    /// call to happen on the main thread and does not silently no-op if
    /// notification authorization hasn't been granted for the `.badge`
    /// option.
    static func setBadge(_ count: Int, center: UNUserNotificationCenter = .current()) {
        center.setBadgeCount(clampedBadgeCount(count)) { error in
            if let error {
                // Badge updates are best-effort UI polish; a failure here
                // (e.g. authorization not yet granted) should never crash
                // or block the caller's event handling.
                assertionFailure("BadgeController.setBadge failed: \(error)")
            }
        }
    }

    /// Clamps a raw badge count to a value `UNUserNotificationCenter` can
    /// display: negative counts (e.g. from a caller computing a delta that
    /// underflows past zero) become 0. Exposed separately from `setBadge`
    /// so the clamping rule can be unit-tested without touching
    /// `UNUserNotificationCenter`, which needs notification authorization
    /// and a running app host to behave predictably.
    static func clampedBadgeCount(_ count: Int) -> Int {
        max(0, count)
    }

    /// Requests notification authorization for the `.badge` option if the
    /// app hasn't already asked (or been granted/denied) it.
    ///
    /// Safe to call multiple times — `UNUserNotificationCenter` tracks
    /// authorization status itself, and this only prompts the user once
    /// per install (or after they reset the app's notification permission).
    static func requestBadgeAuthorizationIfNeeded(
        center: UNUserNotificationCenter = .current(),
        completion: ((Bool) -> Void)? = nil
    ) {
        center.getNotificationSettings { settings in
            switch settings.authorizationStatus {
            case .notDetermined:
                center.requestAuthorization(options: [.badge]) { granted, error in
                    if let error {
                        assertionFailure("BadgeController.requestBadgeAuthorizationIfNeeded failed: \(error)")
                    }
                    completion?(granted)
                }
            case .authorized, .provisional, .ephemeral:
                completion?(true)
            case .denied:
                completion?(false)
            @unknown default:
                completion?(false)
            }
        }
    }
}
