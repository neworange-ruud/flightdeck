//
//  NotificationService.swift
//  FlightDeckNotificationService
//
//  The Notification Service Extension (remote-control-bza0). iOS runs it for
//  every `mutable-content: 1` push before showing it; it decrypts the
//  `fd_push` payload the desktop sealed (spec §7.2) and rewrites the relay's
//  generic alert with the real title/body, sound, interruption level and deep
//  link.
//
//  Deliberately thin: every decision lives in the shared, unit-tested
//  `PushNotificationRewriter`. This class only wires it to the Keychain and the
//  App Group settings, and guarantees the content handler is called exactly
//  once — with the original, untouched content on any failure or when iOS
//  says time is up. A notification is never dropped.
//
//  Memory: the extension runs under a ~24 MB limit. It links only Foundation,
//  CryptoKit, Security and UserNotifications, and does one Keychain read, one
//  ECDH + HKDF and one AEAD open per push.
//

import Foundation
import os
import UserNotifications

final class NotificationService: UNNotificationServiceExtension {
    private let logger = Logger(
        subsystem: "agency.neworange.flightdeck.remote.notification-service",
        category: "push")
    private let lock = NSLock()
    private var contentHandler: ((UNNotificationContent) -> Void)?
    private var originalContent: UNNotificationContent?

    override func didReceive(
        _ request: UNNotificationRequest,
        withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void
    ) {
        lock.lock()
        self.contentHandler = contentHandler
        self.originalContent = request.content
        lock.unlock()

        let (content, result) = PushNotificationRewriter.content(
            for: request.content,
            keys: KeychainPushKeyMaterialProvider(),
            settings: UserDefaultsNotificationSettingsStore().load())
        switch result {
        case let .rewritten(push):
            // Never log content; the outcome is enough to diagnose.
            logger.info("push rewritten (suppressed: \(push.suppressed, privacy: .public))")
        case let .fallback(reason):
            logger.notice("push left generic: \(String(describing: reason), privacy: .public)")
        }
        deliver(content)
    }

    override func serviceExtensionTimeWillExpire() {
        // Out of time: show the relay's generic text rather than nothing.
        lock.lock()
        let original = originalContent
        lock.unlock()
        logger.notice("push left generic: serviceExtensionTimeWillExpire")
        if let original { deliver(original) }
    }

    /// Call the content handler at most once.
    private func deliver(_ content: UNNotificationContent) {
        lock.lock()
        let handler = contentHandler
        contentHandler = nil
        lock.unlock()
        handler?(content)
    }
}
