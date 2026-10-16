//
//  ForegroundPresentationPolicyTests.swift
//  FlightDeckRemoteTests
//
//  One agent event, one banner (remote-control-bza0): the same event can reach
//  the phone as the desktop's push and as a live local notification, both with
//  the same `event_id`. Covers the pure policy, the App Group log the extension
//  writes, and `PushCoordinator`'s bookkeeping across both orders.
//

import Foundation
import Testing
import UserNotifications
@testable import FlightDeckRemote

struct ForegroundPresentationPolicyTests {
    typealias P = ForegroundPresentationPolicy

    private func decide(
        _ source: P.Source,
        eventId: String? = "evt_1",
        sessionId: String? = "sess_1",
        isPassive: Bool = false,
        byPush: Bool = false,
        locally: Bool = false,
        visible: String? = nil
    ) -> P.Presentation {
        P.decide(source: source, eventId: eventId, sessionId: sessionId, isPassive: isPassive,
                 presentedByPush: byPush, presentedLocally: locally, visibleSessionId: visible)
    }

    @Test func firstPresentationBannersWhicheverPathWins() {
        #expect(decide(.remote) == .full)
        #expect(decide(.local) == .full)
        // The extension always logs its own push before willPresent runs, so a
        // remote push must not count its own log entry against itself.
        #expect(decide(.remote, byPush: true) == .full)
    }

    @Test func secondPresentationOfAnEventIsListOnly() {
        #expect(decide(.remote, locally: true) == .listOnly)  // local first, then push
        #expect(decide(.local, byPush: true) == .listOnly)    // push first (even backgrounded), then local
        #expect(decide(.local, locally: true) == .listOnly)   // repeated local
    }

    @Test func theVisibleSessionNeverBanners() {
        #expect(decide(.remote, visible: "sess_1") == .listOnly)
        #expect(decide(.local, visible: "sess_1") == .listOnly)
        #expect(decide(.local, visible: "sess_other") == .full)
    }

    @Test func passiveDeliveriesNeverBanner() {
        #expect(decide(.remote, isPassive: true) == .listOnly)
    }

    @Test func notificationsWithoutAnEventIdAreLeftAlone() {
        // e.g. the generic fallback of a push the extension could not open.
        #expect(decide(.remote, eventId: nil, sessionId: nil, locally: true) == .full)
        #expect(decide(.local, eventId: "", byPush: true) == .full)
    }

    @Test func presentationOptions() {
        #expect(P.Presentation.full.options == [.banner, .sound, .list])
        #expect(P.Presentation.listOnly.options == [.list])
    }
}

struct PresentedPushLogTests {
    private func freshDefaults() -> UserDefaults {
        let name = "test.push-log.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }

    @Test func recordsAndIsBounded() {
        let log = PresentedPushLog(defaults: freshDefaults(), capacity: 3)
        #expect(!log.contains("a"))
        ["a", "b", "c", "d"].forEach(log.record)
        #expect(log.eventIds == ["b", "c", "d"])
        #expect(!log.contains("a"))
        // Re-recording refreshes, never duplicates.
        log.record("b")
        #expect(log.eventIds == ["c", "d", "b"])
        log.record("")
        #expect(log.eventIds.count == 3)
    }
}

@MainActor
struct PushCoordinatorPresentationTests {
    private func freshLog() -> PresentedPushLog {
        let name = "test.push-coord.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return PresentedPushLog(defaults: defaults)
    }

    private func userInfo(_ eventId: String, session: String = "sess_1") -> [AnyHashable: Any] {
        PushPayload.userInfo(
            eventId: eventId,
            deepLink: Wire.DeepLink(projectId: Wire.ProjectId("proj_1"), sessionId: Wire.SessionId(session), itemId: nil),
            pairingId: "pair_a")
    }

    @Test func localThenRemoteBannersOnce() {
        let coordinator = PushCoordinator(pushLog: freshLog())
        #expect(coordinator.presentation(source: .local, userInfo: userInfo("evt_1"), isPassive: false) == .full)
        #expect(coordinator.presentation(source: .remote, userInfo: userInfo("evt_1"), isPassive: false) == .listOnly)
        // Another event is unaffected.
        #expect(coordinator.presentation(source: .remote, userInfo: userInfo("evt_2"), isPassive: false) == .full)
    }

    @Test func pushThenLocalBannersOnce() {
        let log = freshLog()
        let coordinator = PushCoordinator(pushLog: log)
        // The extension rewrote the push (possibly while backgrounded)…
        log.record("evt_1")
        #expect(coordinator.presentation(source: .remote, userInfo: userInfo("evt_1"), isPassive: false) == .full)
        // …so the live transport's local notification for it stays quiet.
        #expect(coordinator.presentation(source: .local, userInfo: userInfo("evt_1"), isPassive: false) == .listOnly)
    }

    @Test func visibleSessionTracksTheChatScreen() {
        let coordinator = PushCoordinator(pushLog: freshLog())
        coordinator.visibleSessionId = "sess_1"
        #expect(coordinator.presentation(source: .remote, userInfo: userInfo("evt_1"), isPassive: false) == .listOnly)
        // A stale disappear for another session must not clear it.
        coordinator.chatDidDisappear(sessionId: "sess_other")
        #expect(coordinator.visibleSessionId == "sess_1")
        coordinator.chatDidDisappear(sessionId: "sess_1")
        #expect(coordinator.presentation(source: .remote, userInfo: userInfo("evt_2"), isPassive: false) == .full)
    }

    @Test func localNotificationsUseTheEventIdAsRequestIdentifier() {
        // The push's request identifier is its apns-collapse-id (= event_id);
        // the local one must match so Notification Center keeps one entry.
        let recorder = RecordingNotificationScheduler()
        let scheduler = NotificationScheduler(scheduler: recorder)
        scheduler.ingest([PushFixtures.needsInput], settings: NotificationSettings(), pairingId: "pair_a")
        #expect(recorder.identifiers == [PushFixtures.needsInput.eventId.rawValue])
    }
}
