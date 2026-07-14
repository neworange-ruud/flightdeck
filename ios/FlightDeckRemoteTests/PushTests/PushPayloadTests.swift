//
//  PushPayloadTests.swift
//  FlightDeckRemoteTests
//
//  Parsing a notification's `userInfo` deep-link payload (PRD §5.2), matching
//  the relay's Rust APNs payload shape. Both directions: build → parse.
//

import Testing
import Foundation
@testable import FlightDeckRemote

struct PushPayloadTests {

    @Test func parsesFullPayloadIncludingItemId() throws {
        let userInfo: [AnyHashable: Any] = [
            "event_id": "evt_9",
            "deep_link": ["project_id": "p1", "session_id": "s1", "item_id": "i1"],
        ]
        let payload = try #require(PushPayload(userInfo: userInfo))
        #expect(payload.eventId == "evt_9")
        #expect(payload.deepLink.projectId.rawValue == "p1")
        #expect(payload.deepLink.sessionId.rawValue == "s1")
        #expect(payload.deepLink.itemId?.rawValue == "i1")
    }

    @Test func parsesNullItemId() throws {
        let userInfo: [AnyHashable: Any] = [
            "event_id": "evt_9",
            "deep_link": ["project_id": "p1", "session_id": "s1", "item_id": NSNull()],
        ]
        let payload = try #require(PushPayload(userInfo: userInfo))
        #expect(payload.deepLink.itemId == nil)
    }

    @Test func rejectsMissingDeepLink() {
        #expect(PushPayload(userInfo: ["event_id": "evt_9"]) == nil)
    }

    @Test func rejectsMissingOrEmptyIds() {
        #expect(PushPayload(userInfo: ["deep_link": ["session_id": "s1"]]) == nil)
        #expect(PushPayload(userInfo: ["deep_link": ["project_id": "", "session_id": "s1"]]) == nil)
        #expect(PushPayload(userInfo: ["deep_link": ["project_id": "p1", "session_id": ""]]) == nil)
    }

    @Test func roundTripsThroughBuilder() throws {
        let deepLink = Wire.DeepLink(
            projectId: Wire.ProjectId("p1"),
            sessionId: Wire.SessionId("s1"),
            itemId: Wire.ItemId("i1"))
        let userInfo = PushPayload.userInfo(eventId: "evt_9", deepLink: deepLink)
        let parsed = try #require(PushPayload(userInfo: userInfo))
        #expect(parsed.eventId == "evt_9")
        #expect(parsed.deepLink == deepLink)
    }

    @Test func exposesAppDeepLinkForSharedRouting() {
        let userInfo = PushPayload.userInfo(
            eventId: "e",
            deepLink: Wire.DeepLink(projectId: Wire.ProjectId("p1"), sessionId: Wire.SessionId("s1"), itemId: nil))
        let payload = PushPayload(userInfo: userInfo)!
        #expect(payload.appDeepLink == DeepLink(projectId: "p1", sessionId: "s1"))
    }
}
