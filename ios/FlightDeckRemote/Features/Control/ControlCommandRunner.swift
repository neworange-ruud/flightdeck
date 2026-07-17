//
//  ControlCommandRunner.swift
//  FlightDeckRemote
//
//  The one-in-flight command helper every Tier-2 control action shares
//  (PRD §5.6/§5.8): send a `Wire.CommandBody` → track the returned
//  `CommandHandle`'s delivery honestly → expose a single observable
//  `ControlActionPhase` a view can render as spinner / applied ✓ / rejected
//  (verbatim desktop reason) / "not delivered — retry".
//
//  Retry command-id semantics mirror `ChatSendLogic` exactly (the crux of
//  PRD §5.8 delivery honesty):
//   - a transport-level `.failed` (timeout / link down / peer unavailable)
//     means we never saw the desktop's ack — the command MAY have applied, so
//     a retry REUSES the original command id (the desktop dedups by id;
//     reusing can never double-apply);
//   - a delivered `rejected`/`failed` OUTCOME is a definitive desktop-side
//     negative we observed — a retry is a genuinely new attempt and mints a
//     NEW id.
//
//  Chat-compose adoption note: `CommandRunner` deliberately depends only on
//  `ControlCommandSending` (a protocol `TransportStore` satisfies for free
//  via its existing public `sendCommand` API — no Transport changes) plus an
//  `isPaused` closure. Chat's compose task tracks *multiple* concurrent
//  optimistic messages, so it keeps its own per-message bookkeeping today,
//  but each per-command track/retry loop there is the same machine as this
//  one — compose can adopt a `CommandRunner`-per-outgoing-message later
//  without any transport change.
//

import Foundation
import Observation

// MARK: - Send seam

/// The minimal command-send surface the Control feature needs from the
/// transport. `TransportStore` conforms below via its existing public API;
/// unit tests / DEBUG fixture paths inject a scripted fake.
@MainActor
protocol ControlCommandSending: AnyObject {
    /// Seal and send `body`. When `commandId` is nil a fresh id is minted;
    /// when provided, that id is reused (idempotent retry — the desktop
    /// dedups by command id). Returns the live `CommandHandle` whose
    /// `delivery`/`ackMessage` the caller observes.
    @discardableResult
    func sendControlCommand(_ body: Wire.CommandBody,
                            commandId: Wire.CommandId?) -> CommandHandle
}

extension TransportStore: ControlCommandSending {
    @discardableResult
    func sendControlCommand(_ body: Wire.CommandBody,
                            commandId: Wire.CommandId?) -> CommandHandle {
        if let commandId { return sendCommand(body, commandId: commandId) }
        return sendCommand(body)
    }
}

// MARK: - Phase

/// The single observable display state of a control action.
enum ControlActionPhase: Equatable, Sendable {
    /// Nothing in flight; no outcome to show.
    case idle
    /// Sent; awaiting the desktop's ack (spinner).
    case inFlight
    /// The desktop accepted/applied it. `detail` is the ack's verbatim result
    /// note when present (e.g. close-session's "Stopping agent…" honesty).
    case succeeded(detail: String?)
    /// The desktop refused, for a stated reason — shown verbatim.
    case rejected(reason: String)
    /// Not delivered, or attempted-and-failed on the desktop. The UI offers
    /// retry; `retryReusesId` records the §5.8 id-reuse rule for it.
    case failed(reason: String, retryReusesId: Bool)

    /// Pure mapping from a command's delivery state (+ the ack's verbatim
    /// message, when one arrived) to the display phase. Unit-tested directly.
    static func from(delivery: CommandDeliveryState,
                     ackMessage: String?) -> ControlActionPhase {
        switch delivery {
        case .sending:
            return .inFlight
        case let .delivered(outcome):
            switch outcome {
            case .accepted, .applied, .duplicate:
                return .succeeded(detail: ackMessage)
            case .rejected:
                // Definitive desktop refusal — show its exact reason.
                return .rejected(reason: ackMessage ?? "rejected by desktop")
            case .failed:
                // Attempted on the desktop and failed (e.g. merge conflict).
                // Observed negative → a retry is a fresh attempt (new id).
                return .failed(reason: ackMessage ?? "failed on desktop",
                               retryReusesId: false)
            }
        case let .failed(reason):
            // Never saw an ack — may have applied; retry reuses the id.
            return .failed(reason: reason, retryReusesId: true)
        }
    }
}

// MARK: - Runner

/// Runs one control command at a time: `run` sends and tracks, `retry`
/// re-sends a failed command with the correct id semantics, `reset` clears
/// the outcome. Owns no UI — views render `phase`.
@MainActor
@Observable
final class CommandRunner {

    /// The current display phase (observable).
    private(set) var phase: ControlActionPhase = .idle
    /// The body of the command currently tracked (drives in-flight labels
    /// and retries).
    private(set) var currentBody: Wire.CommandBody?

    private let sender: any ControlCommandSending
    /// Visible commands-paused gate (PRD §8): when paused, `run`/`retry`
    /// refuse to send — nothing goes out blind.
    private let isPaused: () -> Bool
    private var handle: CommandHandle?

    init(sender: any ControlCommandSending,
         isPaused: @escaping () -> Bool = { false }) {
        self.sender = sender
        self.isPaused = isPaused
    }

    /// Send `body` (fresh command id). Refused while paused or while another
    /// command is still in flight. Returns whether the send was issued.
    @discardableResult
    func run(_ body: Wire.CommandBody) -> Bool {
        guard !isPaused(), phase != .inFlight else { return false }
        currentBody = body
        issue(body, commandId: nil)
        return true
    }

    /// Retry the failed current command, reusing the original command id when
    /// the failure was transport-level (dedup-safe), else minting a new id.
    @discardableResult
    func retry() -> Bool {
        guard case let .failed(_, reusesId) = phase,
              let body = currentBody, !isPaused() else { return false }
        issue(body, commandId: reusesId ? handle?.commandId : nil)
        return true
    }

    /// Clear the tracked command and outcome (back to `.idle`).
    func reset() {
        handle = nil
        currentBody = nil
        phase = .idle
    }

    /// Fold a delivery state (+ verbatim ack message) into `phase`. Exposed
    /// (not private) so the phase machine is unit-testable without a live
    /// handle-observation loop.
    func apply(delivery: CommandDeliveryState, ackMessage: String?) {
        phase = .from(delivery: delivery, ackMessage: ackMessage)
    }

    private func issue(_ body: Wire.CommandBody, commandId: Wire.CommandId?) {
        let h = sender.sendControlCommand(body, commandId: commandId)
        handle = h
        apply(delivery: h.delivery, ackMessage: h.ackMessage)
        track(h)
    }

    /// Observe the handle's `delivery` + `ackMessage` and fold each change
    /// back into `phase`. Re-arms on every change (Observation fires once).
    private func track(_ h: CommandHandle) {
        withObservationTracking {
            _ = h.delivery
            _ = h.ackMessage
        } onChange: { [weak self, weak h] in
            Task { @MainActor in
                guard let self, let h, h === self.handle else { return }
                self.apply(delivery: h.delivery, ackMessage: h.ackMessage)
                self.track(h)
            }
        }
    }
}

// MARK: - Fallbacks / DEBUG seams

/// Sender used when a surface has no live `TransportStore` (e.g. the chat
/// header before Chat's own store wiring lands): every send immediately
/// reports an honest "not connected" failure — nothing pretends to deliver.
@MainActor
final class UnavailableControlCommandSender: ControlCommandSending {
    @discardableResult
    func sendControlCommand(_ body: Wire.CommandBody,
                            commandId: Wire.CommandId?) -> CommandHandle {
        CommandHandle(commandId: commandId ?? Wire.CommandId("cmd_unavailable"),
                      body: body,
                      delivery: .failed(reason: "not connected"))
    }
}

#if DEBUG
/// DEBUG-only scripted sender (mirrors `ScriptedChatCommandSender`): hands
/// back handles left `.sending` so tests can observe the in-flight state and
/// advance delivery manually via `resolve`.
@MainActor
final class ScriptedControlCommandSender: ControlCommandSending {
    /// Every handle produced, in send order (for assertions).
    private(set) var handles: [CommandHandle] = []
    /// The sends issued, pairing the id used with the body (records id reuse).
    private(set) var sends: [(commandId: Wire.CommandId, body: Wire.CommandBody)] = []

    private var counter = 0

    @discardableResult
    func sendControlCommand(_ body: Wire.CommandBody,
                            commandId: Wire.CommandId?) -> CommandHandle {
        let id = commandId ?? nextId()
        let handle: CommandHandle
        if let existing = handles.first(where: { $0.commandId == id }) {
            existing.delivery = .sending
            handle = existing
        } else {
            handle = CommandHandle(commandId: id, body: body)
            handles.append(handle)
        }
        sends.append((commandId: id, body: body))
        return handle
    }

    /// Advance a tracked handle's delivery (and optional verbatim ack message).
    func resolve(_ commandId: Wire.CommandId, with state: CommandDeliveryState,
                 ackMessage: String? = nil) {
        guard let handle = handles.first(where: { $0.commandId == commandId }) else { return }
        if let ackMessage { handle.ackMessage = ackMessage }
        handle.delivery = state
    }

    private func nextId() -> Wire.CommandId {
        counter += 1
        return Wire.CommandId("scripted_ctl_\(counter)")
    }
}
#endif
