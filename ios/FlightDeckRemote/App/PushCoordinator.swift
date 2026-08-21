//
//  PushCoordinator.swift
//  FlightDeckRemote
//
//  The thin bridge between UIKit's push callbacks (`AppDelegate`) and the rest
//  of the app: it owns the current APNs device token, requests notification
//  authorization + remote registration, and routes a notification tap into
//  navigation (PRD §5.2/§9.1).
//
//  It is a shared object because `AppDelegate` is instantiated by SwiftUI's
//  `UIApplicationDelegateAdaptor` and can't be handed dependencies directly;
//  the SwiftUI `App` attaches the live `AppRouter`, and the tab container
//  observes `deviceTokenHex` to register the token with the transport (the
//  token is opaque and travels the relay plane, spec §5.5). Deliberately kept
//  glue-thin — all decision logic lives in the pure, tested `Features/Push`
//  types.
//

import Foundation
import Observation
import UIKit
import UserNotifications

@MainActor
@Observable
final class PushCoordinator {
    /// The app-wide instance `AppDelegate` talks to.
    static let shared = PushCoordinator()

    /// The latest APNs device token, lowercase hex — `nil` until APNs hands one
    /// over (it never does on the Simulator, which has no push service). The
    /// tab container observes this to register it with the relay.
    private(set) var deviceTokenHex: String?
    /// The APNs environment this build targets (sandbox in DEBUG).
    let environment: Wire.ApnsEnvironment = PushEnvironment.current

    /// The live router, for routing notification taps. Weak: the SwiftUI `App`
    /// owns it.
    weak var router: AppRouter?

    /// The background wake performer, registered by `MainTabView` (which owns
    /// the live `TransportCoordinator` + `NotificationScheduler`). A silent wake
    /// push (spec §11 step 1, remote-control-0ef.4) runs this to reconnect the
    /// backgrounded-and-torn-down transport, let queued envelopes replay,
    /// schedule their local notifications, and tear back down — all before the
    /// background execution window closes. `nil` until the paired UI mounts and
    /// registers it (an unpaired app has nothing to wake). Returns whether any
    /// new data was fetched (drives the `UIBackgroundFetchResult`).
    private var wakeHandler: (@MainActor () async -> Bool)?

    private let center: UNUserNotificationCenter

    init(center: UNUserNotificationCenter = .current()) {
        self.center = center
    }

    /// Attach the app's router so notification taps can navigate.
    func attach(router: AppRouter) {
        self.router = router
    }

    /// Register the background wake performer (remote-control-0ef.4). Called by
    /// `MainTabView` once its live transport is wired; overwriting on remount is
    /// fine (the newest coordinator wins). See `wakeHandler`.
    func registerWakeHandler(_ handler: @escaping @MainActor () async -> Bool) {
        self.wakeHandler = handler
    }

    /// Handle a silent wake push (spec §11 step 1, remote-control-0ef.4): run
    /// the registered wake performer to completion (reconnect → resume/fetch →
    /// schedule notifications → tear down) and report the fetch result. With no
    /// handler registered (unpaired, or the UI not yet mounted) there is nothing
    /// to wake, so `.noData`.
    func handleWakePush() async -> UIBackgroundFetchResult {
        guard let wakeHandler else { return .noData }
        return await wakeHandler() ? .newData : .noData
    }

    /// Ask for notification authorization and, if granted, register for remote
    /// notifications (which eventually calls back into `didRegister`). Safe to
    /// call repeatedly — the system only prompts once per install.
    func requestAuthorizationAndRegister() {
        center.requestAuthorization(options: [.alert, .sound, .badge]) { granted, error in
            if let error {
                assertionFailure("push authorization failed: \(error)")
            }
            guard granted else { return }
            // Registration must happen on the main thread.
            Task { @MainActor in
                UIApplication.shared.registerForRemoteNotifications()
            }
        }
    }

    /// Called by `AppDelegate` with the raw APNs token bytes.
    func didRegister(deviceToken: Data) {
        deviceTokenHex = PushEnvironment.hexToken(from: deviceToken)
    }

    /// Route a tapped notification's payload to the target agent.
    func handleTap(userInfo: [AnyHashable: Any]) {
        guard let router else { return }
        PushRouting.route(userInfo: userInfo, in: router)
    }
}
