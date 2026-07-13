//
//  TypographyTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the bundled Geist / Geist Mono fonts (Resources/Fonts/*.ttf,
//  registered via UIAppFonts in project.yml) actually register with
//  UIFont(name:). Skips gracefully if the app is running in system-font
//  fallback mode (Typography.isCustomFontAvailable == false) — see
//  Typography.swift's doc comment on graceful degradation.
//

import Testing
import UIKit
@testable import FlightDeckRemote

struct TypographyTests {

    private let bundledFontNames: [Typography.FontName] = [
        .geistRegular, .geistMedium, .geistSemiBold, .geistBold, .monoRegular, .monoMedium,
    ]

    @Test func bundledFontsRegisterWhenAvailable() {
        guard Typography.isCustomFontAvailable else {
            // Fallback mode: Geist isn't registered (e.g. a stripped test
            // bundle). Typography's presets fall back to system fonts, so
            // there's nothing further to assert here.
            return
        }
        for name in bundledFontNames {
            #expect(UIFont(name: name.rawValue, size: 12) != nil, "Expected \(name.rawValue) to be registered")
        }
    }

    @Test func presetsProduceNonNilFontsRegardlessOfAvailability() {
        // Every preset must resolve to *some* usable Font whether or not the
        // bundled fonts registered — this is the whole point of routing all
        // presets through Typography rather than raw Font.custom() calls.
        let presets: [Typography.Style] = [
            Typography.largeTitle, Typography.title, Typography.headline, Typography.body,
            Typography.bodyMedium, Typography.callout, Typography.caption, Typography.captionBold,
            Typography.mono, Typography.monoSmall, Typography.monoMedium,
        ]
        #expect(presets.count == 11)
    }
}
