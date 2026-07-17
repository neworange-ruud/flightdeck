//
//  ShellStateMachine.swift
//  FlightDeckRemote
//
//  The shell lifecycle state machine (PRD §5.4, desktop contract). Pure: a
//  phase + a reducer over lifecycle inputs, so the connect → live → exited →
//  closed flow (and the "already open" rejection) is unit-tested without a
//  view or transport.
//
//  Desktop contract mirrored here:
//   * `shell_open` → the desktop acks `applied` and pushes a `ShellEvent`
//     `opened{cols,rows}` carrying the `shell_id` → phase becomes `.live`.
//   * a second `shell_open` while one is live is rejected "already open" → the
//     open command's ack is `.rejected` → phase `.rejectedAlreadyOpen`.
//   * the process dying pushes `exited{code}` but the desktop holds the slot
//     until `shell_close` → phase `.exited(code)` (Close / Reopen offered).
//   * `closed` (either side) → phase `.closed`.
//   * desktop unpair drops shells silently — the phone re-opens after re-pair;
//     that surfaces as the link going down (paused gate) then a fresh `.open`.
//

import Foundation

/// The observable phase of the session's single shell.
enum ShellPhase: Equatable, Sendable {
    /// No shell yet — show the "Open shell in <worktree>" CTA.
    case noShell
    /// `shell_open` sent, awaiting the `opened` event / ack.
    case opening
    /// Live: streaming output, accepting input.
    case live
    /// The process exited; the slot is still held until we `shell_close`.
    case exited(code: Int32?)
    /// A `shell_open` was rejected because one is already open (v1: honest
    /// message + offer to Close the existing).
    case rejectedAlreadyOpen(message: String)
    /// The shell was closed (by either side).
    case closed
}

/// Lifecycle inputs that drive the phase.
enum ShellLifecycleInput: Equatable, Sendable {
    /// The user asked to open a shell (or reopen after exit/close).
    case requestOpen
    /// A `ShellEvent.opened` arrived for our shell.
    case opened
    /// A `ShellEvent.exited` arrived.
    case exited(code: Int32?)
    /// A `ShellEvent.closed` arrived, or we closed it.
    case closed
    /// Our `shell_open` command was rejected (e.g. "already open").
    case openRejected(message: String)
}

/// Pure reducer. Deliberately total: unexpected inputs for a phase leave it
/// unchanged rather than trap, since the desktop is the source of truth and a
/// late/duplicate event must never crash the surface.
enum ShellStateMachine {

    static func reduce(_ phase: ShellPhase, _ input: ShellLifecycleInput) -> ShellPhase {
        switch (phase, input) {
        // Opening (fresh, or after exit/close/rejection).
        case (.noShell, .requestOpen),
             (.exited, .requestOpen),
             (.closed, .requestOpen),
             (.rejectedAlreadyOpen, .requestOpen):
            return .opening

        // Open confirmed.
        case (.opening, .opened):
            return .live
        // A stray `opened` while already live is a no-op.
        case (.live, .opened):
            return .live

        // Rejection while opening.
        case (.opening, let .openRejected(message)):
            return .rejectedAlreadyOpen(message: message)

        // Process died — slot still held.
        case (.opening, let .exited(code)),
             (.live, let .exited(code)):
            return .exited(code: code)

        // Closed from any phase.
        case (_, .closed):
            return .closed

        // Everything else: no change.
        default:
            return phase
        }
    }

    /// Map a decoded `ShellEventKind` to a lifecycle input.
    static func input(for kind: Wire.ShellEventKind) -> ShellLifecycleInput {
        switch kind {
        case .opened: .opened
        case let .exited(code): .exited(code: code)
        case .closed: .closed
        }
    }
}
