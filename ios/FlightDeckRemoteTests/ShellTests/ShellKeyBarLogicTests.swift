//
//  ShellKeyBarLogicTests.swift
//  FlightDeckRemoteTests
//
//  Key bar tap → action mapping, and the sticky-`Ctrl` composition rule
//  (PRD §5.4).
//

import Testing
@testable import FlightDeckRemote

@Suite struct ShellKeyBarLogicTests {

    // MARK: - Plain key → bytes

    @Test func escapeSendsEsc() {
        #expect(ShellKeyBarLogic.action(for: .escape, ctrlArmed: false)
                == .sendBytes([0x1b], ctrlConsumed: false))
    }

    @Test func tabSendsHT() {
        #expect(ShellKeyBarLogic.action(for: .tab, ctrlArmed: false)
                == .sendBytes([0x09], ctrlConsumed: false))
    }

    @Test func arrowsSendCSI() {
        #expect(ShellKeyBarLogic.action(for: .up, ctrlArmed: false)
                == .sendBytes([0x1b, 0x5b, 0x41], ctrlConsumed: false))
        #expect(ShellKeyBarLogic.action(for: .down, ctrlArmed: false)
                == .sendBytes([0x1b, 0x5b, 0x42], ctrlConsumed: false))
        #expect(ShellKeyBarLogic.action(for: .right, ctrlArmed: false)
                == .sendBytes([0x1b, 0x5b, 0x43], ctrlConsumed: false))
        #expect(ShellKeyBarLogic.action(for: .left, ctrlArmed: false)
                == .sendBytes([0x1b, 0x5b, 0x44], ctrlConsumed: false))
    }

    @Test func symbolsSendTheirByte() {
        #expect(ShellKeyBarLogic.action(for: .pipe, ctrlArmed: false)
                == .sendBytes([0x7c], ctrlConsumed: false))
        #expect(ShellKeyBarLogic.action(for: .slash, ctrlArmed: false)
                == .sendBytes([0x2f], ctrlConsumed: false))
        #expect(ShellKeyBarLogic.action(for: .dash, ctrlArmed: false)
                == .sendBytes([0x2d], ctrlConsumed: false))
        #expect(ShellKeyBarLogic.action(for: .tilde, ctrlArmed: false)
                == .sendBytes([0x7e], ctrlConsumed: false))
        #expect(ShellKeyBarLogic.action(for: .backtick, ctrlArmed: false)
                == .sendBytes([0x60], ctrlConsumed: false))
    }

    // MARK: - Special keys

    @Test func ctrlTogglesModifier() {
        #expect(ShellKeyBarLogic.action(for: .ctrl, ctrlArmed: false) == .toggleCtrl)
        #expect(ShellKeyBarLogic.action(for: .ctrl, ctrlArmed: true) == .toggleCtrl)
    }

    @Test func interruptKeyFiresInterruptCommand() {
        #expect(ShellKeyBarLogic.action(for: .interrupt, ctrlArmed: false) == .interrupt)
    }

    @Test func pasteKeyPastes() {
        #expect(ShellKeyBarLogic.action(for: .paste, ctrlArmed: false) == .paste)
    }

    // MARK: - Sticky Ctrl composition

    @Test func armedCtrlComposesSymbolIntoControlByte() {
        // Ctrl + `/` (0x2f) → 0x0f, and Ctrl is consumed.
        #expect(ShellKeyBarLogic.action(for: .slash, ctrlArmed: true)
                == .sendBytes([0x0f], ctrlConsumed: true))
    }

    @Test func armedCtrlPassesThroughArrows() {
        // Ctrl + arrow has no portable control byte → arrow as-is, Ctrl consumed.
        #expect(ShellKeyBarLogic.action(for: .up, ctrlArmed: true)
                == .sendBytes([0x1b, 0x5b, 0x41], ctrlConsumed: true))
    }

    // MARK: - Keyboard input composition

    @Test func keyboardInputUnmodifiedWhenCtrlIdle() {
        let out = ShellKeyBarLogic.keyboardInput([0x63], ctrlArmed: false)
        #expect(out.bytes == [0x63])
        #expect(out.ctrlConsumed == false)
    }

    @Test func keyboardCtrlCComposesToETX() {
        // Arm Ctrl, then type 'c' (0x63) on the keyboard → 0x03, Ctrl consumed.
        let out = ShellKeyBarLogic.keyboardInput([0x63], ctrlArmed: true)
        #expect(out.bytes == [0x03])
        #expect(out.ctrlConsumed == true)
    }
}
