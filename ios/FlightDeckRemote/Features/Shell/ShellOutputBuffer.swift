//
//  ShellOutputBuffer.swift
//  FlightDeckRemote
//
//  Reassembles `ShellOutput` chunks into an in-order stream (PRD §5.4). Each
//  chunk carries a monotonic per-shell `seq` starting at 1 (E2E.swift). The
//  E2E channel over the relay is ordered and reliable, so in practice chunks
//  arrive 1,2,3,…; this buffer exists to make that *robust* rather than
//  assumed:
//
//   * duplicates / already-emitted seqs (`seq < nextSeq`) are dropped —
//     idempotent, so re-ingesting the store's whole chunk array is safe;
//   * an out-of-order chunk (`seq > nextSeq`) is held in `pending` until the
//     gap ahead of it fills, then flushed in seq order;
//   * `flushPending()` force-emits everything buffered in seq order, used when
//     the terminal is torn down / reopened and any lingering gap will never be
//     filled (better to show the tail than to strand it).
//
//  Pure value type — no view or transport — so reordering, dedup, and gap
//  handling are all unit-tested directly.
//

import Foundation

/// Ordered, gap-tolerant reassembly of per-shell output chunks.
struct ShellOutputBuffer: Equatable {

    /// The next seq expected in order (chunks number from 1).
    private(set) var nextSeq: UInt64 = 1

    /// Chunks emitted in order so far, ready to feed the renderer.
    private(set) var ordered: [String] = []

    /// Held out-of-order chunks, keyed by seq, awaiting the gap ahead to fill.
    private var pending: [UInt64: String] = [:]

    init() {}

    /// True while at least one chunk is buffered waiting on a gap.
    var hasPending: Bool { !pending.isEmpty }

    /// Ingest one chunk. Returns the chunks newly emitted in order by this call
    /// (may be empty if it filled a gap that's still incomplete, or was a
    /// duplicate).
    @discardableResult
    mutating func ingest(seq: UInt64, data: String) -> [String] {
        // Already emitted (duplicate or stale replay) — drop idempotently.
        guard seq >= nextSeq else { return [] }
        pending[seq] = data
        return flush()
    }

    /// Emit every buffered chunk in seq order regardless of gaps. Use on
    /// teardown/reopen when remaining gaps can no longer be filled.
    @discardableResult
    mutating func flushPending() -> [String] {
        let remaining = pending.keys.sorted()
        var emitted: [String] = []
        for seq in remaining {
            if let data = pending.removeValue(forKey: seq) {
                ordered.append(data)
                emitted.append(data)
                nextSeq = max(nextSeq, seq + 1)
            }
        }
        return emitted
    }

    /// Drain the contiguous run starting at `nextSeq`.
    private mutating func flush() -> [String] {
        var emitted: [String] = []
        while let data = pending.removeValue(forKey: nextSeq) {
            ordered.append(data)
            emitted.append(data)
            nextSeq += 1
        }
        return emitted
    }
}
