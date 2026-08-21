//
//  DesignSystemTokenTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the expanded semantic design tokens (Theme.swift) resolve to the
//  exact PRD §11 hex values, radii/spacing are sane, and AgentStatus maps to
//  the right status colors (PRD §4).
//

import Testing
import SwiftUI
@testable import FlightDeckRemote

struct DesignSystemTokenTests {

    private func assertHex(_ color: Color, _ hex: UInt32) {
        let expected = Color(hex: hex).resolve(in: .init())
        let actual = color.resolve(in: .init())
        #expect(abs(Double(actual.red) - Double(expected.red)) < 0.01)
        #expect(abs(Double(actual.green) - Double(expected.green)) < 0.01)
        #expect(abs(Double(actual.blue) - Double(expected.blue)) < 0.01)
    }

    @Test func semanticBackgroundTokensMatchPalette() {
        assertHex(Theme.bgDeep, 0x061417)
        assertHex(Theme.bgField, 0x05191C)
        assertHex(Theme.bgCard, 0x0B2429)
        assertHex(Theme.bgRaised, 0x0D353B)
    }

    @Test func semanticTextTokensMatchPalette() {
        assertHex(Theme.textPrimary, 0xF7F8E2)
        assertHex(Theme.textMuted, 0xA9C0C0)
        assertHex(Theme.textDim, 0x6E8488)
    }

    @Test func accentTokenMatchesBrandOrange() {
        assertHex(Theme.accent, 0xFF6601)
    }

    @Test func semanticStatusTokensMatchPRDPalette() {
        assertHex(Theme.statusWorking, 0xE5484D)
        assertHex(Theme.statusIdle, 0x6FB26C)
        assertHex(Theme.statusNeedsInput, 0xFF6601)
        assertHex(Theme.statusManual, 0x4FB3C4)
    }

    @Test func semanticTokensAliasOriginalTokens() {
        // The design-system task's semantic names must not fork the palette
        // — they're aliases onto the same original tokens already consumed
        // by Feature placeholder views.
        assertHex(Theme.bgDeep, 0x061417)
        #expect(Theme.background.resolve(in: .init()).red == Theme.bgDeep.resolve(in: .init()).red)
        #expect(Theme.surface.resolve(in: .init()).red == Theme.bgCard.resolve(in: .init()).red)
        #expect(Theme.surfaceElevated.resolve(in: .init()).red == Theme.bgRaised.resolve(in: .init()).red)
        #expect(Theme.text.resolve(in: .init()).red == Theme.textPrimary.resolve(in: .init()).red)
        #expect(Theme.textMutedDark.resolve(in: .init()).red == Theme.textDim.resolve(in: .init()).red)
        #expect(Theme.statusRed.resolve(in: .init()).red == Theme.statusWorking.resolve(in: .init()).red)
        #expect(Theme.statusGreen.resolve(in: .init()).red == Theme.statusIdle.resolve(in: .init()).red)
        #expect(Theme.statusOrange.resolve(in: .init()).red == Theme.statusNeedsInput.resolve(in: .init()).red)
        #expect(Theme.statusCyan.resolve(in: .init()).red == Theme.statusManual.resolve(in: .init()).red)
    }

    @Test func radiiAreOrderedAndPositive() {
        #expect(Theme.Radius.card > 0)
        #expect(Theme.Radius.cardLarge > Theme.Radius.card)
        #expect(Theme.Radius.sheet > Theme.Radius.cardLarge)
        #expect(Theme.Radius.pill > Theme.Radius.sheet)
    }

    @Test func spacingScaleIsMonotonicallyIncreasing() {
        let scale = [
            Theme.Spacing.xxs, Theme.Spacing.xs, Theme.Spacing.sm, Theme.Spacing.md,
            Theme.Spacing.lg, Theme.Spacing.xl, Theme.Spacing.xxl, Theme.Spacing.xxxl,
        ]
        #expect(scale == scale.sorted())
        #expect(Set(scale).count == scale.count)
    }

    @Test func agentStatusMapsToPRDColors() {
        #expect(AgentStatus.working.color.resolve(in: .init()).red == Theme.statusWorking.resolve(in: .init()).red)
        #expect(AgentStatus.idle.color.resolve(in: .init()).red == Theme.statusIdle.resolve(in: .init()).red)
        #expect(AgentStatus.needsInput.color.resolve(in: .init()).red == Theme.statusNeedsInput.resolve(in: .init()).red)
        #expect(AgentStatus.manual().color.resolve(in: .init()).red == Theme.statusManual.resolve(in: .init()).red)
    }

    @Test func onlyNeedsInputPulsesByDefault() {
        #expect(AgentStatus.working.pulsesByDefault == false)
        #expect(AgentStatus.idle.pulsesByDefault == false)
        #expect(AgentStatus.needsInput.pulsesByDefault == true)
        #expect(AgentStatus.manual().pulsesByDefault == false)
    }
}
