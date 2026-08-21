//
//  PairingLimits.swift
//  FlightDeckRemote
//
//  The SINGLE shared source of truth for the multi-pairing hard cap (epic
//  remote-control-b8d, product decision: "~3-4 paired instances"; this picks
//  4). Every cap check in the app references `maxPairedInstances` from here
//  rather than hardcoding its own literal, so they can never drift out of
//  sync (remote-control-b8d.7):
//   - `PairingStore.isAtPairingCap` — what `PairingView`/`AddMachineSheet`
//     check before letting the user start a new pairing.
//   - `TransportCoordinator`'s default `cap` — the fan-out bound on live
//     transports (remote-control-b8d.5).
//

import Foundation

/// The hard maximum number of FlightDeck desktop instances one phone may be
/// simultaneously paired with.
enum PairingLimits {
    /// PRD/epic decision: "a hard maximum of ~3-4 paired instances." Picked 4
    /// (the top of that range) as the single concrete value everything else
    /// derives from.
    static let maxPairedInstances = 4

    /// User-facing copy shown when starting a new pairing is blocked because
    /// the cap has been reached (PRD §5.6/§8 connection honesty: a specific,
    /// actionable message rather than a generic failure).
    static let capReachedMessage =
        "You've reached the limit of \(maxPairedInstances) paired Macs. Unpair one before adding another."
}
