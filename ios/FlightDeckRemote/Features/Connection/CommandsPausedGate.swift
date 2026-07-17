//
//  CommandsPausedGate.swift
//  FlightDeckRemote
//
//  Single source of truth for "are commands currently paused because the
//  phone↔desktop link is down" (PRD §5.6/§8: lost link pauses commands
//  loudly; nothing sent blind). `TransportClient` already refuses to send
//  while not connected (delivery honesty) — this gate is the *visible*
//  counterpart other screens consult before letting the user try.
//
//  This task only builds the gate itself; wiring it into chat compose /
//  approve-deny controls is the compose task's job. There is no default
//  source — whoever mounts a gate picks the real `TransportStore` instance
//  explicitly, so a screen can never accidentally read a stale/placeholder
//  connection state.
//
//  Usage (sibling screens):
//  ```swift
//  @State private var pausedGate = CommandsPausedGate(source: transportStore)
//  ...
//  Button("Send") { ... }
//      .disabled(pausedGate.commandsPaused)
//  ```
//

import Foundation
import Observation

/// Exposes `commandsPaused`, computed from the live `linkState`: `true`
/// whenever the link isn't a fully-established `.connected` session (i.e.
/// during `.connecting`/`.authenticating`/`.disconnected` alike).
@MainActor
@Observable
final class CommandsPausedGate {
    private let source: any ConnectionStatusSource

    #if DEBUG
    private let forced: RemoteLinkState?
    #endif

    init(
        source: any ConnectionStatusSource,
        launchArguments: [String] = ProcessInfo.processInfo.arguments
    ) {
        self.source = source
        #if DEBUG
        self.forced = ConnectionDebugSeam.forcedLinkState(arguments: launchArguments)
        #endif
    }

    /// The link state driving `commandsPaused` (DEBUG launch-arg override
    /// wins, for UI tests/previews — see `ConnectionDebugSeam`).
    var linkState: RemoteLinkState {
        #if DEBUG
        if let forced { return forced }
        #endif
        return source.linkState
    }

    /// `true` whenever sending would either fail outright or leave the
    /// sender unsure whether it landed — i.e. whenever the link isn't a
    /// live `.connected` session.
    var commandsPaused: Bool {
        ReconnectingBannerModel.isDown(linkState)
    }
}
