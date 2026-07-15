//
//  AppDelegate.swift
//  FlightDeckRemote
//
//  The UIKit application delegate, adopted by the SwiftUI `App` via
//  `@UIApplicationDelegateAdaptor` purely for the push-notification callbacks
//  SwiftUI doesn't surface natively (PRD §5.2/§9.1):
//   - remote-notification registration success/failure,
//   - silent *wake* pushes (the relay's zero-knowledge push, `apns.rs`),
//   - foreground presentation + tap handling as the
//     `UNUserNotificationCenterDelegate`.
//
//  It holds no state and makes no decisions — everything is delegated to the
//  shared `PushCoordinator`, which the SwiftUI `App` wires to the live router.
//

import UIKit
import UserNotifications

final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        // Become the notification-center delegate so foreground presentation
        // and taps route through us.
        UNUserNotificationCenter.current().delegate = self
        return true
    }

    // MARK: - Remote registration

    func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        Task { @MainActor in
            PushCoordinator.shared.didRegister(deviceToken: deviceToken)
        }
    }

    func application(
        _ application: UIApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        // Expected on the Simulator (no APNs) and offline; never fatal — the
        // app works without push, it just can't be woken while backgrounded.
        NSLog("FlightDeckRemote: remote notification registration failed: \(error)")
    }

    func application(
        _ application: UIApplication,
        didReceiveRemoteNotification userInfo: [AnyHashable: Any],
        fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void
    ) {
        // A silent *wake* push (spec §11 step 1): the transport's own reconnect
        // supervisor brings the link back and `resume`s the queued envelope,
        // after which `NotificationScheduler` renders the real notification.
        // Nothing content-bearing crosses the relay, so there is nothing to
        // decode here.
        completionHandler(.newData)
    }

    // MARK: - UNUserNotificationCenterDelegate

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        // Present even while foregrounded (banner + sound + Notification
        // Center); the sound was already gated by settings when the content was
        // built (`NotificationContentMapper`), so honoring it here is correct.
        completionHandler([.banner, .sound, .list])
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let userInfo = response.notification.request.content.userInfo
        Task { @MainActor in
            PushCoordinator.shared.handleTap(userInfo: userInfo)
            completionHandler()
        }
    }
}
