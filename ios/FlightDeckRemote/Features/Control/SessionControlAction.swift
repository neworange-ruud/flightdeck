//
//  SessionControlAction.swift
//  FlightDeckRemote
//
//  The Tier-2 session actions catalog (PRD §5.6): the safe group on top
//  (Restart agent, Open shell, Set manual status, Pull base, Merge back) and
//  the destructive group apart in red (Close session, Abandon worktree) —
//  never mixed. Plus the pure pieces the sheet renders from:
//   - the action → `Wire.CommandBody` mapping (unit-tested against the wire
//     protocol, so a tap can never send the wrong command);
//   - each action's confirmation copy (PRD §8: state changes are deliberate
//     and confirmed; title + consequence sentence + confirm/cancel);
//   - the abandon type-to-confirm gate (exact-match only, PRD §5.6);
//   - honest in-flight phrasing per command.
//

import Foundation

// MARK: - Action catalog

enum SessionControlAction: String, CaseIterable, Identifiable, Sendable {
    case restartAgent
    case openShell
    case setManualStatus
    case pullBase
    case mergeBack
    case closeSession
    case abandonWorktree

    var id: String { rawValue }

    /// The safe group, in PRD §5.6 order.
    static let safeGroup: [SessionControlAction] = [
        .restartAgent, .openShell, .setManualStatus, .pullBase, .mergeBack,
    ]

    /// The destructive group, apart and in red.
    static let destructiveGroup: [SessionControlAction] = [
        .closeSession, .abandonWorktree,
    ]

    var title: String {
        switch self {
        case .restartAgent: "Restart agent"
        case .openShell: "Open shell"
        case .setManualStatus: "Set manual status"
        case .pullBase: "Pull base"
        case .mergeBack: "Merge back"
        case .closeSession: "Close session"
        case .abandonWorktree: "Abandon worktree"
        }
    }

    var systemImage: String {
        switch self {
        case .restartAgent: "arrow.clockwise"
        case .openShell: "terminal"
        case .setManualStatus: "tag"
        case .pullBase: "arrow.down.to.line"
        case .mergeBack: "arrow.triangle.merge"
        case .closeSession: "xmark.circle"
        case .abandonWorktree: "trash"
        }
    }

    var isDestructive: Bool {
        switch self {
        case .closeSession, .abandonWorktree: true
        default: false
        }
    }

    /// Stable accessibility identifier for the sheet row.
    var accessibilityIdentifier: String {
        switch self {
        case .restartAgent: "control-action-restart"
        case .openShell: "control-action-shell"
        case .setManualStatus: "control-action-manual-status"
        case .pullBase: "control-action-pull-base"
        case .mergeBack: "control-action-merge-back"
        case .closeSession: "control-action-close"
        case .abandonWorktree: "control-action-abandon"
        }
    }
}

// MARK: - Action → wire command mapping

/// The one place a control tap becomes a wire command. Unit-tested for all
/// seven bodies so the mapping can never drift from the protocol.
enum ControlCommands {
    static func restartAgent(_ sessionId: Wire.SessionId) -> Wire.CommandBody {
        .restartAgent(sessionId: sessionId)
    }

    static func closeSession(_ sessionId: Wire.SessionId) -> Wire.CommandBody {
        .closeSession(sessionId: sessionId)
    }

    static func setManualStatus(_ sessionId: Wire.SessionId, label: String) -> Wire.CommandBody {
        .setManualStatus(sessionId: sessionId, label: label)
    }

    static func clearManualStatus(_ sessionId: Wire.SessionId) -> Wire.CommandBody {
        .clearManualStatus(sessionId: sessionId)
    }

    static func pullBase(_ sessionId: Wire.SessionId) -> Wire.CommandBody {
        .gitPullBase(sessionId: sessionId)
    }

    static func mergeBack(_ sessionId: Wire.SessionId) -> Wire.CommandBody {
        .gitMergeBack(sessionId: sessionId)
    }

    static func abandonWorktree(_ sessionId: Wire.SessionId, confirmName: String) -> Wire.CommandBody {
        .gitAbandonWorktree(sessionId: sessionId, confirmName: confirmName)
    }
}

// MARK: - Confirmation copy

/// A standard confirmation dialog's copy: title + consequence sentence +
/// confirm label (PRD §8).
struct ControlConfirmation: Equatable, Sendable {
    var title: String
    var message: String
    var confirmLabel: String
    var isDestructive: Bool
}

extension SessionControlAction {
    /// The standard confirmation for this action, or nil when the action uses
    /// its own flow (manual status sub-sheet, abandon type-to-confirm, open
    /// shell — opening a shell is read-frictionless, no confirmation).
    func confirmation(sessionName: String) -> ControlConfirmation? {
        switch self {
        case .restartAgent:
            return ControlConfirmation(
                title: "Restart agent?",
                message: "Relaunches a fresh agent process in the same worktree and branch. The transcript is preserved.",
                confirmLabel: "Restart",
                isDestructive: false)
        case .pullBase:
            return ControlConfirmation(
                title: "Pull base?",
                message: "Pulls the base branch into the \(sessionName) worktree. A conflict stops the pull and is reported here.",
                confirmLabel: "Pull",
                isDestructive: false)
        case .mergeBack:
            return ControlConfirmation(
                title: "Merge back?",
                message: "Merges \(sessionName) back into its base branch on your Mac. Nothing is pushed.",
                confirmLabel: "Merge",
                isDestructive: false)
        case .closeSession:
            return ControlConfirmation(
                title: "Close session?",
                message: "If the agent is running it is stopped first (Ctrl-C) and the tab stays — tap close again once it's idle. Closing removes the \(sessionName) session on your Mac.",
                confirmLabel: "Stop & close",
                isDestructive: true)
        case .openShell, .setManualStatus, .abandonWorktree:
            return nil
        }
    }
}

// MARK: - Abandon type-to-confirm gate

/// PRD §5.6: the one truly destructive path requires typing the session name.
/// Exact match only — no trimming, no case folding — so the gate can never be
/// satisfied by a near-miss.
enum AbandonConfirmLogic {
    static func isConfirmed(input: String, sessionName: String) -> Bool {
        !sessionName.isEmpty && input == sessionName
    }
}

// MARK: - Honest in-flight phrasing

enum ControlActionPhrasing {
    /// The spinner label while a control command is in flight.
    static func inFlightLabel(for body: Wire.CommandBody?) -> String {
        switch body {
        case .restartAgent: "Restarting agent…"
        case .closeSession: "Closing session…"
        case .setManualStatus: "Setting status…"
        case .clearManualStatus: "Clearing status…"
        case .gitPullBase: "Pulling base…"
        case .gitMergeBack: "Merging back…"
        case .gitAbandonWorktree: "Abandoning worktree…"
        case .newAgent: "Launching agent…"
        default: "Sending…"
        }
    }
}
