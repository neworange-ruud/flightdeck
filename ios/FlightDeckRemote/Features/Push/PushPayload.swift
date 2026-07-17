//
//  PushPayload.swift
//  FlightDeckRemote
//
//  Parses the deep-link payload carried by a notification's `userInfo` (PRD
//  §5.2: notifications deep-link straight to the agent). The same shape is
//  produced by the relay's Rust APNs alert payload and by this app's own local
//  notifications (`NotificationContentMapper`), so one parser handles a tapped
//  remote push and a tapped local notification alike.
//
//  Wire shape (top-level `userInfo`, matching `remote/relay/src/apns.rs`):
//
//      { "event_id": "...",
//        "deep_link": { "project_id": "...", "session_id": "...",
//                       "item_id": "..."|null } }
//
//  Pure and host-free (`[AnyHashable: Any]` → typed), so it is fully unit
//  testable.
//

import Foundation

/// A parsed notification deep-link payload.
struct PushPayload: Equatable {
    /// The event id (dedup key, spec §6.4).
    let eventId: String
    /// The wire deep link (project + session + optional item).
    let deepLink: Wire.DeepLink

    /// Parse an APNs / local-notification `userInfo` dictionary. Returns `nil`
    /// if the required `deep_link` project/session ids are missing — a
    /// malformed payload must never crash or navigate somewhere wrong.
    init?(userInfo: [AnyHashable: Any]) {
        guard let link = userInfo["deep_link"] as? [AnyHashable: Any],
              let projectId = link["project_id"] as? String, !projectId.isEmpty,
              let sessionId = link["session_id"] as? String, !sessionId.isEmpty
        else { return nil }

        let itemIdRaw = link["item_id"] as? String
        self.eventId = (userInfo["event_id"] as? String) ?? ""
        self.deepLink = Wire.DeepLink(
            projectId: Wire.ProjectId(projectId),
            sessionId: Wire.SessionId(sessionId),
            itemId: itemIdRaw.map(Wire.ItemId.init))
    }

    /// The app-layer `DeepLink` (Navigation) this payload routes to, reusing
    /// the exact type the `flightdeck-remote://` URL scheme produces so push
    /// taps and URL opens share one navigation path.
    var appDeepLink: DeepLink {
        DeepLink(projectId: deepLink.projectId.rawValue, sessionId: deepLink.sessionId.rawValue)
    }

    /// Build the `userInfo` dictionary a local notification carries, matching
    /// the relay's APNs payload shape so `init?(userInfo:)` round-trips it.
    static func userInfo(eventId: String, deepLink: Wire.DeepLink) -> [AnyHashable: Any] {
        [
            "event_id": eventId,
            "deep_link": [
                "project_id": deepLink.projectId.rawValue,
                "session_id": deepLink.sessionId.rawValue,
                // `item_id` is optional; use `NSNull` for the absent case so the
                // dictionary is a faithful mirror of the JSON `null`.
                "item_id": deepLink.itemId?.rawValue ?? NSNull(),
            ],
        ]
    }
}
