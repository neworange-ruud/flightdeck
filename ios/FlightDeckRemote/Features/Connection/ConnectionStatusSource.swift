//
//  ConnectionStatusSource.swift
//  FlightDeckRemote
//
//  The minimal read surface the Connection feature (ConnectionIndicator,
//  ReconnectingBanner, CommandsPausedGate) needs from the transport layer:
//  just the current link state. `TransportStore` conforms for free below —
//  it already exposes `linkState` with this exact signature — so this
//  indirection exists purely so unit tests can inject a trivial fake instead
//  of constructing a real `TransportClient` (an actor wired to a live relay
//  socket, device identity, and a pairing record store).
//

import Foundation

/// Read-only connection-state surface consumed by the Connection feature.
@MainActor
protocol ConnectionStatusSource: AnyObject {
    /// The current relay link state (PRD §5.8/§8: connection honesty).
    var linkState: RemoteLinkState { get }
}

extension TransportStore: ConnectionStatusSource {}
