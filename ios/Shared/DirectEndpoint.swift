//
//  DirectEndpoint.swift
//  FlightDeckRemote (shared with FlightDeckNotificationService)
//
//  Where the phone can reach a Mac directly over Tailscale (REMOTE_PROTOCOL
//  v4, §5.10): the Mac's MagicDNS name (`ruuds-mbp.tail1234.ts.net`) or
//  Tailscale address (`100.101.102.103`) and the port its direct-link
//  WebSocket listens on. Carried in `pairing_offer` and copied by the relay
//  into the phone-bound `pairing_claimed`; a pairing whose claim carried one is
//  in direct mode. Mirrors Rust `relay::DirectEndpoint`; the app's wire layer
//  refers to it as `Wire.DirectEndpoint`.
//
//  It lives in Shared/ (not Transport/Protocol/) because it is persisted on
//  `PairingRecord`, which the Notification Service Extension also compiles —
//  and the extension deliberately compiles no `Wire` types. Foundation only.
//

import Foundation

struct DirectEndpoint: Codable, Hashable, Sendable {
    /// MagicDNS name or Tailscale IP of the Mac.
    var host: String
    /// TCP port of the Mac's direct-link WebSocket.
    var port: UInt16

    /// Path the desktop serves the direct link on (`DIRECT_LINK_PATH`).
    static let path = "/remote/v1"

    /// `ws://<host>:<port>/remote/v1` — byte-for-byte Rust
    /// `DirectEndpoint::ws_url`: an IPv6 literal (contains `:`) is bracketed
    /// per RFC 3986. Plain `ws://` on purpose: Tailscale's WireGuard tunnel is
    /// the transport encryption, and the E2E envelopes ride on top unchanged.
    var wsURLString: String {
        if host.contains(":") && !host.hasPrefix("[") {
            return "ws://[\(host)]:\(port)\(Self.path)"
        }
        return "ws://\(host):\(port)\(Self.path)"
    }

    /// `wsURLString` as a `URL`, or `nil` for a host that cannot form one.
    var wsURL: URL? { URL(string: wsURLString) }
}
