//
//  BackoffTests.swift
//  FlightDeckRemoteTests
//
//  The reconnect backoff schedule (REMOTE_PROTOCOL §5.3): 1s floor, 60s cap,
//  exponential doubling, up to +25% jitter — must match the desktop.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@Suite struct BackoffTests {

    private func ms(_ d: Duration) -> Int64 {
        let c = d.components
        return c.seconds * 1000 + Int64(Double(c.attoseconds) / 1e15)
    }

    @Test func floorWithoutJitter() {
        #expect(ms(Backoff.delay(attempt: 0, jitterUnit: 0)) == 1_000)
    }

    @Test func maxJitterAddsUpToTwentyFivePercent() {
        #expect(ms(Backoff.delay(attempt: 0, jitterUnit: 1)) == 1_250)
    }

    @Test func doublesEachAttempt() {
        #expect(ms(Backoff.delay(attempt: 1, jitterUnit: 0)) == 2_000)
        #expect(ms(Backoff.delay(attempt: 2, jitterUnit: 0)) == 4_000)
        #expect(ms(Backoff.delay(attempt: 3, jitterUnit: 0)) == 8_000)
    }

    @Test func capsAtSixtySeconds() {
        // 1000 << 6 == 64000, clamped to the 60s ceiling.
        #expect(ms(Backoff.delay(attempt: 6, jitterUnit: 0)) == 60_000)
        // Even with full jitter and a huge attempt, never exceed the cap.
        #expect(ms(Backoff.delay(attempt: 20, jitterUnit: 1)) == 60_000)
    }

    @Test func staysWithinBounds() {
        for attempt in 0...15 {
            for j in [0.0, 0.3, 0.7, 1.0] {
                let d = ms(Backoff.delay(attempt: attempt, jitterUnit: j))
                #expect(d >= 1_000)
                #expect(d <= 60_000)
            }
        }
    }
}
