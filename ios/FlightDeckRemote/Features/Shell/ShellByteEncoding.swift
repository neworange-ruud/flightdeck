//
//  ShellByteEncoding.swift
//  FlightDeckRemote
//
//  Pure byte-level encoding for the minimal shell terminal (PRD §5.4). All of
//  the "which bytes does this key send" knowledge lives here as static,
//  side-effect-free functions so the mapping is unit-tested independently of
//  any view, keyboard, or transport.
//
//  Everything the phone types is delivered to the desktop as the `data` string
//  of a `shell_input` command (E2E.swift `CommandBody.shellInput`). On the wire
//  `data` is a UTF-8 string, so these helpers produce `[UInt8]` and the model
//  turns them into a `String` via `String(decoding:as:)` — a lossless round
//  trip for the ASCII control/escape bytes below.
//
//  Bracketed paste (PRD §5.4 copy/paste): v1 sends plain text on paste. The
//  desktop shell only interprets bracketed-paste markers (ESC[200~ … ESC[201~)
//  when the running program enabled the mode; the remote does not track that
//  bit, so wrapping blindly would corrupt input for programs that never asked
//  for it. Plain paste is the honest, safe default; the markers are documented
//  here (`bracketedPasteStart`/`End`) for the fast-follow that negotiates it.
//

import Foundation

/// Static byte sequences and composition rules for terminal input. Pure.
enum ShellByteEncoding {

    // MARK: - Single control keys

    /// Escape (`Esc`).
    static let escape: [UInt8] = [0x1b]
    /// Horizontal tab (`Tab`).
    static let tab: [UInt8] = [0x09]
    /// Carriage return (`Return`/Enter) — what a shell expects for "run".
    static let carriageReturn: [UInt8] = [0x0d]
    /// ETX (`Ctrl-C`) — the control *byte*. Note: the terminal surface sends a
    /// `shell_interrupt` command for the interrupt button instead (it works
    /// even when the PTY is wedged); this byte exists for the sticky-`Ctrl`
    /// composition path (`Ctrl` armed + `c`).
    static let etx: [UInt8] = [0x03]

    // MARK: - Arrow keys (CSI sequences)

    /// Cursor up — `ESC [ A`.
    static let arrowUp: [UInt8] = [0x1b, 0x5b, 0x41]
    /// Cursor down — `ESC [ B`.
    static let arrowDown: [UInt8] = [0x1b, 0x5b, 0x42]
    /// Cursor right — `ESC [ C`.
    static let arrowRight: [UInt8] = [0x1b, 0x5b, 0x43]
    /// Cursor left — `ESC [ D`.
    static let arrowLeft: [UInt8] = [0x1b, 0x5b, 0x44]

    // MARK: - Bracketed paste markers (documented, not sent in v1)

    /// `ESC [ 200 ~` — bracketed-paste begin (fast-follow only).
    static let bracketedPasteStart: [UInt8] = [0x1b, 0x5b, 0x32, 0x30, 0x30, 0x7e]
    /// `ESC [ 201 ~` — bracketed-paste end (fast-follow only).
    static let bracketedPasteEnd: [UInt8] = [0x1b, 0x5b, 0x32, 0x30, 0x31, 0x7e]

    // MARK: - Text

    /// UTF-8 bytes for arbitrary typed / pasted text.
    static func bytes(for text: String) -> [UInt8] {
        Array(text.utf8)
    }

    // MARK: - Control composition (sticky `Ctrl`)

    /// Apply the `Ctrl` modifier to an input byte sequence, mirroring a real
    /// terminal: a single printable ASCII byte `b` becomes `b & 0x1f` (so
    /// `Ctrl`+`c` (`0x63`) → `0x03` ETX, `Ctrl`+`d` → `0x04`, etc.). Anything
    /// that isn't a lone printable ASCII byte (empty, multi-byte escape
    /// sequences like the arrows, or non-ASCII) passes through unchanged —
    /// `Ctrl` composed with a cursor key has no portable meaning, so we send
    /// the key as-is rather than corrupt it.
    static func applyControl(to bytes: [UInt8]) -> [UInt8] {
        guard bytes.count == 1, let b = bytes.first, (0x20...0x7e).contains(b) else {
            return bytes
        }
        return [b & 0x1f]
    }

    /// Convenience: the control byte for a character, or `nil` when the
    /// character isn't a single printable ASCII scalar. `control(for: "c")` is
    /// `0x03`.
    static func control(for character: Character) -> UInt8? {
        guard let ascii = character.asciiValue, (0x20...0x7e).contains(ascii) else {
            return nil
        }
        return ascii & 0x1f
    }

    // MARK: - Wire helpers

    /// Turn a byte sequence into the `shell_input` `data` string (lossless for
    /// the ASCII control/escape bytes this type produces).
    static func wireString(_ bytes: [UInt8]) -> String {
        String(decoding: bytes, as: UTF8.self)
    }
}
