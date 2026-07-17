//
//  DeepLink.swift
//  FlightDeckRemote
//
//  Parses the `flightdeck-remote://` URL scheme (registered in project.yml)
//  per PRD §5.2/§5.7: notifications ("needs input" / "finished") deep-link
//  straight to the agent. v1 supports exactly one shape:
//
//      flightdeck-remote://agent/<project_id>/<session_id>
//
//  This task only proves parsing + landing on the Projects tab
//  (`AppRouter.handleDeepLink`, `AppRouter.pendingDeepLink`). Actually
//  navigating `ProjectsNavModel.path` into the session's chat is a later
//  task's job.
//

import Foundation

/// A successfully parsed deep link. Any other scheme/host/shape fails to
/// parse (`init?(url:)` returns `nil`) and is silently ignored by the
/// router — a malformed or unknown link must never crash or navigate
/// somewhere wrong.
struct DeepLink: Equatable {
    let projectId: String
    let sessionId: String

    init(projectId: String, sessionId: String) {
        self.projectId = projectId
        self.sessionId = sessionId
    }

    /// Parses `flightdeck-remote://agent/<project_id>/<session_id>`.
    /// Returns `nil` for any other scheme, host, or path shape (wrong
    /// number of path components, or an empty component).
    init?(url: URL) {
        guard url.scheme?.lowercased() == "flightdeck-remote" else { return nil }
        guard url.host == "agent" else { return nil }

        let components = url.pathComponents.filter { $0 != "/" }
        guard components.count == 2 else { return nil }

        let projectId = components[0]
        let sessionId = components[1]
        guard !projectId.isEmpty, !sessionId.isEmpty else { return nil }

        self.init(projectId: projectId, sessionId: sessionId)
    }
}
