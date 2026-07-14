//
//  ShellKey.swift
//  FlightDeckRemote
//
//  The accessory key bar's keys (PRD §5.4 — the bar is make-or-break and IS
//  v1) and the *pure* rule that turns a key tap (plus the current sticky-`Ctrl`
//  state) into an action. Keeping the reducer pure lets the byte mapping and
//  the sticky-`Ctrl` composition be unit-tested with no view or transport.
//

import Foundation

/// One key on the shell accessory bar. Ordered roughly as laid out portrait:
/// `Esc Tab Ctrl ← ↑ ↓ → | / - ~ \` ⌃C Paste`.
enum ShellKey: String, CaseIterable, Hashable, Sendable {
    case escape
    case tab
    case ctrl        // sticky modifier — toggles, doesn't emit on its own
    case left
    case up
    case down
    case right
    case pipe        // |
    case slash       // /
    case dash        // -
    case tilde       // ~
    case backtick    // `
    case interrupt   // dedicated Ctrl-C → shell_interrupt (not a byte)
    case paste

    /// The label rendered on the button.
    var label: String {
        switch self {
        case .escape: "esc"
        case .tab: "tab"
        case .ctrl: "ctrl"
        case .left: "←"
        case .up: "↑"
        case .down: "↓"
        case .right: "→"
        case .pipe: "|"
        case .slash: "/"
        case .dash: "-"
        case .tilde: "~"
        case .backtick: "`"
        case .interrupt: "⌃C"
        case .paste: "paste"
        }
    }

    /// Stable accessibility identifier (`shell-key-esc`, …) for UI tests.
    var accessibilityId: String { "shell-key-\(rawValue)" }

    /// The raw (unmodified) bytes this key emits, or `nil` for keys that don't
    /// emit bytes directly (`ctrl`, `interrupt`, `paste` are handled specially).
    var rawBytes: [UInt8]? {
        switch self {
        case .escape: ShellByteEncoding.escape
        case .tab: ShellByteEncoding.tab
        case .left: ShellByteEncoding.arrowLeft
        case .up: ShellByteEncoding.arrowUp
        case .down: ShellByteEncoding.arrowDown
        case .right: ShellByteEncoding.arrowRight
        case .pipe: ShellByteEncoding.bytes(for: "|")
        case .slash: ShellByteEncoding.bytes(for: "/")
        case .dash: ShellByteEncoding.bytes(for: "-")
        case .tilde: ShellByteEncoding.bytes(for: "~")
        case .backtick: ShellByteEncoding.bytes(for: "`")
        case .ctrl, .interrupt, .paste: nil
        }
    }
}

/// The resolved effect of a key tap, given the sticky-`Ctrl` state at tap time.
enum ShellKeyAction: Equatable, Sendable {
    /// Send these bytes as `shell_input`. `ctrlConsumed` is true when a primed
    /// `Ctrl` was folded into these bytes (so the caller disarms the modifier).
    case sendBytes([UInt8], ctrlConsumed: Bool)
    /// Toggle the sticky `Ctrl` modifier (armed ⇄ idle).
    case toggleCtrl
    /// Fire the interrupt *command* (`shell_interrupt`), not a byte.
    case interrupt
    /// Read the pasteboard and send it as input.
    case paste
}

/// Pure reducer for the key bar. No state of its own — the caller owns
/// `ctrlArmed` and applies the returned action.
enum ShellKeyBarLogic {

    /// Resolve a key tap. `ctrlArmed` is the sticky-`Ctrl` state *before* the
    /// tap.
    static func action(for key: ShellKey, ctrlArmed: Bool) -> ShellKeyAction {
        switch key {
        case .ctrl:
            return .toggleCtrl
        case .interrupt:
            return .interrupt
        case .paste:
            return .paste
        default:
            guard let raw = key.rawBytes else { return .toggleCtrl }
            if ctrlArmed {
                return .sendBytes(ShellByteEncoding.applyControl(to: raw), ctrlConsumed: true)
            }
            return .sendBytes(raw, ctrlConsumed: false)
        }
    }

    /// Resolve raw keyboard input (from the terminal's own keyboard) against
    /// the sticky-`Ctrl` state. Returns the bytes to send and whether a primed
    /// `Ctrl` was consumed.
    static func keyboardInput(_ bytes: [UInt8], ctrlArmed: Bool) -> (bytes: [UInt8], ctrlConsumed: Bool) {
        guard ctrlArmed else { return (bytes, false) }
        return (ShellByteEncoding.applyControl(to: bytes), true)
    }
}
