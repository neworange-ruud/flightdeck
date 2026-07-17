//
//  Theme.swift
//  FlightDeckRemote
//
//  "Midnight mission-control" design tokens — PRD §11.
//
//  This is the single source of truth for color, radius, and spacing tokens.
//  The original (pre-design-system-task) token names are kept exactly as-is
//  — several placeholder Feature views already reference them — and a fuller
//  set of semantic names is layered on top per the design-system brief:
//  bgDeep / bgField / bgCard / bgRaised, textPrimary / textMuted / textDim,
//  accent, and statusWorking / statusIdle / statusNeedsInput / statusManual.
//

import SwiftUI

/// Central design-token namespace for FlightDeck Remote.
///
/// The app is dark-only (see `UIUserInterfaceStyle` = Dark in Info.plist), so
/// these colors are not adapted for a light appearance.
enum Theme {

    // MARK: - Brand

    /// Signal orange — brand color, "needs input" status, primary CTAs.
    static let orange = Color(hex: 0xFF6601)

    // MARK: - Midnight greens (field/background scale, darkest → lightest)

    /// Deepest background (sunken fields, wells).
    static let midnight05 = Color(hex: 0x05191C)
    /// Base background.
    static let midnight06 = Color(hex: 0x061417)
    /// Elevated surface (cards, sheets).
    static let midnight0B = Color(hex: 0x0B2429)
    /// Highest elevation surface (headers, selected rows).
    static let midnight0D = Color(hex: 0x0D353B)

    // MARK: - Text

    /// Primary text color.
    static let text = Color(hex: 0xF7F8E2)
    /// Secondary/muted text — lighter muted tone.
    static let textMuted = Color(hex: 0xA9C0C0)
    /// Tertiary/muted text — darker muted tone (captions, disabled).
    static let textMutedDark = Color(hex: 0x6E8488)

    // MARK: - Status colors (PRD §4)

    /// Working — agent actively running a turn.
    static let statusRed = Color(hex: 0xE5484D)
    /// Idle / finished — turn done, waiting for a prompt.
    static let statusGreen = Color(hex: 0x6FB26C)
    /// Needs input — most urgent, pulls the user in. Same as `orange`.
    static let statusOrange = orange
    /// Manual override — user-flagged status.
    static let statusCyan = Color(hex: 0x4FB3C4)

    // MARK: - Surfaces (semantic aliases, original names)

    /// App-wide background color.
    static let background = midnight06
    /// Default card/surface background.
    static let surface = midnight0B
    /// Elevated surface (e.g. sheets, selected state).
    static let surfaceElevated = midnight0D

    // MARK: - Semantic tokens (design-system task naming)
    //
    // These are aliases onto the palette above — kept as distinct `static
    // let`s (rather than typealiases) so call sites read by role
    // ("Theme.bgCard") instead of by raw palette step ("Theme.midnight0B").

    /// App root / deepest background. Same value as `background`.
    static let bgDeep = midnight06
    /// Sunken field background (text inputs, wells, code blocks).
    static let bgField = midnight05
    /// Card / row surface background. Same value as `surface`.
    static let bgCard = midnight0B
    /// Raised surface (sheets, headers, selected rows). Same as `surfaceElevated`.
    static let bgRaised = midnight0D

    /// Primary text color. Same value as `text`.
    static let textPrimary = text
    /// Tertiary/dimmest text (captions, disabled, timestamps). Same as `textMutedDark`.
    static let textDim = textMutedDark

    /// Brand / signal accent. Same value as `orange`.
    static let accent = orange

    /// Working — animated red spinner state.
    static let statusWorking = statusRed
    /// Idle / finished — green state.
    static let statusIdle = statusGreen
    /// Needs input — orange glow state. Most urgent.
    static let statusNeedsInput = statusOrange
    /// Manual override — cyan state.
    static let statusManual = statusCyan

    // MARK: - Radii

    /// Corner-radius scale (PRD §11: "large corner radii, cards ~16–22pt").
    enum Radius {
        /// Standard card / row corner radius.
        static let card: CGFloat = 18
        /// Larger card radius (project cards, prominent tiles).
        static let cardLarge: CGFloat = 22
        /// Sheet / modal corner radius.
        static let sheet: CGFloat = 28
        /// Pill badges — large enough to always fully round the shape.
        static let pill: CGFloat = 999
    }

    // MARK: - Spacing

    /// 4pt-based spacing scale.
    enum Spacing {
        static let xxs: CGFloat = 2
        static let xs: CGFloat = 4
        static let sm: CGFloat = 8
        static let md: CGFloat = 12
        static let lg: CGFloat = 16
        static let xl: CGFloat = 20
        static let xxl: CGFloat = 24
        static let xxxl: CGFloat = 32
    }
}

extension Color {
    /// Convenience initializer for design tokens expressed as 0xRRGGBB.
    init(hex: UInt32) {
        let r = Double((hex >> 16) & 0xFF) / 255.0
        let g = Double((hex >> 8) & 0xFF) / 255.0
        let b = Double(hex & 0xFF) / 255.0
        self.init(.sRGB, red: r, green: g, blue: b, opacity: 1.0)
    }
}
