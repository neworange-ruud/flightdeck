//
//  RunningTimeTests.swift
//  FlightDeckRemoteTests
//
//  Covers `RunningTime.format` (PRD §5.2): `1h 12m` / `8m` / `just started`.
//

import Testing
@testable import FlightDeckRemote

struct RunningTimeTests {

    @Test func underAMinuteIsJustStarted() {
        #expect(RunningTime.format(seconds: 0) == "just started")
        #expect(RunningTime.format(seconds: 1) == "just started")
        #expect(RunningTime.format(seconds: 59) == "just started")
    }

    @Test func minutesOnlyUnderAnHour() {
        #expect(RunningTime.format(seconds: 60) == "1m")
        #expect(RunningTime.format(seconds: 480) == "8m")
        #expect(RunningTime.format(seconds: 3599) == "59m")
    }

    @Test func hoursAndMinutes() {
        #expect(RunningTime.format(seconds: 3600) == "1h 0m")
        #expect(RunningTime.format(seconds: 4320) == "1h 12m")
        #expect(RunningTime.format(seconds: 7_260) == "2h 1m")
    }
}
