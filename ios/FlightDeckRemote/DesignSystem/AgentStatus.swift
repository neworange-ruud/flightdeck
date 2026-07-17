//
//  AgentStatus.swift
//  FlightDeckRemote
//
//  Design-system-local status enum (PRD §4). This is intentionally *not*
//  the transport/data-model status type — it exists so DesignSystem
//  components (StatusDot, WorkingSpinner, StatusPill, NotificationCell) can
//  be built and previewed independently of the Transport layer. Once the
//  real agent/session model lands, it will map onto this (or replace it).
//
import SwiftUI

/// The four states an agent (or a project roll-up) can be in.
enum AgentStatus: Hashable {
    /// Agent is actively running a turn. Red, animated spinner.
    case working
    /// Turn done, waiting for a prompt. Green.
    case idle
    /// Agent stopped, asking the human (permission / question). Orange glow,
    /// the most urgent state.
    case needsInput
    /// User-flagged manual override, with a custom label. Cyan.
    case manual(label: String = "manual")

    /// The token color for this status (PRD §4 / §11).
    var color: Color {
        switch self {
        case .working: Theme.statusWorking
        case .idle: Theme.statusIdle
        case .needsInput: Theme.statusNeedsInput
        case .manual: Theme.statusManual
        }
    }

    /// Plain-language label, e.g. for pills and roll-up text.
    var label: String {
        switch self {
        case .working: "working"
        case .idle: "idle"
        case .needsInput: "needs input"
        case .manual(let label): label
        }
    }

    /// Whether this status should render a pulsing/glowing animation by
    /// default (StatusDot) — only "needs input" pulls the user in urgently.
    var pulsesByDefault: Bool {
        if case .needsInput = self { return true }
        return false
    }

    /// A stable, identifier-safe string for accessibility identifiers and
    /// gallery section keys.
    var identifier: String {
        switch self {
        case .working: "working"
        case .idle: "idle"
        case .needsInput: "needs-input"
        case .manual(let label): "manual-\(label)"
        }
    }
}
