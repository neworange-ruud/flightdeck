//
//  ShellFontSizeTests.swift
//  FlightDeckRemoteTests
//
//  The font-size ladder's cycling + clamping rule (PRD §5.4 font-size
//  control). Pure logic, no view/renderer involved.
//

import Testing
@testable import FlightDeckRemote

@Suite struct ShellFontSizeTests {

    // MARK: - Default

    @Test func defaultIsRegular13pt() {
        #expect(ShellFontSize.default == .regular)
        #expect(ShellFontSize.default.rawValue == 13)
    }

    // MARK: - Cycling

    @Test func cyclesThroughTheLadderInOrder() {
        #expect(ShellFontSize.small.next == .regular)
        #expect(ShellFontSize.regular.next == .medium)
        #expect(ShellFontSize.medium.next == .large)
        #expect(ShellFontSize.large.next == .extraLarge)
    }

    @Test func wrapsAroundFromLargestToSmallest() {
        #expect(ShellFontSize.extraLarge.next == .small)
    }

    @Test func repeatedCyclingReturnsToStart() {
        var size = ShellFontSize.small
        for _ in ShellFontSize.allCases {
            size = size.next
        }
        #expect(size == .small)
    }

    // MARK: - Resolving a persisted raw value

    @Test func resolvedMatchesAnExactKnownRawValue() {
        #expect(ShellFontSize.resolved(rawValue: 11) == .small)
        #expect(ShellFontSize.resolved(rawValue: 13) == .regular)
        #expect(ShellFontSize.resolved(rawValue: 19) == .extraLarge)
    }

    @Test func resolvedFallsBackToDefaultForAnUnknownRawValue() {
        #expect(ShellFontSize.resolved(rawValue: 0) == .default)
        #expect(ShellFontSize.resolved(rawValue: 999) == .default)
    }
}
