//
//  ProjectsNavModel.swift
//  FlightDeckRemote
//
//  The Projects tab's `NavigationStack` path (PRD §3 hierarchy: Projects →
//  Agent sessions → Terminals). This task only exposes the typed path and a
//  minimal `navigationDestination` wiring (see `MainTabView`) — later
//  feature tasks push onto `path` to drive real navigation (e.g. from a
//  project card into its sessions list, or from a session into its chat),
//  and the deep-link seam (`AppRouter.pendingDeepLink`) will eventually push
//  straight to `.chat(projectId:sessionId:)`.
//

import Observation

/// A destination the Projects tab's `NavigationStack` can push.
enum ProjectsRoute: Hashable {
    /// A project's agent-sessions list.
    case sessions(projectId: String)
    /// A single agent session's chat surface.
    case chat(projectId: String, sessionId: String)
}

@Observable
final class ProjectsNavModel {
    var path: [ProjectsRoute] = []
}
