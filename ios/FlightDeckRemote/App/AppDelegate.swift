//
//  AppDelegate.swift
//  FlightDeckRemote
//
//  The UIKit application delegate, adopted by the SwiftUI `App` via
//  `@UIApplicationDelegateAdaptor` purely for the push-notification callbacks
//  SwiftUI doesn't surface natively (PRD §5.2/§9.1):
//   - remote-notification registration success/failure,
//   - foreground presentation + tap handling as the
//     `UNUserNotificationCenterDelegate`.
//
//  There is no `didReceiveRemoteNotification` handler: pushes are visible
//  alerts whose sealed payload the Notification Service Extension decrypts
//  before iOS shows them (spec §7.2, remote-control-bza0). The old silent
//  *wake* push — reconnect, resume, post a local notification inside the
//  background window — was throttled by iOS and dropped entirely after a
//  force-quit, so it is gone, together with the `remote-notification`
//  background mode.
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
        // app works without push, it just gets no notifications while away.
        NSLog("FlightDeckRemote: remote notification registration failed: \(error)")
    }

    // MARK: - UNUserNotificationCenterDelegate

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        // Present while foregrounded, but never twice for one agent event: the
        // same event can arrive as a push and as a live local notification
        // (`ForegroundPresentationPolicy`). Sounds were already gated by the
        // user's settings when the content was built.
        let request = notification.request
        let source: ForegroundPresentationPolicy.Source =
            request.trigger is UNPushNotificationTrigger ? .remote : .local
        let userInfo = request.content.userInfo
        let isPassive = request.content.interruptionLevel == .passive
        Task { @MainActor in
            let presentation = PushCoordinator.shared.presentation(
                source: source, userInfo: userInfo, isPassive: isPassive)
            completionHandler(presentation.options)
        }
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
