//
//  FeedRefreshPlan.swift
//  FlightDeckRemote
//
//  Pure pull-to-refresh decision for the unified feed (remote-control-b8d.8):
//  for each paired machine, whether refreshing should ask the desktop for a
//  fresh snapshot (a live, `.connected` client) or attempt a reconnect
//  instead (anything else — `.disconnected`/`.connecting`/`.authenticating`).
//  This lets pull-to-refresh double as a manual retry for every currently
//  offline machine, not only the already-live ones, without the user having
//  to find each offline row's own retry button. Kept free of
//  `TransportCoordinator`/`TransportStore` so the decision is unit-testable
//  in isolation (mirrors `FeedStore.isOnline`'s shape for the same
//  `RemoteLinkState` — kept separate since this is a UI-facing action, not
//  the feed's own online/offline determination).
//

import Foundation

/// What pull-to-refresh should do for one paired machine.
enum FeedRefreshAction: Equatable {
    /// The machine's client is live — ask it for a fresh snapshot.
    case resync
    /// The machine's client isn't live — (re)start it instead.
    case reconnect
}

enum FeedRefreshPlan {
    /// The refresh action for a single machine's current link state.
    static func action(for linkState: RemoteLinkState) -> FeedRefreshAction {
        if case .connected = linkState { return .resync }
        return .reconnect
    }
}
