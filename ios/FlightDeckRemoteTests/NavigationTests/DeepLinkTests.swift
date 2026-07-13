//
//  DeepLinkTests.swift
//  FlightDeckRemoteTests
//
//  Verifies `DeepLink` parses `flightdeck-remote://agent/<project_id>/<session_id>`
//  and rejects every other scheme/host/shape (PRD §5.2/§5.7).
//

import Testing
import Foundation
@testable import FlightDeckRemote

struct DeepLinkTests {

    @Test func parsesValidAgentSessionURL() throws {
        let url = try #require(URL(string: "flightdeck-remote://agent/proj-1/sess-42"))
        #expect(DeepLink(url: url) == DeepLink(projectId: "proj-1", sessionId: "sess-42"))
    }

    @Test func rejectsWrongScheme() throws {
        let url = try #require(URL(string: "https://agent/proj-1/sess-42"))
        #expect(DeepLink(url: url) == nil)
    }

    @Test func rejectsWrongHost() throws {
        let url = try #require(URL(string: "flightdeck-remote://session/proj-1/sess-42"))
        #expect(DeepLink(url: url) == nil)
    }

    @Test func rejectsMissingHost() throws {
        let url = try #require(URL(string: "flightdeck-remote:///proj-1/sess-42"))
        #expect(DeepLink(url: url) == nil)
    }

    @Test func rejectsMissingSessionId() throws {
        let url = try #require(URL(string: "flightdeck-remote://agent/proj-1"))
        #expect(DeepLink(url: url) == nil)
    }

    @Test func rejectsExtraPathComponents() throws {
        let url = try #require(URL(string: "flightdeck-remote://agent/proj-1/sess-42/extra"))
        #expect(DeepLink(url: url) == nil)
    }

    @Test func rejectsNoPathAtAll() throws {
        let url = try #require(URL(string: "flightdeck-remote://agent"))
        #expect(DeepLink(url: url) == nil)
    }
}
