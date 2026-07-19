//
//  PairedInstance.swift
//  FlightDeckRemote
//
//  Non-secret display/prefs metadata for ONE established pairing
//  (multi-pairing, remote-control-b8d.4). Secrets (the E2E key material) and
//  cursors (seq watermarks) live in the Keychain-backed `PairingRecord` /
//  `PairingRecordStore` (remote-control-b8d.3); `PairedInstance` holds only
//  what the UI needs to show and let the user tweak, joined to that record
//  by the same `pairingId`.
//
//  `PairingStore` is the persisted collection of these (see PairingStore.swift),
//  the single source of truth for "which machines am I paired with" consumed
//  by the transport coordinator (b8d.5), the aggregated feed (b8d.6), the
//  router (b8d.7), push (b8d.10), and settings/unpair (b8d.11).
//

import Foundation

/// Non-secret metadata about one paired FlightDeck desktop instance.
struct PairedInstance: Codable, Equatable, Identifiable, Sendable {
    /// The relay-assigned pairing identifier (REMOTE_PROTOCOL §5.2
    /// `pairing_claimed.pairing_id`) — the join key with `PairingRecord`.
    var pairingId: String
    /// The machine name most recently reported by the desktop on connect
    /// (carried in an authenticated post-auth frame, remote-control-b8d.1).
    /// `nil` until the first post-pairing connect reports it, or if this
    /// pairing predates that wire addition. Re-sent on every connect so this
    /// auto-updates if the Mac is renamed — see `setMachineName(pairingId:_:)`.
    var machineNameFromDesktop: String?
    /// A user-chosen display name for this machine, set from the machine
    /// naming UI (remote-control-b8d.9). Always wins over the desktop-reported
    /// name when present.
    var userOverrideName: String?
    /// The relay endpoint THIS pairing connects to (per-instance relay
    /// topology — each pairing may live behind a different relay URL).
    var relayURL: URL
    /// Whether push notifications from this specific machine are muted
    /// (remote-control-b8d.10). Defaults to `false` — all machines are on by
    /// default, muted per-machine.
    var mutePush: Bool
    /// When this pairing was established.
    var pairedAt: Date
    /// Whether this machine was reachable the last time we checked (drives
    /// the offline-dimmed + "offline" badge in the unified feed,
    /// remote-control-b8d.6/.8). Not a live health check by itself — updated
    /// by whatever owns the live connection (the transport coordinator,
    /// remote-control-b8d.5).
    var lastKnownOnline: Bool

    /// Shown when neither the desktop name nor a user override is available
    /// (e.g. immediately after pairing, before the first post-auth frame).
    static let fallbackDisplayName = "Paired Mac"

    /// Display-name precedence (remote-control-b8d.4 acceptance criteria):
    /// user override wins over the desktop-reported name, which wins over a
    /// generic fallback.
    var displayName: String {
        userOverrideName ?? machineNameFromDesktop ?? Self.fallbackDisplayName
    }

    /// Stable identity for `Identifiable` / SwiftUI `List`/`ForEach` — the
    /// pairing id itself, since it's already the unique join key.
    var id: String { pairingId }

    init(
        pairingId: String,
        machineNameFromDesktop: String? = nil,
        userOverrideName: String? = nil,
        relayURL: URL,
        mutePush: Bool = false,
        pairedAt: Date = Date(),
        lastKnownOnline: Bool = true
    ) {
        self.pairingId = pairingId
        self.machineNameFromDesktop = machineNameFromDesktop
        self.userOverrideName = userOverrideName
        self.relayURL = relayURL
        self.mutePush = mutePush
        self.pairedAt = pairedAt
        self.lastKnownOnline = lastKnownOnline
    }
}
