//
//  RelayConnection.swift
//  FlightDeckRemote
//
//  The WebSocket layer under the transport (REMOTE_PROTOCOL §2): one JSON
//  `RelayFrame` per WebSocket *text* message. This is factored behind a small
//  `WebSocketConnecting` / `WebSocketChannel` seam so the state machine
//  (`TransportClient`) and pairing (`RealPairingService`) can be driven by a
//  scripted mock in unit tests while production uses a real
//  `URLSessionWebSocketTask`.
//
//  TLS (`wss://`) and plain (`ws://`, for localhost dev / the integration test)
//  are both handled transparently by URLSession from the URL scheme.
//

import Foundation

/// Errors surfaced by the relay WebSocket layer.
enum RelayConnectionError: Error, Equatable {
    /// The relay URL string was not a valid URL.
    case invalidURL
    /// A send/receive was attempted on a closed or never-opened channel.
    case notConnected
    /// The peer closed the connection.
    case closed
    /// A received frame could not be decoded as a `RelayFrame`.
    case decodeFailed
    /// An outgoing frame could not be encoded.
    case encodeFailed
    /// A non-text (binary) message arrived; the relay plane is JSON text only.
    case unexpectedMessage
}

/// Opens relay connections. The production implementation is
/// `URLSessionWebSocketConnection`; tests inject a scripted mock.
protocol WebSocketConnecting: Sendable {
    /// Open a WebSocket to `url` and return a live channel. Throws on a URL or
    /// transport-setup failure (the first `receive`/`send` surfaces later
    /// connect errors, matching `URLSessionWebSocketTask`'s lazy handshake).
    func connect(to url: URL) async throws -> any WebSocketChannel
}

/// A live relay WebSocket. One JSON `RelayFrame` per text message.
protocol WebSocketChannel: Sendable {
    /// Encode and send one relay frame as a WebSocket text message.
    func send(_ frame: Wire.RelayFrame) async throws
    /// Await and decode the next relay frame. Throws `RelayConnectionError`
    /// (`.closed` on a clean close) when the stream ends.
    func receive() async throws -> Wire.RelayFrame
    /// Send a WebSocket-level ping (keepalive; distinct from the relay-plane
    /// `ping` frame that measures latency).
    func ping() async throws
    /// Close the connection.
    func close() async
}

// MARK: - URLSession-backed production implementation

/// Production `WebSocketConnecting` over `URLSessionWebSocketTask`.
struct URLSessionWebSocketConnection: WebSocketConnecting {
    private let session: URLSession

    init(session: URLSession = .shared) {
        self.session = session
    }

    func connect(to url: URL) async throws -> any WebSocketChannel {
        let task = session.webSocketTask(with: url)
        task.resume()
        return URLSessionWebSocketChannel(task: task)
    }
}

/// A `WebSocketChannel` wrapping one `URLSessionWebSocketTask`. Concurrent
/// send/receive is supported by the task; the JSON coders are value-immutable.
final class URLSessionWebSocketChannel: WebSocketChannel, @unchecked Sendable {
    private let task: URLSessionWebSocketTask
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    init(task: URLSessionWebSocketTask) {
        self.task = task
    }

    func send(_ frame: Wire.RelayFrame) async throws {
        let data: Data
        do {
            data = try encoder.encode(frame)
        } catch {
            throw RelayConnectionError.encodeFailed
        }
        let text = String(decoding: data, as: UTF8.self)
        try await task.send(.string(text))
    }

    func receive() async throws -> Wire.RelayFrame {
        let message = try await task.receive()
        let data: Data
        switch message {
        case .string(let text):
            data = Data(text.utf8)
        case .data(let raw):
            // The relay plane is text JSON, but decode data defensively.
            data = raw
        @unknown default:
            throw RelayConnectionError.unexpectedMessage
        }
        do {
            return try decoder.decode(Wire.RelayFrame.self, from: data)
        } catch {
            throw RelayConnectionError.decodeFailed
        }
    }

    func ping() async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            task.sendPing { error in
                if let error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume()
                }
            }
        }
    }

    func close() async {
        task.cancel(with: .goingAway, reason: nil)
    }
}
