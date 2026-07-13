//
//  PairingStore.swift
//  FlightDeckRemote
//
//  Stub for the pairing feature. The real implementation will own the
//  per-device identity keypair (Keychain/Secure Enclave), talk to the relay
//  to complete pairing (QR / 4-digit code), and persist pairing state across
//  launches (PRD §9 — "persists until explicitly unpaired").
//

import Foundation
import Observation

/// Tracks whether this device is paired with a FlightDeck desktop instance.
///
/// Currently a stub: always reports unpaired so `AppRouter` routes to the
/// Pairing flow. The Pairing feature team will replace `isPaired` with real
/// persisted state (e.g. backed by Keychain).
@Observable
final class PairingStore {
    /// Whether this device currently has an active pairing with a Mac.
    var isPaired: Bool = false
}
