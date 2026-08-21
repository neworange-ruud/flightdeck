//
//  BadgeControllerTests.swift
//  FlightDeckRemoteTests
//
//  Verifies BadgeController's badge-count clamping rule (PRD §5.1: the
//  home-screen badge is the count of agents waiting for input, which can
//  never go negative). Deliberately does not exercise
//  UNUserNotificationCenter itself — that needs notification authorization
//  and a running app host, neither of which a unit test bundle provides.
//

import Testing
@testable import FlightDeckRemote

struct BadgeControllerTests {

    @Test func clampsNegativeCountsToZero() {
        #expect(BadgeController.clampedBadgeCount(-1) == 0)
        #expect(BadgeController.clampedBadgeCount(-42) == 0)
        #expect(BadgeController.clampedBadgeCount(Int.min) == 0)
    }

    @Test func passesThroughNonNegativeCounts() {
        #expect(BadgeController.clampedBadgeCount(0) == 0)
        #expect(BadgeController.clampedBadgeCount(1) == 1)
        #expect(BadgeController.clampedBadgeCount(3) == 3)
        #expect(BadgeController.clampedBadgeCount(Int.max) == Int.max)
    }
}
