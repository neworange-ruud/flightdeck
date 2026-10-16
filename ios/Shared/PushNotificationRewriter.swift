//
//  PushNotificationRewriter.swift
//  FlightDeckRemote + FlightDeckNotificationService
//
//  What the Notification Service Extension does to an incoming push
//  (remote-control-bza0), as a pure function of the notification's `userInfo`,
//  the pairing key material and the notification settings — so every branch is
//  unit-tested in the app's test target rather than only on a device.
//
//   1. Parse `fd_push`, find the pairing, derive its push key, open and
//      validate the `PushPayload` (`PushPayloadCrypto.openEnvelope`).
//   2. Apply the same policy the app applies to live events
//      (`NotificationPolicy`: per-project mute + the three toggles).
//   3. Present: real title/body, the class's sound and interruption level, and
//      the deep-link `userInfo` the app's tap handling (`PushPayload` →
//      `PushRouting`) already understands, including `pairing_id`.
//      Suppressed: as quiet as iOS allows without the notification-filtering
//      entitlement (which we do not request) — `.passive` interruption level
//      (no banner, no sound, no screen wake; it lands silently in Notification
//      Center), no sound, the original generic text, relevance 0. It cannot be
//      removed entirely.
//   4. ANY failure → the original generic content, unchanged. A notification is
//      never dropped and never shows partial or wrong content.
//

import Foundation
import UserNotifications

/// The decided content of a rewritten push, as plain values.
struct RewrittenPush: Equatable {
    enum Sound: Equatable {
        /// The distinct, urgent *needs input* tone.
        case needsInput
        /// The system default (completion chime).
        case standard
        /// No sound.
        case none
    }

    enum Interruption: Equatable {
        case timeSensitive
        case active
        /// Silent delivery to Notification Center (muted / toggled off).
        case passive
    }

    let title: String
    let subtitle: String
    let body: String
    let sound: Sound
    let interruption: Interruption
    let threadIdentifier: String
    let relevanceScore: Double
    /// Whether the user's settings suppressed this event.
    let suppressed: Bool
    let eventId: String
    let pairingId: String
    let projectId: String
    let sessionId: String
    let itemId: String?
}

enum PushRewriteResult: Equatable {
    case rewritten(RewrittenPush)
    /// Leave the notification exactly as the relay sent it.
    case fallback(SealedPushError)
}

enum PushNotificationRewriter {
    /// Bundled sound for *needs input* (the same name the app's local
    /// notifications use); iOS plays the default sound if it is absent.
    static let needsInputSoundName = "needs_input.caf"
    /// Thread that groups suppressed (muted / toggled-off) deliveries.
    static let suppressedThreadIdentifier = "flightdeck.suppressed"

    /// Decide what to show for a push. `originalTitle`/`originalBody` are the
    /// relay's generic alert text, reused for suppressed deliveries.
    static func rewrite(
        userInfo: [AnyHashable: Any],
        originalTitle: String,
        originalBody: String,
        keys: PushKeyMaterialProviding,
        settings: NotificationSettings
    ) -> PushRewriteResult {
        let envelope: SealedPushEnvelope
        let payload: DecryptedPushPayload
        do {
            envelope = try SealedPushEnvelope(userInfo: userInfo)
            let material: PushKeyMaterial
            do {
                guard let found = try keys.keyMaterial(pairingId: envelope.pairingId) else {
                    return .fallback(.unknownPairing)
                }
                material = found
            } catch let error as SealedPushError {
                throw error
            } catch is PairingRecordError {
                throw SealedPushError.invalidKeyMaterial
            } catch {
                throw SealedPushError.keychainUnavailable
            }
            payload = try PushPayloadCrypto.openEnvelope(envelope, material: material)
        } catch let error as SealedPushError {
            return .fallback(error)
        } catch {
            return .fallback(.malformedPayload)
        }

        let outcome = NotificationPolicy.outcome(
            eventClass: payload.eventClass,
            projectId: payload.deepLink.projectId,
            settings: settings)

        let link = payload.deepLink
        switch outcome {
        case .suppressed:
            return .rewritten(RewrittenPush(
                title: originalTitle,
                subtitle: "",
                body: originalBody,
                sound: .none,
                interruption: .passive,
                threadIdentifier: suppressedThreadIdentifier,
                relevanceScore: 0,
                suppressed: true,
                eventId: payload.eventId,
                pairingId: envelope.pairingId,
                projectId: link.projectId,
                sessionId: link.sessionId,
                itemId: link.itemId))
        case let .present(withSound):
            let sound: RewrittenPush.Sound
            if !withSound {
                sound = .none
            } else {
                sound = payload.eventClass == .needsInput ? .needsInput : .standard
            }
            return .rewritten(RewrittenPush(
                title: payload.title.isEmpty ? originalTitle : payload.title,
                subtitle: payload.projectName ?? "",
                body: payload.body,
                sound: sound,
                interruption: payload.eventClass == .needsInput ? .timeSensitive : .active,
                // Same thread key as the app's local notifications: one thread
                // per agent session.
                threadIdentifier: link.sessionId,
                relevanceScore: payload.eventClass == .needsInput ? 1 : 0.5,
                suppressed: false,
                eventId: payload.eventId,
                pairingId: envelope.pairingId,
                projectId: link.projectId,
                sessionId: link.sessionId,
                itemId: link.itemId))
        }
    }

    /// The content the extension hands back: `original` rewritten per
    /// `rewrite`, or `original` itself on any failure.
    static func content(
        for original: UNNotificationContent,
        keys: PushKeyMaterialProviding,
        settings: NotificationSettings
    ) -> (content: UNNotificationContent, result: PushRewriteResult) {
        let result = rewrite(
            userInfo: original.userInfo,
            originalTitle: original.title,
            originalBody: original.body,
            keys: keys,
            settings: settings)
        guard case let .rewritten(push) = result,
              let mutable = original.mutableCopy() as? UNMutableNotificationContent else {
            return (original, result)
        }
        apply(push, to: mutable)
        return (mutable, result)
    }

    /// Write a decided rewrite into mutable content.
    static func apply(_ push: RewrittenPush, to content: UNMutableNotificationContent) {
        content.title = push.title
        content.subtitle = push.subtitle
        content.body = push.body
        switch push.sound {
        case .needsInput:
            content.sound = UNNotificationSound(named: UNNotificationSoundName(needsInputSoundName))
        case .standard:
            content.sound = .default
        case .none:
            content.sound = nil
        }
        switch push.interruption {
        case .timeSensitive: content.interruptionLevel = .timeSensitive
        case .active: content.interruptionLevel = .active
        case .passive: content.interruptionLevel = .passive
        }
        content.threadIdentifier = push.threadIdentifier
        content.relevanceScore = push.relevanceScore

        // Keep the relay's keys (`aps`), drop the sealed envelope (no use to
        // the app once opened), and add the deep link the tap handler parses.
        var userInfo = content.userInfo
        userInfo.removeValue(forKey: SealedPushEnvelope.userInfoKey)
        let link = NotificationUserInfo.make(
            eventId: push.eventId,
            projectId: push.projectId,
            sessionId: push.sessionId,
            itemId: push.itemId,
            pairingId: push.pairingId)
        userInfo.merge(link) { _, new in new }
        content.userInfo = userInfo
    }
}
