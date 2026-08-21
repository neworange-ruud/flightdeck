//
//  ShellSessionModelTests.swift
//  FlightDeckRemoteTests
//
//  The @Observable shell model: sticky-`Ctrl` composition end-to-end, key-bar
//  byte plumbing, interrupt-via-command, paste, open/paused gating (PRD §5.4 /
//  §8). Uses the DEBUG scripted sender (records sends) + a fixture connection
//  source (drives the paused gate), no relay.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@MainActor
@Suite struct ShellSessionModelTests {

    private func makeModel(
        linkState: RemoteLinkState = .connected(latencyMs: 5)
    ) -> (ShellSessionModel, ScriptedShellCommandSender) {
        let model = ShellSessionModel(sessionId: Wire.SessionId("sess_x"),
                                      sessionName: "fix-login",
                                      idFactory: { Wire.ShellId("sh_test") })
        let sender = ScriptedShellCommandSender()
        let gate = CommandsPausedGate(source: ShellFixtureConnectionSource(linkState: linkState),
                                      launchArguments: [])
        model.configure(sender: sender, gate: gate)
        return (model, sender)
    }

    // MARK: - Open

    @Test func openSendsShellOpenWithGeometry() {
        let (model, sender) = makeModel()
        model.setGeometry(cols: 100, rows: 30)
        model.open()
        #expect(model.phase == .opening)
        #expect(model.shellId == Wire.ShellId("sh_test"))
        #expect(sender.sends == [.open(cols: 100, rows: 30)])
    }

    @Test func openIsBlockedWhilePaused() {
        let (model, sender) = makeModel(linkState: .disconnected)
        #expect(model.commandsPaused)
        model.open()
        #expect(model.phase == .noShell)
        #expect(sender.sends.isEmpty)
    }

    // MARK: - Key bar plumbing (requires a live shell)

    private func liveModel(
        linkState: RemoteLinkState = .connected(latencyMs: 5)
    ) -> (ShellSessionModel, ScriptedShellCommandSender) {
        let (model, sender) = makeModel(linkState: linkState)
        model.debugDriveFixture(shellId: Wire.ShellId("sh_test"), chunks: [])
        return (model, sender)
    }

    @Test func tabKeySendsHT() {
        let (model, sender) = liveModel()
        model.tapKey(.tab)
        #expect(sender.sends == [.input("\u{09}")])
    }

    @Test func escapeKeySendsEsc() {
        let (model, sender) = liveModel()
        model.tapKey(.escape)
        #expect(sender.sends == [.input("\u{1b}")])
    }

    @Test func arrowUpSendsCSI() {
        let (model, sender) = liveModel()
        model.tapKey(.up)
        #expect(sender.sends == [.input("\u{1b}[A")])
    }

    // MARK: - Sticky Ctrl

    @Test func ctrlArmsThenComposesAndDisarms() {
        let (model, sender) = liveModel()
        model.tapKey(.ctrl)
        #expect(model.ctrlArmed)
        // Ctrl + '/' (0x2f) → 0x0f.
        model.tapKey(.slash)
        #expect(!model.ctrlArmed)
        #expect(sender.sends == [.input("\u{0f}")])
    }

    @Test func stickyCtrlComposesKeyboardCIntoETX() {
        let (model, sender) = liveModel()
        model.tapKey(.ctrl)
        #expect(model.ctrlArmed)
        // Keyboard 'c' arrives after arming → ETX (0x03), Ctrl disarms.
        model.handleKeyboardInput([0x63])
        #expect(!model.ctrlArmed)
        #expect(sender.sends == [.input("\u{03}")])
    }

    @Test func keyboardInputUnmodifiedWhenCtrlIdle() {
        let (model, sender) = liveModel()
        model.handleKeyboardInput(Array("ls\r".utf8))
        #expect(sender.sends == [.input("ls\r")])
    }

    // MARK: - Interrupt (command, not byte)

    @Test func interruptSendsCommandNotByte() {
        let (model, sender) = liveModel()
        model.interrupt()
        #expect(sender.sends == [.interrupt])
    }

    @Test func interruptKeyRoutesToInterruptCommand() {
        let (model, sender) = liveModel()
        model.tapKey(.interrupt)
        #expect(sender.sends == [.interrupt])
    }

    // MARK: - Paste

    @Test func pasteSendsTextAsInput() {
        let (model, sender) = liveModel()
        model.paste("echo hi")
        #expect(sender.sends == [.input("echo hi")])
    }

    @Test func emptyPasteIsNoOp() {
        let (model, sender) = liveModel()
        model.paste("")
        #expect(sender.sends.isEmpty)
    }

    // MARK: - Paused gating (PRD §8: nothing sent blind)

    @Test func inputBlockedWhilePaused() {
        let (model, sender) = liveModel(linkState: .disconnected)
        #expect(model.commandsPaused)
        model.tapKey(.tab)
        model.handleKeyboardInput([0x63])
        model.interrupt()
        model.paste("x")
        #expect(sender.sends.isEmpty)
    }

    // MARK: - Interactivity gating

    @Test func inputBeforeLiveIsNoOp() {
        let (model, sender) = makeModel() // phase == .noShell
        model.tapKey(.tab)
        model.interrupt()
        #expect(sender.sends.isEmpty)
    }

    // MARK: - Open rejection (desktop "already open" contract)

    @Test func rejectedOpenAckSurfacesAlreadyOpen() {
        let (model, sender) = makeModel()
        model.open()
        #expect(model.phase == .opening)
        // The desktop rejects the second open ("already open") — the ack lands
        // on the open command's handle; the model reconciles it into the
        // honest rejected phase (with the desktop's verbatim reason).
        let handle = sender.handles[0]
        handle.ackMessage = "already open"
        handle.delivery = .delivered(.rejected)
        model.reconcileOpenDelivery()
        #expect(model.phase == .rejectedAlreadyOpen(message: "already open"))
    }

    @Test func appliedOpenAckLeavesPhaseAlone() {
        let (model, sender) = makeModel()
        model.open()
        sender.handles[0].delivery = .delivered(.applied)
        model.reconcileOpenDelivery()
        // Still opening — only the `opened` ShellEvent moves it to .live.
        #expect(model.phase == .opening)
    }

    // MARK: - Reopen resets the shell id + output

    @Test func reopenMintsFreshStateAfterExit() {
        let (model, sender) = liveModel()
        model.debugDriveFixture(shellId: Wire.ShellId("sh_test"), chunks: ["old\r\n"])
        #expect(model.orderedOutput == ["old\r\n"])
        model.reopen()
        #expect(model.phase == .opening)
        #expect(model.orderedOutput.isEmpty)
        #expect(sender.sends.contains { if case .open = $0 { return true }; return false })
    }
}
