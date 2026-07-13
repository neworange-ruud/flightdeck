//
//  ActivityStoreTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the Activity tab's unread badge (PRD §5.7): defaults to visible
//  (1 unread) and clears once the tab is viewed.
//

import Testing
@testable import FlightDeckRemote

struct ActivityStoreTests {

    @Test func defaultsToOneUnreadItemSoTheBadgeIsVisible() {
        let store = ActivityStore()
        #expect(store.unreadCount == 1)
    }

    @Test func markViewedClearsUnreadCount() {
        let store = ActivityStore(unreadCount: 3)
        store.markViewed()
        #expect(store.unreadCount == 0)
    }
}
