//
//  ActivityStore.swift
//  FlightDeckRemote
//
//  Stub backing store for the Activity tab's unread-dot badge (PRD §5.7:
//  "Activity carries an unread dot... clears on view"). `unreadCount`
//  defaults to 1 so the badge is visible out of the box; the real Activity
//  feature task will drive this from the actual status-event feed instead.
//
//  `markViewed()` is wired from `MainTabView` when the Activity tab becomes
//  selected.
//

import Observation

@Observable
final class ActivityStore {
    var unreadCount: Int

    init(unreadCount: Int = 1) {
        self.unreadCount = unreadCount
    }

    /// Clears the unread badge. Called once the Activity tab is actually
    /// viewed, not merely when new events arrive.
    func markViewed() {
        unreadCount = 0
    }
}
