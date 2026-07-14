//
//  ChatCommandSender.swift
//  FlightDeckRemote
//
//  The send seam for the agent chat (compose reply + permission decision). It
//  abstracts `TransportStore`'s command API behind a tiny protocol so the send
//  state machine can be unit-tested against a fake, and so the DEBUG fixture /
//  UI-test path can drive `CommandHandle` delivery deterministically without a
//  live relay (the real store needs a `TransportClient` wired to identity,
//  keychain and a socket).
//
//  `send(_:reusingId:)` is the one primitive the view-model needs: mint a fresh
//  command id, or reuse a prior id for a dedup-safe retry (PRD §5.8, see
//  `ChatSendLogic`). `TransportStore` conforms via its existing public command
//  API — the additive `sendCommand(_:commandId:)` overload it grew for the
//  id-reuse case is the *only* Chat-driven addition to the transport facade.
//

import Foundation

/// The minimal command-send surface the Chat feature needs from the transport.
/// `TransportStore` conforms for free below; unit tests inject a fake.
@MainActor
protocol ChatCommandSending: AnyObject {
    /// Seal and send `body`. When `commandId` is `nil` a fresh id is minted;
    /// when provided, that id is reused (idempotent retry — the desktop dedups
    /// by command id). Returns the live `CommandHandle` whose `delivery` the
    /// caller observes for honest send feedback.
    @discardableResult
    func send(_ body: Wire.CommandBody, reusingId commandId: Wire.CommandId?) -> CommandHandle
}

extension TransportStore: ChatCommandSending {
    @discardableResult
    func send(_ body: Wire.CommandBody, reusingId commandId: Wire.CommandId?) -> CommandHandle {
        if let commandId {
            return sendCommand(body, commandId: commandId)
        }
        return sendCommand(body)
    }
}

#if DEBUG
/// DEBUG-only scripted sender for the fixture / UI-test path. It never touches a
/// relay: it just hands back `CommandHandle`s left in `.sending` so a UI test can
/// observe the optimistic pending / spinner states. Delivery can be advanced
/// manually (`resolve(_:with:)`) for scripted flows.
@MainActor
final class ScriptedChatCommandSender: ChatCommandSending {
    /// Every handle this sender has produced, in send order (for assertions).
    private(set) var handles: [CommandHandle] = []
    /// The bodies sent, paired with the id used (records id-reuse on retry).
    private(set) var sends: [(commandId: Wire.CommandId, body: Wire.CommandBody)] = []

    private var counter = 0

    @discardableResult
    func send(_ body: Wire.CommandBody, reusingId commandId: Wire.CommandId?) -> CommandHandle {
        let id = commandId ?? nextId()
        // Reuse the existing handle object when retrying the same id so an
        // observer keeps watching one handle across a same-id retry.
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

    /// Advance a tracked handle's delivery (scripted flows / tests).
    func resolve(_ commandId: Wire.CommandId, with state: CommandDeliveryState) {
        handles.first(where: { $0.commandId == commandId })?.delivery = state
    }

    private func nextId() -> Wire.CommandId {
        counter += 1
        return Wire.CommandId("scripted_cmd_\(counter)")
    }
}

/// DEBUG-only trivial `ConnectionStatusSource` for the fixture path, where no
/// live `TransportStore` is bound. Under `-uitest-linkstate` the
/// `CommandsPausedGate`'s forced state wins regardless of this value, so it only
/// needs to be a safe default (paused) otherwise.
@MainActor
final class FixtureConnectionSource: ConnectionStatusSource {
    var linkState: RemoteLinkState = .disconnected
}
#endif
