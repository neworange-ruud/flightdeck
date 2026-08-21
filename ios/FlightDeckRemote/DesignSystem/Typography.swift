//
//  Typography.swift
//  FlightDeckRemote
//
//  Type tokens (PRD §11): Geist for UI, Geist Mono for code / terminal / git
//  indicators. Fonts are bundled under Resources/Fonts (OFL-licensed, see
//  Resources/Fonts/OFL.txt) and registered via `UIAppFonts` in project.yml.
//
//  Every preset goes through `isCustomFontAvailable` so the whole app
//  degrades gracefully to system fonts (SF / SF Mono) if font registration
//  ever fails (e.g. a stripped test bundle) — callers never branch on font
//  availability themselves.
//

import SwiftUI
import UIKit

/// Central typography namespace. Use `Theme.Spacing` / `Theme.Radius` for
/// layout tokens; this is scoped to fonts, sizes, and tracking presets.
enum Typography {

    /// The bundled Geist / Geist Mono PostScript names, as registered via
    /// `UIAppFonts` (see project.yml) from Resources/Fonts/*.ttf.
    enum FontName: String {
        case geistRegular = "Geist-Regular"
        case geistMedium = "Geist-Medium"
        case geistSemiBold = "Geist-SemiBold"
        case geistBold = "Geist-Bold"
        case monoRegular = "GeistMono-Regular"
        case monoMedium = "GeistMono-Medium"
    }

    /// Whether the bundled Geist fonts are actually registered and usable.
    ///
    /// Checked once per process via `UIFont(name:size:)`. If this is
    /// `false` (e.g. resources failed to bundle), every preset below falls
    /// back to the closest system font/weight instead.
    static let isCustomFontAvailable: Bool = {
        UIFont(name: FontName.geistRegular.rawValue, size: 12) != nil
    }()

    /// A resolved type preset: a `Font` plus its tracking (letter-spacing).
    struct Style {
        let font: Font
        let tracking: CGFloat
    }

    // MARK: - Font builders

    private static func geist(_ name: FontName, size: CGFloat, fallbackWeight: Font.Weight) -> Font {
        isCustomFontAvailable
            ? .custom(name.rawValue, size: size)
            : .system(size: size, weight: fallbackWeight, design: .default)
    }

    private static func mono(_ name: FontName, size: CGFloat, fallbackWeight: Font.Weight) -> Font {
        isCustomFontAvailable
            ? .custom(name.rawValue, size: size)
            : .system(size: size, weight: fallbackWeight, design: .monospaced)
    }

    // MARK: - Presets (UI, Geist)

    /// Large title — pairing / empty-state headlines.
    static let largeTitle = Style(font: geist(.geistBold, size: 32, fallbackWeight: .bold), tracking: 0.1)
    /// Screen / sheet title.
    static let title = Style(font: geist(.geistSemiBold, size: 22, fallbackWeight: .semibold), tracking: 0)
    /// Section headline (card titles, row primary text).
    static let headline = Style(font: geist(.geistSemiBold, size: 17, fallbackWeight: .semibold), tracking: 0)
    /// Standard body copy.
    static let body = Style(font: geist(.geistRegular, size: 16, fallbackWeight: .regular), tracking: 0)
    /// Medium-weight body (emphasized inline text).
    static let bodyMedium = Style(font: geist(.geistMedium, size: 16, fallbackWeight: .medium), tracking: 0)
    /// Secondary/supporting copy.
    static let callout = Style(font: geist(.geistRegular, size: 14, fallbackWeight: .regular), tracking: 0)
    /// Captions, timestamps, muted metadata.
    static let caption = Style(font: geist(.geistRegular, size: 12, fallbackWeight: .regular), tracking: 0.2)
    /// Small bold uppercase label — status pills, section eyebrows.
    static let captionBold = Style(font: geist(.geistSemiBold, size: 11, fallbackWeight: .semibold), tracking: 0.6)

    // MARK: - Presets (monospace, Geist Mono)

    /// Standard monospace — shell output, code blocks.
    static let mono = Style(font: mono(.monoRegular, size: 14, fallbackWeight: .regular), tracking: 0)
    /// Compact monospace — git indicators, inline diff counts.
    static let monoSmall = Style(font: mono(.monoRegular, size: 12, fallbackWeight: .regular), tracking: 0)
    /// Medium-weight monospace — emphasized values in a mono context.
    static let monoMedium = Style(font: mono(.monoMedium, size: 13, fallbackWeight: .medium), tracking: 0)
}

extension View {
    /// Applies a `Typography.Style` (font + tracking) in one call.
    func typography(_ style: Typography.Style) -> some View {
        font(style.font).tracking(style.tracking)
    }
}
