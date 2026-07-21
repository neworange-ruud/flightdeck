//
//  ChatSendLogic.swift
//  FlightDeckRemote
//
//  Pure, view-agnostic state machine for the Tier-1 killer loop (PRD §5.3 /
//  §5.8): sending a reply / follow-up and resolving a permission ask inline,
//  with honest delivery feedback ("not delivered — retry", never silent).
//
//  Everything here is a value transform with no SwiftUI / transport dependency
//  (it only reads `CommandDeliveryState`, an enum), so the send/retry/permission
//  state machine and the optimistic-message reconciliation heuristic can be
//  unit-tested directly.
//
//  Retry command-id semantics (the crux, PRD §5.8 delivery honesty):
//   - A *transport-level* `.failed` (timeout / link down / peer unavailable /
//     seal-or-send error) means we never saw the desktop's ack — the command
//     MAY still have applied. The dedup-safe retry therefore reuses the ORIGINAL
//     command id: the desktop dedups by id and re-emits the original outcome
//     (a `duplicate` ack) if it already applied, or applies it fresh if it did
//     not. Reusing the id can never double-apply.
//   - An explicit `rejected` / `failed` OUTCOME (`.delivered(.rejected/.failed)`)
//     is a definitive desktop-side negative we *did* observe. Retrying the same
//     id would only earn another `duplicate`/rejection, so a retry here is a
//     genuinely new attempt and must mint a NEW command id.
//

import Foundation

/// Display state of an optimistic outgoing message (reply / follow-up).
enum OutgoingState: Equatable, Sendable {
    /// Optimistically appended; awaiting the desktop's ack.
    case sending
    /// The desktop accepted / applied it (or acked a dedup `duplicate`).
    case sent
    /// Not delivered — the row shows "not delivered — retry". `retryReusesId`
    /// records whether a retry should reuse the original command id (§5.8).
    case failed(reason: String, retryReusesId: Bool)
}

/// What the phone answered a permission/question prompt with. Generalizes the
/// old binary-only `PermissionChoice` payload so the action-state machine
/// (and the card that renders it) can show *what* was answered — a binary
/// choice, a selected N-option, or a typed free-text reply — not just
/// Allowed/Denied.
enum PermissionAnswer: Equatable, Sendable {
    /// The binary fast-path (permission prompts).
    case choice(Wire.PermissionChoice)
    /// A selected Question option, by index — `label` is carried along purely
    /// for display (the wire only sends the index).
    case option(index: Int, label: String)
    /// The selected options of a multi-select (checklist) Question, by index —
    /// `labels` (in the same order as `indices`) are carried for display only.
    case options(indices: [Int], labels: [String])
    /// A typed "Type your own answer" reply.
    case freeText(String)

    /// A short human-readable resolved label, e.g. for "Allowed ✓" / "Answered
    /// “Postgres” ✓" lines.
    var resolvedText: String {
        switch self {
        case let .choice(choice):
            return choice == .allowOnce ? "Allowed ✓" : "Denied ✕"
        case let .option(_, label):
            return "Answered “\(label)” ✓"
        case let .options(_, labels):
            return "Answered “\(labels.joined(separator: ", "))” ✓"
        case let .freeText(text):
            return "Answered “\(text)” ✓"
        }
    }
}

/// Display state of an inline permission/question decision.
enum PermissionActionState: Equatable, Sendable {
    /// No decision in flight — the options are live (if the prompt is current).
    case idle
    /// A decision was tapped/typed and is awaiting the desktop ack (spinner on
    /// the chosen option); `answer` is what the user picked/typed.
    case sending(PermissionAnswer)
    /// The desktop applied the decision — the card collapses to a muted result.
    case resolved(PermissionAnswer)
    /// The desktop rejected the decision because the prompt was already answered
    /// there (stale) — an honest inline note, no retry.
    case stale
    /// The decision was not delivered — a retry affordance is offered.
    case failed(reason: String, answer: PermissionAnswer, retryReusesId: Bool)
}

/// An optimistically-appended outgoing user message. Held locally by
/// `ChatViewModel` and merged into the rendered transcript until the desktop's
/// authoritative feed echoes it back (see `isReconciled`).
struct OutgoingMessage: Identifiable, Equatable, Sendable {
    /// Local render id (never a server item id) — e.g. `local-<token>`.
    let localId: Wire.ItemId
    /// The prose the user sent.
    var text: String
    /// Phone wall-clock time (unix ms) the send was issued.
    let issuedAtMs: Int64
    /// The command id currently tracking this message's delivery. Changes only
    /// on a new-id retry (see `ChatSendLogic.retryReusesId`).
    var commandId: Wire.CommandId
    /// Current display state.
    var state: OutgoingState

    var id: String { localId.rawValue }
}

/// Namespace for the send / permission / reconciliation state machine.
enum ChatSendLogic {

    /// The default "not delivered" reason shown when only an outcome is known.
    static let notDeliveredReason = "not delivered"

    /// Clock-skew slack when matching an optimistic message against the
    /// desktop's echoed transcript item (5 min).
    static let reconciliationSlackMs: Int64 = 5 * 60 * 1000

    // MARK: - Delivery → display state

    /// Map a command's delivery state to the outgoing-message display state.
    static func outgoingState(for delivery: CommandDeliveryState) -> OutgoingState {
        switch delivery {
        case .sending:
            return .sending
        case let .delivered(outcome):
            switch outcome {
            case .accepted, .applied, .duplicate:
                return .sent
            case .rejected, .failed:
                return .failed(reason: notDeliveredReason,
                               retryReusesId: retryReusesId(for: delivery))
            }
        case let .failed(reason):
            return .failed(reason: reason, retryReusesId: retryReusesId(for: delivery))
        }
    }

    /// Map a command's delivery state to a permission-action display state,
    /// given the `answer` the user picked/typed.
    static func permissionState(for delivery: CommandDeliveryState,
                                answer: PermissionAnswer) -> PermissionActionState {
        switch delivery {
        case .sending:
            return .sending(answer)
        case let .delivered(outcome):
            switch outcome {
            case .accepted, .applied, .duplicate:
                return .resolved(answer)
            case .rejected:
                // Validated against the *current* pending prompt on the desktop;
                // a rejected decision means the prompt was already answered there.
                return .stale
            case .failed:
                return .failed(reason: notDeliveredReason, answer: answer,
                               retryReusesId: retryReusesId(for: delivery))
            }
        case let .failed(reason):
            return .failed(reason: reason, answer: answer,
                           retryReusesId: retryReusesId(for: delivery))
        }
    }

    // MARK: - Retry id semantics

    /// Whether a retry after `delivery` should reuse the original command id.
    /// See the file header for the full rationale.
    static func retryReusesId(for delivery: CommandDeliveryState) -> Bool {
        switch delivery {
        case .failed:
            // Never observed an ack — the command may have applied. Reuse the id
            // so the desktop dedups (idempotent, cannot double-apply).
            return true
        case let .delivered(outcome):
            switch outcome {
            case .rejected, .failed:
                // Observed a definitive negative — a retry is a fresh attempt.
                return false
            case .accepted, .applied, .duplicate:
                // Not a failure — retry is not applicable; default to a fresh id.
                return false
            }
        case .sending:
            return false
        }
    }

    // MARK: - Optimistic reconciliation

    /// Whether an optimistic outgoing message has been echoed back by the
    /// desktop's authoritative transcript, and so should be hidden to avoid a
    /// duplicate row.
    ///
    /// Heuristic: an authoritative `.userMessage` reconciles the optimistic copy
    /// when its text matches (whitespace-trimmed) AND its timestamp is at or
    /// after the optimistic issue time (minus a clock-skew slack). The desktop
    /// echoes the exact prose we sent, so text is the anchor; the time guard
    /// stops a much-older identical message from swallowing a fresh send. We
    /// deliberately do NOT gate on the send having reached `.sent`: the trigger
    /// is the echo arriving, so a delivered-but-not-yet-acked message still
    /// dedupes the instant the transcript feed carries it, while a truly
    /// undelivered (failed) message — which never gets echoed — stays visible
    /// with its retry affordance.
    static func isReconciled(_ outgoing: OutgoingMessage,
                             against items: [Wire.TranscriptItem],
                             slackMs: Int64 = reconciliationSlackMs) -> Bool {
        let wanted = outgoing.text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !wanted.isEmpty else { return false }
        for item in items {
            guard case let .userMessage(_, text, atMs) = item else { continue }
            let candidate = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if candidate == wanted, atMs >= outgoing.issuedAtMs - slackMs {
                return true
            }
        }
        return false
    }

    /// The subset of `outgoing` that has NOT yet been reconciled against the
    /// authoritative transcript — i.e. the optimistic rows still worth showing.
    static func visibleOutgoing(_ outgoing: [OutgoingMessage],
                                against items: [Wire.TranscriptItem],
                                slackMs: Int64 = reconciliationSlackMs) -> [OutgoingMessage] {
        outgoing.filter { !isReconciled($0, against: items, slackMs: slackMs) }
    }
}
