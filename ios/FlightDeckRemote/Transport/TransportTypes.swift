//
//  TransportTypes.swift
//  FlightDeckRemote
//
//  Shared value types for the relay transport: the app-facing link state, the
//  delivery-honesty state of a sent command (PRD §5.8), the internal event
//  stream `TransportClient` publishes to `TransportStore`, and the pure
//  reconnect backoff schedule (mirrors the desktop `src/remote/client.rs`).
//

import Foundation

/// The relay connection state, surfaced to the UI (mirrors the desktop's
/// `RemoteLinkState`). `connected` carries the last measured phone↔relay
/// round-trip latency (0 until the first pong).
enum RemoteLinkState: Equatable, Sendable {
    /// Not connected (idle, or between reconnect attempts).
    case disconnected
    /// A TCP/WebSocket connection attempt is in progress.
    case connecting
    /// Connected; running the hello → auth handshake.
    case authenticating
    /// Authenticated and live.
    case connected(latencyMs: Int)
}

/// The delivery-honesty state of a phone command (REMOTE_PROTOCOL §6.5,
/// PRD §5.8). "Delivered" means the *desktop* acked at the application layer —
/// not merely that the relay accepted the envelope.
enum CommandDeliveryState: Equatable, Sendable {
    /// Sealed and handed to the relay; awaiting the desktop's `command_ack`.
    case sending
    /// The desktop acked with this outcome.
    case delivered(Wire.CommandOutcome)
    /// Not delivered — the UI shows "not delivered — retry" (§6.5). Carries an
    /// honest reason (timeout, link down, peer unavailable, seal/send error).
    case failed(reason: String)
}

/// An event published by `TransportClient` to its observer (`TransportStore`).
enum TransportEvent: Sendable {
    /// The link state changed.
    case link(RemoteLinkState)
    /// A decoded desktop→phone application message arrived (deduped, in order).
    case message(Wire.DesktopToPhone)
    /// A tracked command's delivery state changed.
    case delivery(commandId: Wire.CommandId, state: CommandDeliveryState)
    /// The peer's presence changed (drives the paused-command / reconnecting UI).
    case presence(peer: Wire.Role, connected: Bool)
}

/// The reconnect backoff schedule (REMOTE_PROTOCOL §5.3): exponential from a 1s
/// floor, capped at 60s, plus up to +25% jitter — byte-for-byte the desktop's
/// `backoff_delay`. Pure and unit-tested.
enum Backoff {
    /// Backoff floor (first retry) in milliseconds.
    static let baseMs: UInt64 = 1_000
    /// Backoff ceiling in milliseconds.
    static let capMs: UInt64 = 60_000

    /// Delay for retry `attempt` (0 = first retry). `jitterUnit` is in `[0, 1)`;
    /// the result always stays within `[1s, 60s]`.
    static func delay(attempt: Int, jitterUnit: Double) -> Duration {
        // Cap the shift so `1_000 << attempt` never overflows.
        let shift = min(max(attempt, 0), 6)
        let full = min(baseMs << shift, capMs)
        let clamped = min(max(jitterUnit, 0.0), 1.0)
        let jitter = UInt64(clamped * Double(full) * 0.25)
        let total = min(full + jitter, capMs)
        return .milliseconds(Int64(total))
    }
}
