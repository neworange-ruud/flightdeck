//
//  ShellStateMachineTests.swift
//  FlightDeckRemoteTests
//
//  The shell lifecycle: connect → live → exited → closed, plus the "already
//  open" rejection and reopen (PRD §5.4, desktop contract).
//

import Testing
@testable import FlightDeckRemote

@Suite struct ShellStateMachineTests {

    @Test func openThenOpenedGoesLive() {
        var phase = ShellPhase.noShell
        phase = ShellStateMachine.reduce(phase, .requestOpen)
        #expect(phase == .opening)
        phase = ShellStateMachine.reduce(phase, .opened)
        #expect(phase == .live)
    }

    @Test func liveThenExitedHoldsSlot() {
        var phase = ShellPhase.live
        phase = ShellStateMachine.reduce(phase, .exited(code: 0))
        #expect(phase == .exited(code: 0))
    }

    @Test func exitedThenClosed() {
        var phase = ShellPhase.exited(code: 1)
        phase = ShellStateMachine.reduce(phase, .closed)
        #expect(phase == .closed)
    }

    @Test func closedThenReopen() {
        var phase = ShellPhase.closed
        phase = ShellStateMachine.reduce(phase, .requestOpen)
        #expect(phase == .opening)
    }

    @Test func exitedThenReopen() {
        var phase = ShellPhase.exited(code: nil)
        phase = ShellStateMachine.reduce(phase, .requestOpen)
        #expect(phase == .opening)
    }

    @Test func openRejectedWhileOpening() {
        var phase = ShellPhase.opening
        phase = ShellStateMachine.reduce(phase, .openRejected(message: "already open"))
        #expect(phase == .rejectedAlreadyOpen(message: "already open"))
    }

    @Test func rejectedThenReopen() {
        var phase = ShellPhase.rejectedAlreadyOpen(message: "x")
        phase = ShellStateMachine.reduce(phase, .requestOpen)
        #expect(phase == .opening)
    }

    @Test func closedFromAnyPhase() {
        #expect(ShellStateMachine.reduce(.live, .closed) == .closed)
        #expect(ShellStateMachine.reduce(.opening, .closed) == .closed)
        #expect(ShellStateMachine.reduce(.noShell, .closed) == .closed)
    }

    @Test func exitCarriesNilCode() {
        let phase = ShellStateMachine.reduce(.live, .exited(code: nil))
        #expect(phase == .exited(code: nil))
    }

    @Test func strayOpenedWhileLiveIsNoOp() {
        #expect(ShellStateMachine.reduce(.live, .opened) == .live)
    }

    @Test func inputMappingFromEventKind() {
        #expect(ShellStateMachine.input(for: .opened(cols: 80, rows: 24)) == .opened)
        #expect(ShellStateMachine.input(for: .exited(code: 2)) == .exited(code: 2))
        #expect(ShellStateMachine.input(for: .closed) == .closed)
    }
}
