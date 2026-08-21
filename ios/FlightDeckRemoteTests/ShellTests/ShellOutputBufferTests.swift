//
//  ShellOutputBufferTests.swift
//  FlightDeckRemoteTests
//
//  Chunk seq ordering + gap tolerance / out-of-order reassembly (PRD §5.4).
//

import Testing
@testable import FlightDeckRemote

@Suite struct ShellOutputBufferTests {

    @Test func inOrderChunksEmitImmediately() {
        var buffer = ShellOutputBuffer()
        #expect(buffer.ingest(seq: 1, data: "a") == ["a"])
        #expect(buffer.ingest(seq: 2, data: "b") == ["b"])
        #expect(buffer.ingest(seq: 3, data: "c") == ["c"])
        #expect(buffer.ordered == ["a", "b", "c"])
    }

    @Test func outOfOrderIsBufferedThenFlushedInSeqOrder() {
        var buffer = ShellOutputBuffer()
        // seq 2 arrives before seq 1 → held (gap ahead not filled).
        #expect(buffer.ingest(seq: 2, data: "b") == [])
        #expect(buffer.hasPending)
        // seq 1 arrives → both flush, in seq order.
        #expect(buffer.ingest(seq: 1, data: "a") == ["a", "b"])
        #expect(buffer.ordered == ["a", "b"])
        #expect(!buffer.hasPending)
    }

    @Test func duplicateAndStaleSeqsAreDropped() {
        var buffer = ShellOutputBuffer()
        buffer.ingest(seq: 1, data: "a")
        buffer.ingest(seq: 2, data: "b")
        // Re-ingesting already-emitted seqs (e.g. re-scanning the store array)
        // is a no-op — idempotent.
        #expect(buffer.ingest(seq: 1, data: "a") == [])
        #expect(buffer.ingest(seq: 2, data: "b") == [])
        #expect(buffer.ordered == ["a", "b"])
    }

    @Test func gapIsHeldUntilFilled() {
        var buffer = ShellOutputBuffer()
        #expect(buffer.ingest(seq: 1, data: "a") == ["a"])
        // seq 3 arrives, seq 2 missing → held.
        #expect(buffer.ingest(seq: 3, data: "c") == [])
        #expect(buffer.hasPending)
        // seq 2 fills the gap → 2 and 3 flush together.
        #expect(buffer.ingest(seq: 2, data: "b") == ["b", "c"])
        #expect(buffer.ordered == ["a", "b", "c"])
    }

    @Test func flushPendingForceEmitsRemainingOnTeardown() {
        var buffer = ShellOutputBuffer()
        buffer.ingest(seq: 1, data: "a")
        buffer.ingest(seq: 3, data: "c") // gap at 2, held
        buffer.ingest(seq: 4, data: "d") // held
        // A permanent gap (teardown): force-emit the tail in seq order.
        #expect(buffer.flushPending() == ["c", "d"])
        #expect(buffer.ordered == ["a", "c", "d"])
        #expect(!buffer.hasPending)
    }

    @Test func seqStartsAtOne() {
        var buffer = ShellOutputBuffer()
        // Per the desktop contract, seq starts at 1. Emits in order from 1.
        #expect(buffer.nextSeq == 1)
        buffer.ingest(seq: 1, data: "x")
        #expect(buffer.nextSeq == 2)
    }
}
