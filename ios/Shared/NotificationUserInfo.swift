//
//  NotificationUserInfo.swift
//  FlightDeckRemote + FlightDeckNotificationService
//
//  The one definition of the deep-link `userInfo` a FlightDeck notification
//  carries, so a tap routes identically whether the notification was posted
//  locally by the app (`NotificationContentMapper`) or rewritten from a sealed
//  push by the Notification Service Extension (remote-control-bza0). The app's
//  `PushPayload.init?(userInfo:)` is the parser.
//
//      { "event_id": "...",
//        "deep_link": { "project_id": "...", "session_id": "...",
//                       "item_id": "..."|NSNull },
//        "pairing_id": "..." }            // omitted when unknown
//

import Foundation

enum NotificationUserInfo {
    static let eventIdKey = "event_id"
    static let deepLinkKey = "deep_link"
    static let projectIdKey = "project_id"
    static let sessionIdKey = "session_id"
    static let itemIdKey = "item_id"
    static let pairingIdKey = "pairing_id"

    /// Build the deep-link `userInfo`. `itemId` absent is stored as `NSNull`,
    /// a faithful mirror of the JSON `null`; `pairingId` is omitted when `nil`.
    static func make(
        eventId: String,
        projectId: String,
        sessionId: String,
        itemId: String?,
        pairingId: String?
    ) -> [AnyHashable: Any] {
        var info: [AnyHashable: Any] = [
            eventIdKey: eventId,
            deepLinkKey: [
                projectIdKey: projectId,
                sessionIdKey: sessionId,
                itemIdKey: itemId ?? NSNull(),
            ] as [String: Any],
        ]
        if let pairingId { info[pairingIdKey] = pairingId }
        return info
    }
}
