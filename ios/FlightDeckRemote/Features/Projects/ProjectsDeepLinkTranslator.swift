//
//  ProjectsDeepLinkTranslator.swift
//  FlightDeckRemote
//
//  Translates a parsed `DeepLink` (Navigation/DeepLink.swift) into a
//  `ProjectsNavModel` path push (PRD §5.2/§5.7: a notification tap lands
//  straight on the agent). `AppRouter.handleDeepLink` only proves the URL
//  parses and switches to the Projects tab — turning it into an actual
//  `[.sessions, .chat]` push, validated against the live snapshot, is this
//  screen's job.
//
//  Unknown ids (a stale notification for a since-closed session, or a link
//  that arrived before the first snapshot) translate to `nil`: the caller
//  clears `pendingDeepLink` without navigating anywhere — never crashes,
//  never pushes a route the sessions list can't render.
//
//  Pure and unit-tested.
//

import Foundation

enum ProjectsDeepLinkTranslator {
    /// Build the nav path for `link`, or `nil` if the project/session isn't
    /// in `snapshot` (including when `snapshot` itself is `nil` — no data to
    /// validate against yet).
    static func path(for link: DeepLink, in snapshot: Wire.StateSnapshot?) -> [ProjectsRoute]? {
        guard let snapshot else { return nil }
        guard let project = snapshot.projects.first(where: { $0.projectId.rawValue == link.projectId }) else {
            return nil
        }
        guard project.sessions.contains(where: { $0.sessionId.rawValue == link.sessionId }) else {
            return nil
        }
        // The Projects tab stays single-store/transitional (remote-control-b8d.12
        // scope is the Feed tab's per-instance navigation) — `link.pairingId`
        // (carried by a per-machine push payload, remote-control-b8d.10) isn't
        // wired to a store resolution here yet, so both routes pin `nil`,
        // matching every other Projects-tab push.
        return [
            .sessions(projectId: link.projectId, pairingId: nil),
            .chat(projectId: link.projectId, sessionId: link.sessionId, pairingId: nil),
        ]
    }
}
