//
//  DirectConnection.swift
//  FlightDeckRemote
//
//  The direct link to the Mac over Tailscale (REMOTE_PROTOCOL §5.10,
//  remote-control-gz0d), beside `RelayConnection`. A direct-mode pairing
//  carries its whole data plane over a WebSocket the Mac serves at
//  `ws://<host>:<port>/remote/v1` instead of through the relay; WireGuard is the
//  transport encryption and the E2E envelopes run on top unchanged.
//
//  What is shared and what differs. The Mac plays the relay's role and speaks
//  the SAME `RelayFrame` JSON, so the whole state machine — hello → auth →
//  resume → envelopes, E2E open/seal, gapless seq, cumulative ack, dedup,
//  stream-reset and seq_violation recovery, backoff, delivery honesty — is the
//  one `TransportClient` already runs for the relay. Only three things change,
//  and they are captured by `TransportLink` below rather than by a second state
//  machine:
//
//   1. the endpoint: the pairing's `DirectEndpoint.wsURL`, not `relayURL`;
//   2. the auth signature: `"flightdeck-remote-direct-v1:" ‖ nonce`
//      (`Wire.directAuthMessage`) instead of the bare nonce, so a signature
//      minted for the Mac is useless at the relay and vice versa;
//   3. what rides the link: the direct link carries the data plane only. Push-
//      token registration and `revoke` stay on the relay (the relay needs the
//      membership and the token; the Mac answers those frames with
//      `bad_frame`, which is fatal), so a direct-mode pairing also runs a
//      second, control-plane-only `TransportClient` against its relay
//      (`TransportPlane.relayControl`).
//
//  `DirectConnection` itself is the socket factory for the direct link, built
//  on Network.framework (`NWConnection` + `NWProtocolWebSocket`) rather than
//  `URLSessionWebSocketTask`. Two reasons:
//
//   - App Transport Security. ATS governs URLSession, and it blocks cleartext
//     `ws://` to an IP literal no matter what: `NSAllowsLocalNetworking` does
//     not cover `100.x`, and `NSExceptionDomains` cannot name an IP (measured
//     in the simulator: both a MagicDNS name and a `100.x` address fail with
//     -1022 under the shipped Info.plist). The Mac's endpoint may be either
//     form (§5.10), so the only URLSession route would be
//     `NSAllowsArbitraryLoads`. Network.framework is not subject to ATS, so the
//     direct link needs NO ATS relaxation at all, and the relay path keeps the
//     app's ATS policy exactly as strict as before (`DirectLinkATSTests`).
//     Tailscale's WireGuard tunnel is the transport encryption; the E2E
//     envelopes ride on top.
//   - Fail fast. A peer that is often simply unreachable (Tailscale off, Mac
//     asleep) must fail within seconds so the reconnect banner and backoff take
//     over, instead of parking for a minute. `NWConnection` reports
//     `.waiting`/`.failed` promptly and the connect is bounded by a timeout.
//
//  Framing is identical to the relay: one JSON `RelayFrame` per WebSocket text
//  message (`WebSocketChannel`).
//
//  Local Network privacy: a `100.x` / MagicDNS destination is routed over the
//  Tailscale VPN (utun) interface, not the local Wi-Fi subnet, so connecting
//  does not trigger the Local Network permission prompt (loopback, used by the
//  tests, is exempt too). See ios/README.md "Direct link (Tailscale)".
//

import Foundation
import Network
import os

// MARK: - Plane / link selection

/// Which half of a pairing's traffic a `TransportClient` carries.
enum TransportPlane: Equatable, Sendable {
    /// The data plane — envelopes, resume, acks, snapshot requests — plus, for
    /// a relay-mode pairing, the relay's control frames. The concrete endpoint
    /// is resolved from the pairing record (`TransportLink.resolve`), so a
    /// direct-mode record can never route its data through the relay.
    case data
    /// Relay control only, for a DIRECT-mode pairing: push-token
    /// registration/unregistration, `revoke`, the relay's `pairing_revoked` and
    /// `machine_name`. Never sends `envelope`/`resume`/`ack` (spec §5.10: "the
    /// phone never sends envelope/resume/ack for a direct-mode pairing to the
    /// relay"), and never reports link state or presence — the data plane's
    /// client owns the honest connection UI.
    case relayControl
}

/// The concrete link one `TransportClient` session drives.
enum TransportLink: Equatable, Sendable {
    /// Relay-mode pairing: data plane and control plane through the relay.
    case relay
    /// Direct-mode pairing, data plane: the Mac's direct link over Tailscale.
    case direct(Wire.DirectEndpoint)
    /// Direct-mode pairing, control plane: the relay, push/revoke only.
    case relayControl

    /// Pick the link for `plane` from the pairing record — the one place the
    /// relay-vs-direct decision is made. A record with a `directEndpoint` is a
    /// direct-mode pairing (§5.2); every other record (including every record
    /// written before v4) is relay mode and behaves exactly as before.
    static func resolve(plane: TransportPlane, record: PairingRecord) -> TransportLink {
        switch plane {
        case .relayControl:
            return .relayControl
        case .data:
            if let endpoint = record.directEndpoint { return .direct(endpoint) }
            return .relay
        }
    }

    /// The URL to dial for `record` over this link, or `nil` if unusable.
    func url(for record: PairingRecord) -> URL? {
        switch self {
        case .relay, .relayControl: return URL(string: record.relayURL)
        case let .direct(endpoint): return endpoint.wsURL
        }
    }

    /// Whether this link carries envelopes / resume / acks.
    var carriesDataPlane: Bool {
        switch self {
        case .relay, .direct: return true
        case .relayControl: return false
        }
    }

    /// Whether this link carries push-token registration (and is where
    /// `revoke` must go — the relay owns membership and the token, §5.10).
    var carriesRelayControl: Bool {
        switch self {
        case .relay, .relayControl: return true
        case .direct: return false
        }
    }

    /// Whether the peer is the relay (so the shared relay password applies and
    /// `hello_ok.relay_mode` is meaningful). The Mac ignores the password
    /// (§5.10), and it is never sent there.
    var isRelay: Bool { carriesRelayControl }

    /// Sign the `auth_challenge` nonce the way this peer verifies it: the bare
    /// nonce for the relay (§5.1), the domain-separated
    /// `Wire.directAuthMessage` for the Mac (§5.10).
    func signChallenge(_ nonceB64: String, with identity: DeviceIdentity) throws -> String {
        switch self {
        case .relay, .relayControl: return try identity.signBase64(nonceBase64: nonceB64)
        case .direct: return try identity.signDirectAuthBase64(nonceBase64: nonceB64)
        }
    }
}

// MARK: - Direct-link socket

/// `WebSocketConnecting` for the direct link: a plain `ws://` WebSocket to the
/// Mac's Tailscale address over Network.framework (see the file header for why
/// not URLSession), framed exactly like the relay — one JSON `RelayFrame` per
/// text message.
///
/// Unlike `URLSessionWebSocketConnection`, whose handshake is lazy, `connect`
/// returns only once the WebSocket is open: it throws as soon as the path is
/// unusable (`.waiting` — e.g. Tailscale off and the MagicDNS name does not
/// resolve, or the Mac refused) or failed, and after `connectTimeout` at the
/// latest (a sleeping Mac just swallows SYNs). The transport's backoff and
/// reconnect banner take it from there.
struct DirectConnection: WebSocketConnecting {
    /// Default bound on DNS + TCP + WebSocket upgrade.
    static let defaultConnectTimeout: Duration = .seconds(10)
    /// Largest inbound message accepted. A full snapshot or transcript replay
    /// can be large; the relay path's URLSession default (1 MiB) is not a
    /// protocol limit, and the Mac is a trusted, authenticated peer.
    static let maximumMessageSize = 16 * 1024 * 1024

    let connectTimeout: Duration

    init(connectTimeout: Duration = DirectConnection.defaultConnectTimeout) {
        self.connectTimeout = connectTimeout
    }

    func connect(to url: URL) async throws -> any WebSocketChannel {
        // Only ever dial the direct-link endpoint shape (§5.10).
        guard let scheme = url.scheme?.lowercased(), scheme == "ws" || scheme == "wss",
              let host = url.host, !host.isEmpty else {
            throw RelayConnectionError.invalidURL
        }
        let parameters: NWParameters = scheme == "wss" ? .tls : .tcp
        if let tcp = parameters.defaultProtocolStack.transportProtocol as? NWProtocolTCP.Options {
            tcp.connectionTimeout = max(1, Int(connectTimeout.components.seconds))
            // Notice a Mac that went to sleep or off the tailnet mid-session
            // without relying on the relay-plane ping cadence alone.
            tcp.enableKeepalive = true
            tcp.keepaliveIdle = 15
            tcp.keepaliveInterval = 5
            tcp.keepaliveCount = 3
        }
        let webSocket = NWProtocolWebSocket.Options()
        webSocket.autoReplyPing = true
        webSocket.maximumMessageSize = Self.maximumMessageSize
        parameters.defaultProtocolStack.applicationProtocols.insert(webSocket, at: 0)

        let channel = NWWebSocketChannel(connection: NWConnection(to: .url(url), using: parameters))
        try await channel.open(timeout: connectTimeout)
        return channel
    }
}

/// A `WebSocketChannel` over one `NWConnection` speaking `NWProtocolWebSocket`.
///
/// Every completion-handler bridge resumes its continuation exactly once: the
/// open gate and the in-flight receive are held in lock-protected state that is
/// TAKEN before resuming, so a racing timeout, cancellation or `close()` can
/// never double-resume — and `close()` always wakes a pending `receive()`, so
/// the transport's receive loop can never hang on a dead link.
final class NWWebSocketChannel: WebSocketChannel, @unchecked Sendable {
    private struct State {
        var openContinuation: CheckedContinuation<Void, Error>?
        var receiveContinuation: CheckedContinuation<Wire.RelayFrame, Error>?
        var closed = false
    }

    private let connection: NWConnection
    private let queue = DispatchQueue(label: "agency.neworange.flightdeck.remote.direct-link")
    private let state = OSAllocatedUnfairLock(initialState: State())
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    init(connection: NWConnection) {
        self.connection = connection
    }

    // MARK: Open

    /// Start the connection and wait until the WebSocket is open. Throws on
    /// `.waiting` / `.failed` / `.cancelled`, on `timeout`, and on task
    /// cancellation (all of which also cancel the connection).
    func open(timeout: Duration) async throws {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                state.withLock { $0.openContinuation = continuation }
                if Task.isCancelled {
                    if finishOpen(.failure(CancellationError())) {
                        markClosed(CancellationError())
                    }
                    return
                }
                connection.stateUpdateHandler = { [weak self] newState in
                    guard let self else { return }
                    switch newState {
                    case .ready:
                        self.finishOpen(.success(()))
                    case let .waiting(error), let .failed(error):
                        // `.waiting` = no usable path right now (unresolvable
                        // name, refused, no route). Fail the attempt; the
                        // transport's backoff decides when to retry.
                        self.finishOpen(.failure(error))
                        self.markClosed(error)
                    case .cancelled:
                        self.finishOpen(.failure(RelayConnectionError.closed))
                        self.markClosed(RelayConnectionError.closed)
                    default:
                        break
                    }
                }
                connection.start(queue: queue)
                queue.asyncAfter(deadline: .now() + timeout.timeInterval) { [weak self] in
                    guard let self else { return }
                    if self.finishOpen(.failure(RelayConnectionError.closed)) {
                        self.markClosed(RelayConnectionError.closed)
                    }
                }
            }
        } onCancel: {
            if finishOpen(.failure(CancellationError())) {
                markClosed(CancellationError())
            }
        }
    }

    /// Resume the open gate once. Returns whether THIS call resumed it.
    @discardableResult
    private func finishOpen(_ result: Result<Void, Error>) -> Bool {
        let continuation = state.withLock { s -> CheckedContinuation<Void, Error>? in
            defer { s.openContinuation = nil }
            return s.openContinuation
        }
        guard let continuation else { return false }
        continuation.resume(with: result)
        return true
    }

    /// Mark the channel closed, wake any pending receive, cancel the socket.
    private func markClosed(_ error: Error) {
        let pending = state.withLock { s -> CheckedContinuation<Wire.RelayFrame, Error>? in
            s.closed = true
            defer { s.receiveContinuation = nil }
            return s.receiveContinuation
        }
        pending?.resume(throwing: RelayConnectionError.closed)
        connection.cancel()
    }

    // MARK: WebSocketChannel

    func send(_ frame: Wire.RelayFrame) async throws {
        let data: Data
        do {
            data = try encoder.encode(frame)
        } catch {
            throw RelayConnectionError.encodeFailed
        }
        try await sendMessage(data, opcode: .text)
    }

    func receive() async throws -> Wire.RelayFrame {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Wire.RelayFrame, Error>) in
            let alreadyClosed = state.withLock { s -> Bool in
                if s.closed { return true }
                s.receiveContinuation = continuation
                return false
            }
            if alreadyClosed {
                continuation.resume(throwing: RelayConnectionError.closed)
                return
            }
            receiveNext()
        }
    }

    func ping() async throws {
        try await sendMessage(Data(), opcode: .ping)
    }

    func close() async {
        markClosed(RelayConnectionError.closed)
    }

    // MARK: Private

    /// Read messages until a data frame (or the end of the stream) resolves the
    /// pending receive. Control frames are handled by the protocol
    /// (`autoReplyPing`) and skipped here.
    private func receiveNext() {
        connection.receiveMessage { [weak self] content, context, _, error in
            guard let self else { return }
            let outcome: Result<Wire.RelayFrame, Error>?
            if let error {
                outcome = .failure(error)
            } else {
                let metadata = context?.protocolMetadata(definition: NWProtocolWebSocket.definition)
                    as? NWProtocolWebSocket.Metadata
                switch metadata?.opcode {
                case .text?, .binary?:
                    do {
                        outcome = .success(try self.decoder.decode(Wire.RelayFrame.self, from: content ?? Data()))
                    } catch {
                        outcome = .failure(RelayConnectionError.decodeFailed)
                    }
                case .close?, nil:
                    // A close frame, or end-of-stream with no message.
                    outcome = .failure(RelayConnectionError.closed)
                default:
                    outcome = nil // ping / pong / continuation: keep reading
                }
            }
            guard let outcome else {
                self.receiveNext()
                return
            }
            let pending = self.state.withLock { s -> CheckedContinuation<Wire.RelayFrame, Error>? in
                defer { s.receiveContinuation = nil }
                return s.receiveContinuation
            }
            pending?.resume(with: outcome)
        }
    }

    private func sendMessage(_ data: Data, opcode: NWProtocolWebSocket.Opcode) async throws {
        if state.withLock({ $0.closed }) { throw RelayConnectionError.closed }
        let metadata = NWProtocolWebSocket.Metadata(opcode: opcode)
        let context = NWConnection.ContentContext(identifier: "relay-frame", metadata: [metadata])
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            connection.send(content: data, contentContext: context, isComplete: true,
                            completion: .contentProcessed { error in
                if let error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume()
                }
            })
        }
    }
}

private extension Duration {
    /// This duration as a `TimeInterval` (seconds), for Dispatch deadlines.
    var timeInterval: TimeInterval {
        let (seconds, attoseconds) = components
        return TimeInterval(seconds) + TimeInterval(attoseconds) / 1e18
    }
}

