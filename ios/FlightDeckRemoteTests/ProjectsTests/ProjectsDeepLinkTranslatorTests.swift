//
//  ProjectsDeepLinkTranslatorTests.swift
//  FlightDeckRemoteTests
//
//  Covers `ProjectsDeepLinkTranslator.path(for:in:)` (PRD §5.2/§5.7): a known
//  project/session translates to a `[.sessions, .chat]` push; a `nil`
//  snapshot, an unknown project, or an unknown session all translate to
//  `nil` (clear `pendingDeepLink` without navigating) rather than crashing.
//

import Testing
@testable import FlightDeckRemote

struct ProjectsDeepLinkTranslatorTests {

    private func snapshot() -> Wire.StateSnapshot {
        let session = Wire.SessionState(
            sessionId: Wire.SessionId("sess-42"), projectId: Wire.ProjectId("proj-1"),
            name: "fix-login", agentType: .claudeCode, status: .idle,
            git: Wire.GitIndicators(
                branch: "main", added: 0, modified: 0, removed: 0,
                ahead: 0, behind: 0, drift: 0, hasUpstream: true),
            runningTimeSecs: 0, pendingQuestion: nil)
        let project = Wire.ProjectState(
            projectId: Wire.ProjectId("proj-1"), name: "flightdeck",
            rollup: Wire.StatusRollup(
                dot: .idle, summary: "idle · 1 agent", working: 0, idle: 1,
                needsInput: 0, manual: 0, agentCount: 1),
            sessions: [session])
        return Wire.StateSnapshot(serverTimeMs: 1, projects: [project])
    }

    @Test func nilSnapshotTranslatesToNil() {
        let link = DeepLink(projectId: "proj-1", sessionId: "sess-42")
        #expect(ProjectsDeepLinkTranslator.path(for: link, in: nil) == nil)
    }

    @Test func unknownProjectTranslatesToNil() {
        let link = DeepLink(projectId: "nope", sessionId: "sess-42")
        #expect(ProjectsDeepLinkTranslator.path(for: link, in: snapshot()) == nil)
    }

    @Test func unknownSessionTranslatesToNil() {
        let link = DeepLink(projectId: "proj-1", sessionId: "nope")
        #expect(ProjectsDeepLinkTranslator.path(for: link, in: snapshot()) == nil)
    }

    @Test func knownLinkTranslatesToSessionsThenChatPush() {
        let link = DeepLink(projectId: "proj-1", sessionId: "sess-42")
        #expect(ProjectsDeepLinkTranslator.path(for: link, in: snapshot()) == [
            .sessions(projectId: "proj-1", pairingId: nil),
            .chat(projectId: "proj-1", sessionId: "sess-42", pairingId: nil),
        ])
    }
}
