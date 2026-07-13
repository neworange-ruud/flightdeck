//
//  ThemeTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the design-system color tokens resolve to the exact PRD §11
//  values, and that AppRouter picks the Pairing route when unpaired.
//

import Testing
import SwiftUI
@testable import FlightDeckRemote

struct ThemeTests {

    @Test func hexInitializerResolvesComponents() {
        let color = Color(hex: 0xFF6601)
        let resolved = color.resolve(in: .init())

        #expect(abs(Double(resolved.red) - 1.0) < 0.01)
        #expect(abs(Double(resolved.green) - (Double(0x66) / 255.0)) < 0.01)
        #expect(abs(Double(resolved.blue) - (Double(0x01) / 255.0)) < 0.01)
    }

    @Test func statusColorsMatchPRDPalette() {
        // PRD §4 / §11: red, green, orange, cyan status colors.
        let cases: [(Color, UInt32)] = [
            (Theme.statusRed, 0xE5484D),
            (Theme.statusGreen, 0x6FB26C),
            (Theme.statusOrange, 0xFF6601),
            (Theme.statusCyan, 0x4FB3C4),
        ]
        for (token, hex) in cases {
            let expected = Color(hex: hex).resolve(in: .init())
            let actual = token.resolve(in: .init())
            #expect(abs(Double(actual.red) - Double(expected.red)) < 0.01)
            #expect(abs(Double(actual.green) - Double(expected.green)) < 0.01)
            #expect(abs(Double(actual.blue) - Double(expected.blue)) < 0.01)
        }
    }

    @Test func routerEntersPairingWhenUnpaired() {
        // Entry flow per PRD §5.8: unpaired -> Pairing, paired -> Projects.
        let store = PairingStore()
        #expect(store.isPaired == false)
        #expect(AppRouter(pairingStore: store).route == .pairing)
    }

    @Test func routerEntersProjectsWhenPaired() {
        let store = PairingStore()
        store.isPaired = true
        #expect(AppRouter(pairingStore: store).route == .projects)
    }
}
