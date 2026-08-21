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
//  The SAME route type is reused by the Feed tab's `NavigationStack`
//  (`MainTabView`'s `.feed` case, remote-control-b8d.8/.12) rather than
//  inventing a parallel one, since both tabs push onto `SessionsListView`/
//  `AgentChatView`. Each case therefore carries its own `pairingId` —
//  `nil` for the Projects tab (single-store, transitional: it always binds
//  to `MainTabView.transportStore`) and the tapped row's machine for the
//  Feed tab — so a route value captured at push time is self-contained:
//  the destination it resolves to (`MainTabView.tabContent`'s
//  `.navigationDestination`) reads the pairingId straight off the route
//  rather than off any separately-mutable "which machine is active" state,
//  and so cannot be switched out from under an already-pushed screen by a
//  LATER tap on a different row (remote-control-b8d.12).
//

import Observation

/// A destination the Projects tab's `NavigationStack` can push.
enum ProjectsRoute: Hashable {
    /// A project's agent-sessions list. `pairingId` pins which machine's
    /// `TransportStore` the destination binds to (`nil` on the Projects tab).
    case sessions(projectId: String, pairingId: String?)
    /// A single agent session's chat surface. `pairingId` pins which
    /// machine's `TransportStore` the destination binds to (`nil` on the
    /// Projects tab).
    case chat(projectId: String, sessionId: String, pairingId: String?)
}

@Observable
final class ProjectsNavModel {
    var path: [ProjectsRoute] = []
}
