//
//  ActivityFeedModel.swift
//  FlightDeckRemote
//
//  Presentation logic for `ActivityFeedView` (PRD §5.7): builds
//  `ActivityCellViewModel`s from `ActivityStore.events` (resolving each
//  event's project id to a display name off `TransportStore.snapshot`, with
//  an honest raw-id fallback when the project isn't known), and translates a
//  tap into navigation.
//
//  Tap-to-navigate deliberately reuses the exact same deep-link path the
//  `flightdeck-remote://` URL-scheme consumer already uses
//  (`AppRouter.pendingDeepLink` → `ProjectsListView`'s
//  `ProjectsDeepLinkTranslator` consumption — see that file) rather than
//  duplicating navigation logic: this model only sets `pendingDeepLink` and
//  switches to the Projects tab. It additionally pre-validates against the
//  same translator so a stale/closed-session tap can surface an inline note
//  instead of silently switching tabs to nowhere.
//

import Foundation
import Observation

@MainActor
@Observable
final class ActivityFeedModel {
    private let activityStore: ActivityStore
    private let transportStore: TransportStore
    private let router: AppRouter

    /// Set by `handleTap` when the tapped event's session is unknown/closed;
    /// the view surfaces it as a transient inline note, then clears it.
    var deadSessionNote: String?

    init(activityStore: ActivityStore, transportStore: TransportStore, router: AppRouter) {
        self.activityStore = activityStore
        self.transportStore = transportStore
        self.router = router
    }

    /// The feed, newest first, mapped for rendering.
    func cellViewModels(nowMs: Int64) -> [ActivityCellViewModel] {
        activityStore.events.map {
            ActivityCellMapper.viewModel(for: $0, projectName: projectName(for: $0), nowMs: nowMs)
        }
    }

    /// Marks the feed viewed (clears the unread badge + advances the
    /// watermark).
    func markViewed() {
        activityStore.markViewed()
    }

    /// Handles a cell tap: navigates via the shared deep-link path when the
    /// session still resolves against the live/cached snapshot; otherwise
    /// surfaces `deadSessionNote` and does not navigate.
    func handleTap(eventId: Wire.EventId) {
        guard let event = activityStore.events.first(where: { $0.eventId == eventId }) else { return }
        let link = DeepLink(
            projectId: event.deepLink.projectId.rawValue,
            sessionId: event.deepLink.sessionId.rawValue
        )
        guard ProjectsDeepLinkTranslator.path(for: link, in: transportStore.snapshot) != nil else {
            deadSessionNote = "That session is no longer active."
            return
        }
        router.pendingDeepLink = link
        router.selectedTab = .projects
    }

    /// Clears the dead-session note (the view auto-dismisses it after a
    /// short delay).
    func dismissDeadSessionNote() {
        deadSessionNote = nil
    }

    private func projectName(for event: Wire.AgentEvent) -> String {
        let projectId = event.deepLink.projectId
        let name = transportStore.snapshot?.projects.first(where: { $0.projectId == projectId })?.name
        return name ?? projectId.rawValue
    }
}
