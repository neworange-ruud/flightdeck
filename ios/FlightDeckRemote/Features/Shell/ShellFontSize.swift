//
//  ShellFontSize.swift
//  FlightDeckRemote
//
//  The shell terminal's font-size control (PRD §5.4 landscape/font-size
//  follow-up). A small fixed ladder of point sizes that the toolbar's "font"
//  button cycles through, one tap at a time, and `@AppStorage` persists
//  (see `ShellView`'s `fontSizeRaw`) so the choice survives relaunches and is
//  shared by every shell surface (Shell tab + Chat `Agent · Shell`). Kept as
//  a pure, UIKit-free enum so the cycling/clamping rule is unit-testable in
//  isolation from the SwiftTerm renderer.
//

import Foundation

/// One step on the shell terminal's font-size ladder. The raw value is the
/// point size fed straight to the SwiftTerm renderer's `UIFont`.
enum ShellFontSize: Double, CaseIterable, Sendable {
    case small = 11
    case regular = 13
    case medium = 15
    case large = 17
    case extraLarge = 19

    /// The default step — matches the renderer's original hardcoded 13pt.
    static let `default`: ShellFontSize = .regular

    /// The next step in the ladder, wrapping back to the smallest after the
    /// largest — a single "font" button cycles rather than needing separate
    /// +/- controls.
    var next: ShellFontSize {
        let all = Self.allCases
        guard let index = all.firstIndex(of: self) else { return .default }
        let nextIndex = all.index(after: index)
        return nextIndex == all.endIndex ? all[all.startIndex] : all[nextIndex]
    }

    /// Resolve a persisted raw value (e.g. read back from `@AppStorage`),
    /// falling back to `.default` for an unrecognized/corrupt stored value
    /// (e.g. a value from a future app version, or the `@AppStorage`
    /// property's zero-initial-state before it has ever been written).
    static func resolved(rawValue: Double) -> ShellFontSize {
        ShellFontSize(rawValue: rawValue) ?? .default
    }
}
