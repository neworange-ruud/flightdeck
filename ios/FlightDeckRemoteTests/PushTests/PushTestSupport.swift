//
//  PushTestSupport.swift
//  FlightDeckRemoteTests
//
//  Shared builders for the Push tests: typed `Wire.AgentEvent`s and a recording
//  notification scheduler double.
//

import Foundation
import UserNotifications
@testable import FlightDeckRemote

enum PushFixtures {
    static func event(
        id: String = "evt_1",
        kind: Wire.EventKind,
        projectId: String = "proj_1",
        sessionId: String = "sess_1",
        itemId: String? = nil,
        title: String = "title"
    ) -> Wire.AgentEvent {
        Wire.AgentEvent(
            eventId: Wire.EventId(id),
            kind: kind,
            deepLink: Wire.DeepLink(
                projectId: Wire.ProjectId(projectId),
                sessionId: Wire.SessionId(sessionId),
                itemId: itemId.map(Wire.ItemId.init)),
            occurredAtMs: 1_752_412_802_000,
            title: title)
    }

    static var needsInput: Wire.AgentEvent {
        event(id: "evt_needs", kind: .needsInput(preview: "Allow `rm -rf dist/`?"),
              title: "fix-login needs your input")
    }

    static var finished: Wire.AgentEvent {
        event(id: "evt_done", kind: .finished(summary: "SpecAssistant", filesChanged: 18, readyToPush: true),
              title: "add-tests finished its turn")
    }

    static var errored: Wire.AgentEvent {
        event(id: "evt_err", kind: .error(message: "npm test failed"), title: "hero-copy hit an error")
    }
}

/// Records every scheduled request so scheduler tests can assert on them.
final class RecordingNotificationScheduler: UserNotificationScheduling {
    private(set) var requests: [UNNotificationRequest] = []
    func add(_ request: UNNotificationRequest) { requests.append(request) }
    var identifiers: [String] { requests.map(\.identifier) }
}

/// In-memory `NotificationSettingsStoring` double.
final class InMemoryNotificationSettingsStore: NotificationSettingsStoring {
    var settings: NotificationSettings
    private(set) var saveCount = 0
    init(settings: NotificationSettings = NotificationSettings()) { self.settings = settings }
    func load() -> NotificationSettings { settings }
    func save(_ settings: NotificationSettings) {
        self.settings = settings
        saveCount += 1
    }
}
