//
//  PresentedPushLog.swift
//  FlightDeckRemote + FlightDeckNotificationService
//
//  The event ids the Notification Service Extension has rewritten into a real
//  notification, kept in the App Group so the app can see them across process
//  lifetimes (remote-control-bza0).
//
//  Why: the desktop guarantees a push's `event_id` equals the feed
//  `AgentEvent`'s `event_id`, and the app still posts a local notification for
//  live events while foregrounded. If a push was already shown — even while the
//  app was backgrounded — the local notification the transport produces for the
//  same event on the next foreground must not banner again
//  (`ForegroundPresentationPolicy`).
//
//  Bounded (newest `capacity` ids), so it never grows without limit.
//

import Foundation

struct PresentedPushLog {
    static let defaultsKey = "agency.neworange.flightdeck.remote.push.presentedEventIds"
    static let defaultCapacity = 200

    private let defaults: UserDefaults
    private let capacity: Int

    init(defaults: UserDefaults = SharedContainer.defaults, capacity: Int = PresentedPushLog.defaultCapacity) {
        self.defaults = defaults
        self.capacity = capacity
    }

    /// Ids, oldest first.
    var eventIds: [String] {
        defaults.stringArray(forKey: Self.defaultsKey) ?? []
    }

    func contains(_ eventId: String) -> Bool {
        eventIds.contains(eventId)
    }

    /// Record that a push for `eventId` was presented. Re-recording moves it to
    /// the newest position; the oldest ids fall off past `capacity`.
    func record(_ eventId: String) {
        guard !eventId.isEmpty else { return }
        var ids = eventIds.filter { $0 != eventId }
        ids.append(eventId)
        if ids.count > capacity { ids.removeFirst(ids.count - capacity) }
        defaults.set(ids, forKey: Self.defaultsKey)
    }
}
