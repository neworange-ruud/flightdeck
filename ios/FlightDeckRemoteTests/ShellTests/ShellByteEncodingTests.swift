//
//  ShellByteEncodingTests.swift
//  FlightDeckRemoteTests
//
//  Byte-level mapping for the shell key bar / input (PRD §5.4): the special
//  keys, arrow CSI sequences, and the sticky-`Ctrl` control composition.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@Suite struct ShellByteEncodingTests {

    // MARK: - Single keys

    @Test func escapeIsEsc() {
        #expect(ShellByteEncoding.escape == [0x1b])
    }

    @Test func tabIsHT() {
        #expect(ShellByteEncoding.tab == [0x09])
    }

    @Test func carriageReturnIsCR() {
        #expect(ShellByteEncoding.carriageReturn == [0x0d])
    }

    // MARK: - Arrows (CSI)

    @Test func arrowsAreCSISequences() {
        #expect(ShellByteEncoding.arrowUp == [0x1b, 0x5b, 0x41])    // ESC [ A
        #expect(ShellByteEncoding.arrowDown == [0x1b, 0x5b, 0x42])  // ESC [ B
        #expect(ShellByteEncoding.arrowRight == [0x1b, 0x5b, 0x43]) // ESC [ C
        #expect(ShellByteEncoding.arrowLeft == [0x1b, 0x5b, 0x44])  // ESC [ D
    }

    // MARK: - Control composition

    @Test func controlCComposesToETX() {
        #expect(ShellByteEncoding.applyControl(to: ShellByteEncoding.bytes(for: "c")) == [0x03])
        #expect(ShellByteEncoding.control(for: "c") == 0x03)
    }

    @Test func controlDAndControlZ() {
        #expect(ShellByteEncoding.control(for: "d") == 0x04)
        #expect(ShellByteEncoding.control(for: "z") == 0x1a)
    }

    @Test func controlUppercaseMatchesLowercase() {
        // 'C' (0x43) & 0x1f == 0x03, same as 'c'.
        #expect(ShellByteEncoding.control(for: "C") == 0x03)
    }

    @Test func controlPassesThroughMultiByteSequences() {
        // Ctrl + an arrow key has no portable meaning → send the arrow as-is.
        #expect(ShellByteEncoding.applyControl(to: ShellByteEncoding.arrowUp) == ShellByteEncoding.arrowUp)
    }

    @Test func controlPassesThroughEmpty() {
        #expect(ShellByteEncoding.applyControl(to: []) == [])
    }

    @Test func controlOfNonPrintableReturnsNil() {
        #expect(ShellByteEncoding.control(for: "é") == nil)
    }

    // MARK: - Text + wire round trip

    @Test func textBytesAreUTF8() {
        #expect(ShellByteEncoding.bytes(for: "|") == [0x7c])
        #expect(ShellByteEncoding.bytes(for: "~") == [0x7e])
        #expect(ShellByteEncoding.bytes(for: "`") == [0x60])
    }

    @Test func wireStringRoundTripsControlBytes() {
        let bytes: [UInt8] = [0x03, 0x1b, 0x5b, 0x41]
        let wire = ShellByteEncoding.wireString(bytes)
        #expect(Array(wire.utf8) == bytes)
    }
}
