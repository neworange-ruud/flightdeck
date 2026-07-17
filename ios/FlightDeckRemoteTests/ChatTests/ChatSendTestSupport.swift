//
//  ChatSendTestSupport.swift
//  FlightDeckRemoteTests
//
//  Shared doubles for the chat send / permission view-model tests: a recording
//  `ChatCommandSending` fake and a mutable `ConnectionStatusSource`.
//

import Foundation
@testable import FlightDeckRemote

/// Records every send and hands back a controllable `CommandHandle`, so the
/// send state machine can be driven deterministically without a relay.
@MainActor
final class FakeChatSender: ChatCommandSending {
    /// The bodies sent, paired with the command id used (records id reuse).
    private(set) var sends: [(commandId: Wire.CommandId, body: Wire.CommandBody)] = []
    /// Handles produced, in send order.
    private(set) var handles: [CommandHandle] = []

    private var counter = 0

    @discardableResult
    func send(_ body: Wire.CommandBody, reusingId commandId: Wire.CommandId?) -> CommandHandle {
        let id: Wire.CommandId
        if let commandId {
            id = commandId
        } else {
            counter += 1
            id = Wire.CommandId("fake_cmd_\(counter)")
        }
        sends.append((commandId: id, body: body))
        let handle = CommandHandle(commandId: id, body: body)
        handles.append(handle)
        return handle
    }

    /// The last command id sent (for retry-id assertions).
    var lastCommandId: Wire.CommandId? { sends.last?.commandId }
}

/// A mutable connection-state source for building a `CommandsPausedGate`.
@MainActor
final class FakeConnectionSource: ConnectionStatusSource {
    var linkState: RemoteLinkState
    init(_ linkState: RemoteLinkState = .connected(latencyMs: 5)) {
        self.linkState = linkState
    }
}
