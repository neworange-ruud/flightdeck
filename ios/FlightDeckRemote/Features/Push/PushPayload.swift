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
    /// Which paired machine this notification came from (multi-pairing push,
    /// remote-control-b8d.10). Top-level `pairing_id` in `userInfo` — stamped
    /// by this app's own per-instance local notifications so a tap deep-links
    /// to the right `PairedInstance`. `nil` for notifications that predate the
    /// field (e.g. a relay-built alert that doesn't carry it) — routing then
    /// falls back to the machine-agnostic path.
    let pairingId: String?

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
        // An empty `pairing_id` is treated as absent (nil) rather than a real,
        // never-matching id — so a stray empty string can't force a resolve miss.
        let pairingIdRaw = userInfo["pairing_id"] as? String
        self.pairingId = (pairingIdRaw?.isEmpty == false) ? pairingIdRaw : nil
        self.deepLink = Wire.DeepLink(
            projectId: Wire.ProjectId(projectId),
            sessionId: Wire.SessionId(sessionId),
            itemId: itemIdRaw.map(Wire.ItemId.init))
    }

    /// The app-layer `DeepLink` (Navigation) this payload routes to, reusing
    /// the exact type the `flightdeck-remote://` URL scheme produces so push
    /// taps and URL opens share one navigation path. Carries `pairingId` so
    /// downstream navigation can bind to the right per-instance store (b8d.12).
    var appDeepLink: DeepLink {
        DeepLink(
            projectId: deepLink.projectId.rawValue,
            sessionId: deepLink.sessionId.rawValue,
            pairingId: pairingId)
    }

    /// Build the `userInfo` dictionary a local notification carries, matching
    /// the relay's APNs payload shape so `init?(userInfo:)` round-trips it.
    /// `pairingId`, when known (the on-device per-instance path), is stamped at
    /// the top level so a tap deep-links to the originating machine
    /// (remote-control-b8d.10); omitted when `nil` so the payload stays a
    /// faithful mirror of a relay push that doesn't carry it.
    static func userInfo(
        eventId: String,
        deepLink: Wire.DeepLink,
        pairingId: String? = nil
    ) -> [AnyHashable: Any] {
        var info: [AnyHashable: Any] = [
            "event_id": eventId,
            "deep_link": [
                "project_id": deepLink.projectId.rawValue,
                "session_id": deepLink.sessionId.rawValue,
                // `item_id` is optional; use `NSNull` for the absent case so the
                // dictionary is a faithful mirror of the JSON `null`.
                "item_id": deepLink.itemId?.rawValue ?? NSNull(),
            ],
        ]
        if let pairingId { info["pairing_id"] = pairingId }
        return info
    }
}
