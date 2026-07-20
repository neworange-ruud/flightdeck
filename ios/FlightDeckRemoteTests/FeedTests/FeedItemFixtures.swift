//
//  FeedItemFixtures.swift
//  FlightDeckRemoteTests
//
//  Shared builders for `FeedItem`s (and their Wire parts) used across the Feed
//  unit suites (`FeedStoreTests`, `FeedUnreadStoreTests`, `FeedRowPresentationTests`),
//  so each test can spell out just the axis it exercises (attention, unread,
//  online) without repeating the full `Wire.ProjectState` scaffolding.
//

import Foundation
@testable import FlightDeckRemote

enum FeedItemFixtures {
    static func rollup(dot: Wire.RollupDot = .idle) -> Wire.StatusRollup {
        Wire.StatusRollup(dot: dot, summary: "idle · 1 agent", working: 0, idle: 1,
                          needsInput: dot == .needsInput ? 1 : 0, manual: 0, agentCount: 1)
    }

    static func git() -> Wire.GitIndicators {
        Wire.GitIndicators(branch: "main", added: 0, modified: 0, removed: 0,
                           ahead: 0, behind: 0, drift: 0, hasUpstream: true)
    }

    static func project(_ id: String, dot: Wire.RollupDot = .idle) -> Wire.ProjectState {
        let session = Wire.SessionState(
            sessionId: Wire.SessionId("s_\(id)"), projectId: Wire.ProjectId(id),
            name: "fix-\(id)", agentType: .claudeCode, status: .idle, git: git(),
            runningTimeSecs: 0, pendingQuestion: nil)
        return Wire.ProjectState(projectId: Wire.ProjectId(id), name: "Project \(id)",
                                 rollup: rollup(dot: dot), sessions: [session])
    }

    static func event(project: String, session: String = "sess", atMs: Int64,
                      kind: Wire.EventKind) -> Wire.AgentEvent {
        Wire.AgentEvent(
            eventId: Wire.EventId("evt_\(project)_\(atMs)"),
            kind: kind,
            deepLink: Wire.DeepLink(projectId: Wire.ProjectId(project), sessionId: Wire.SessionId(session), itemId: nil),
            occurredAtMs: atMs,
            title: "\(project) event")
    }

    static func item(
        pairingId: String,
        projectId: String,
        dot: Wire.RollupDot = .idle,
        isOnline: Bool = true,
        activityMs: Int64 = 0,
        latestEvent: Wire.AgentEvent? = nil
    ) -> FeedItem {
        FeedItem(
            pairingId: pairingId,
            displayName: "Machine \(pairingId)",
            isOnline: isOnline,
            project: project(projectId, dot: dot),
            activityMs: activityMs,
            latestEvent: latestEvent)
    }
}
