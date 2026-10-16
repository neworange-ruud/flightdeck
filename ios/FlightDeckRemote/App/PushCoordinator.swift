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

    /// The agent session whose chat is on screen right now (set by
    /// `AgentChatView`), so a notification for it doesn't banner over the very
    /// conversation the user is reading (`ForegroundPresentationPolicy`).
    var visibleSessionId: String?

    /// Event ids presented as LOCAL notifications this launch (local
    /// notifications only exist while the app is foregrounded, so the process
    /// lifetime is the right scope).
    private var locallyPresentedEventIds: Set<String> = []

    private let center: UNUserNotificationCenter
    private let pushLog: PresentedPushLog

    init(center: UNUserNotificationCenter = .current(), pushLog: PresentedPushLog = PresentedPushLog()) {
        self.center = center
        self.pushLog = pushLog
    }

    /// Decide how a notification arriving in the foreground is presented, and
    /// remember local presentations — see `ForegroundPresentationPolicy`.
    func presentation(
        source: ForegroundPresentationPolicy.Source,
        userInfo: [AnyHashable: Any],
        isPassive: Bool
    ) -> ForegroundPresentationPolicy.Presentation {
        let payload = PushPayload(userInfo: userInfo)
        let eventId = payload?.eventId.isEmpty == false ? payload?.eventId : nil
        let decision = ForegroundPresentationPolicy.decide(
            source: source,
            eventId: eventId,
            sessionId: payload?.deepLink.sessionId.rawValue,
            isPassive: isPassive,
            presentedByPush: eventId.map(pushLog.contains) ?? false,
            presentedLocally: eventId.map(locallyPresentedEventIds.contains) ?? false,
            visibleSessionId: visibleSessionId)
        if source == .local, let eventId {
            locallyPresentedEventIds.insert(eventId)
        }
        return decision
    }

    /// `AgentChatView` left the screen; clear only if it was still the visible one.
    func chatDidDisappear(sessionId: String) {
        if visibleSessionId == sessionId { visibleSessionId = nil }
    }

    /// Attach the app's router so notification taps can navigate.
    func attach(router: AppRouter) {
        self.router = router
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
