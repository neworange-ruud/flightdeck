//
//  NotificationContentMapperTests.swift
//  FlightDeckRemoteTests
//
//  The typed `Wire.AgentEvent → notification content` mapping (PRD §5.2 copy),
//  mirroring the relay's Rust `apns::notification_content`, plus the
//  settings-gated `UNNotificationContent` build.
//

import Testing
import UserNotifications
@testable import FlightDeckRemote

struct NotificationContentMapperTests {

    @Test func needsInputIsUrgentWithDistinctSoundPreference() {
        let model = NotificationContentMapper.displayModel(for: PushFixtures.needsInput)
        #expect(model.title == "fix-login needs your input")
        #expect(model.body == "Allow `rm -rf dist/`?")
        #expect(model.sound == .needsInput)
        #expect(model.urgency == .timeSensitive)
    }

    @Test func finishedBodyMatchesPrdCopy() {
        // "18 files changed · ready to push · <summary>" (mirrors Rust).
        #expect(NotificationContentMapper.finishedBody(summary: "SpecAssistant", filesChanged: 18, readyToPush: true)
                == "18 files changed · ready to push · SpecAssistant")
        #expect(NotificationContentMapper.finishedBody(summary: "", filesChanged: 1, readyToPush: false)
                == "1 file changed")
    }

    @Test func errorMapsToActiveWithMessageBody() {
        let model = NotificationContentMapper.displayModel(for: PushFixtures.errored)
        #expect(model.body == "npm test failed")
        #expect(model.urgency == .active)
    }

    @Test func contentIsNilWhenSuppressed() {
        let settings = NotificationSettings(agentNeedsInput: false)
        #expect(NotificationContentMapper.content(for: PushFixtures.needsInput, settings: settings) == nil)
    }

    @Test func contentCarriesTitleBodyInterruptionAndDeepLink() throws {
        let settings = NotificationSettings(agentNeedsInput: true)
        let content = try #require(NotificationContentMapper.content(for: PushFixtures.needsInput, settings: settings))
        #expect(content.title == "fix-login needs your input")
        #expect(content.body == "Allow `rm -rf dist/`?")
        #expect(content.interruptionLevel == .timeSensitive)
        #expect(content.sound != nil) // distinct needs-input sound

        // The deep link parses straight back out of userInfo (push-deeplink).
        let payload = try #require(PushPayload(userInfo: content.userInfo))
        #expect(payload.eventId == "evt_needs")
        #expect(payload.deepLink.projectId.rawValue == "proj_1")
        #expect(payload.deepLink.sessionId.rawValue == "sess_1")
    }

    @Test func chimeOffProducesSilentFinishedContent() throws {
        let settings = NotificationSettings(agentFinished: true, completionChime: false)
        let content = try #require(NotificationContentMapper.content(for: PushFixtures.finished, settings: settings))
        #expect(content.sound == nil) // present, but silent
    }

    // MARK: - Multi-pairing pairingId stamping (remote-control-b8d.10)

    @Test func contentStampsPairingIdSoATapDeepLinksToTheMachine() throws {
        let settings = NotificationSettings(agentNeedsInput: true)
        let content = try #require(NotificationContentMapper.content(
            for: PushFixtures.needsInput, settings: settings, pairingId: "pair_ruud_mbp"))
        let payload = try #require(PushPayload(userInfo: content.userInfo))
        #expect(payload.pairingId == "pair_ruud_mbp")
        #expect(payload.appDeepLink.pairingId == "pair_ruud_mbp")
    }

    @Test func contentOmitsPairingIdWhenUnknown() throws {
        let settings = NotificationSettings(agentNeedsInput: true)
        let content = try #require(NotificationContentMapper.content(
            for: PushFixtures.needsInput, settings: settings))
        #expect(content.userInfo["pairing_id"] == nil)
        let payload = try #require(PushPayload(userInfo: content.userInfo))
        #expect(payload.pairingId == nil)
    }
}
