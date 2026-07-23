//
//  ConnectionTestSupport.swift
//  FlightDeckRemoteTests
//
//  Shared fake for the Connection feature's tests — a trivial, mutable
//  `ConnectionStatusSource` so tests never need to construct a real
//  `TransportClient`/`TransportStore` (actor, live relay socket, device
//  identity, pairing record store).
//

import Foundation
@testable import FlightDeckRemote

@MainActor
final class FakeConnectionStatusSource: ConnectionStatusSource {
    var linkState: RemoteLinkState
    var peerConnected: Bool?

    init(_ linkState: RemoteLinkState = .disconnected, peerConnected: Bool? = nil) {
        self.linkState = linkState
        self.peerConnected = peerConnected
    }
}
