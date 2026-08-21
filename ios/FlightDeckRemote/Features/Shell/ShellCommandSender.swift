//
//  ShellCommandSender.swift
//  FlightDeckRemote
//
//  The send seam for the shell terminal (open / input / interrupt / close),
//  mirroring `ChatCommandSending`. It abstracts the transport's shell command
//  API behind a tiny protocol so the `ShellSessionModel` state machine can be
//  unit-tested against a fake, and so the DEBUG fixture / UI-test path can
//  drive sends deterministically without a live relay.
//
//  `TransportStore` conforms for free via its additive shell command funcs.
//

import Foundation

/// The minimal shell command surface the Shell feature needs from the transport.
@MainActor
protocol ShellCommandSending: AnyObject {
    /// Open a shell in the session's worktree with a fitted geometry.
    @discardableResult
    func openShell(sessionId: Wire.SessionId, shellId: Wire.ShellId,
                   cols: UInt16, rows: UInt16) -> CommandHandle
    /// Send input bytes (as the `data` string) to a live shell.
    @discardableResult
    func sendShellInput(sessionId: Wire.SessionId, shellId: Wire.ShellId,
                        data: String) -> CommandHandle
    /// Interrupt the foreground process (Ctrl-C) via command — works even when
    /// the PTY is wedged (unlike sending a raw `0x03`).
    @discardableResult
    func interruptShell(sessionId: Wire.SessionId, shellId: Wire.ShellId) -> CommandHandle
    /// Close the shell.
    @discardableResult
    func closeShell(sessionId: Wire.SessionId, shellId: Wire.ShellId) -> CommandHandle
}

extension TransportStore: ShellCommandSending {}

#if DEBUG
/// DEBUG-only scripted shell sender for the fixture / UI-test path. Records
/// every send so tests (and a hidden UI-test debug label) can assert what the
/// key bar produced, without a relay. Handles are left `.sending`.
@MainActor
final class ScriptedShellCommandSender: ShellCommandSending {

    /// A recorded send, in order.
    enum Sent: Equatable {
        case open(cols: UInt16, rows: UInt16)
        case input(String)
        case interrupt
        case close
    }

    private(set) var sends: [Sent] = []
    /// Every handle produced, in send order (tests advance `delivery` to
    /// script acks — e.g. the "already open" rejection).
    private(set) var handles: [CommandHandle] = []
    private var counter = 0

    /// A compact description of the last send, for the UI-test debug label
    /// (`shell-debug-last-sent`). Input bytes render as hex (`input:03`).
    var lastSentDescription: String {
        guard let last = sends.last else { return "none" }
        switch last {
        case let .open(cols, rows): return "open:\(cols)x\(rows)"
        case let .input(data):
            let hex = Array(data.utf8).map { String(format: "%02x", $0) }.joined()
            return "input:\(hex)"
        case .interrupt: return "interrupt"
        case .close: return "close"
        }
    }

    @discardableResult
    func openShell(sessionId: Wire.SessionId, shellId: Wire.ShellId,
                   cols: UInt16, rows: UInt16) -> CommandHandle {
        sends.append(.open(cols: cols, rows: rows))
        return handle(.shellOpen(sessionId: sessionId, shellId: shellId, cols: cols, rows: rows))
    }

    @discardableResult
    func sendShellInput(sessionId: Wire.SessionId, shellId: Wire.ShellId,
                        data: String) -> CommandHandle {
        sends.append(.input(data))
        return handle(.shellInput(sessionId: sessionId, shellId: shellId, data: data))
    }

    @discardableResult
    func interruptShell(sessionId: Wire.SessionId, shellId: Wire.ShellId) -> CommandHandle {
        sends.append(.interrupt)
        return handle(.shellInterrupt(sessionId: sessionId, shellId: shellId))
    }

    @discardableResult
    func closeShell(sessionId: Wire.SessionId, shellId: Wire.ShellId) -> CommandHandle {
        sends.append(.close)
        return handle(.shellClose(sessionId: sessionId, shellId: shellId))
    }

    private func handle(_ body: Wire.CommandBody) -> CommandHandle {
        counter += 1
        let handle = CommandHandle(commandId: Wire.CommandId("scripted_shell_\(counter)"), body: body)
        handles.append(handle)
        return handle
    }
}

/// DEBUG-only trivial `ConnectionStatusSource` for the shell fixture path,
/// where no live `TransportStore` is bound. Under `-uitest-linkstate` the
/// gate's forced state wins, so this only needs a safe default otherwise.
@MainActor
final class ShellFixtureConnectionSource: ConnectionStatusSource {
    var linkState: RemoteLinkState
    var peerConnected: Bool?
    init(linkState: RemoteLinkState = .connected(latencyMs: 8)) {
        self.linkState = linkState
    }
}
#endif
