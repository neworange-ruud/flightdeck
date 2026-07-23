//
//  PairingService.swift
//  FlightDeckRemote
//
//  The pairing transaction itself: redeem a claim token (from a scanned QR
//  or a typed 4-digit code) against the relay and come back with a
//  `PairedDevice`. `PairingView` depends only on `PairingServicing`, so the
//  transport task can drop in a real relay-backed implementation without
//  touching this feature's UI.
//

import Foundation

/// What the user gave us to redeem: a scanned/pasted QR payload, or a typed
/// 4-digit code plus the relay to redeem it against (manual entry has no
/// other way to learn the relay address — see `PairingDefaults.relayURL`).
enum PairingInput {
    case qr(PairingQRPayload)
    case code(String, relayURL: URL)
}

/// The result of a successful pairing, as far as this feature needs to know.
/// Persisted into `PairingStore` via `completePairing(with:)`.
struct PairedDevice: Equatable {
    /// The relay-assigned pairing identifier (REMOTE_PROTOCOL §5.2
    /// `pairing_claimed.pairing_id`). Routing on the relay plane happens by
    /// this id only.
    let pairingId: String
    /// Human-readable name of the paired Mac, for display (e.g. "Ruud's
    /// MacBook Pro — Connected", PRD §5.6). Placeholder until the desktop
    /// side of pairing exchanges a real device name; a real implementation
    /// should populate this from `pairing_claimed`/a follow-up handshake
    /// once the desktop pairing task defines that field.
    let peerName: String
    /// When this pairing was established, for display/diagnostics.
    let pairedAt: Date
}

/// Performs one pairing transaction: redeem a claim token, come back with a
/// `PairedDevice` or a typed `PairingError`.
///
/// **Contract for the real (relay-backed) implementation** (transport task):
///  1. Open a websocket to the relay and send `hello { role: .phone, … }`
///     (REMOTE_PROTOCOL §4), using this device's `DeviceIdentity` for
///     `device_id`/`device_public_key`.
///  2. On `auth_challenge { nonce }`, send
///     `pairing_claim { claim_token, device_id, device_public_key, role: .phone }`
///     — this both redeems the token and self-registers the phone's device
///     key (§5.2), then continue with `auth_response` per §5.1.
///  3. On `pairing_claimed { pairing_id, peer_device_id }` (or
///     `error { code: pairing_claim_rejected }` on failure), the claim is
///     resolved.
///  4. Complete the E2E bootstrap using the `pairing_secret` carried by the
///     QR payload (§7; out of scope for the relay plane and this protocol
///     doc) — for manual-code pairing there is no QR-carried secret, so the
///     real implementation must define how that path establishes/derives
///     its E2E secret before this method can return success for `.code`.
///  5. Persist results: store the negotiated E2E key material / secret in
///     the Keychain (see `KeychainStoring`), and call
///     `PairingStore.completePairing(with:)` so the router flips to the
///     main tab container.
///  6. Return the resulting `PairedDevice`. Any failure at any step must
///     surface as a typed `PairingError`, never a generic `Error` — the UI
///     shows the error's `errorDescription` verbatim (PRD §8 connection
///     honesty: no silent/generic failures).
protocol PairingServicing {
    /// Redeem `input` against the relay. `relayPassword` is the OPTIONAL shared
    /// relay password (remote-control-uq7) captured on the pairing screen —
    /// pairing runs over the relay, so a password-gated relay needs it up front,
    /// in the pairing `hello`. `nil` (unconfigured/local relay) presents no
    /// password. A relay-backed implementation persists it (Keychain) so every
    /// later reconnect can present it too.
    func pair(with input: PairingInput, relayPassword: String?) async throws -> PairedDevice
}

/// Stand-in `PairingServicing` used until the relay transport (a separate
/// task) lands. Simulates a ~1s network round trip, then accepts:
///  - manual code `"4729"` (matches the PRD §5.6 example code), rejecting
///    any other code with `.invalidCode`;
///  - any QR payload that decoded successfully and has non-empty
///    `claimToken`/`pairingSecret`, rejecting an empty/blank one with
///    `.malformedQRPayload`.
///
/// This is a real, always-compiled type (not `#if DEBUG`-gated) because
/// `PairingView` needs a concrete default `PairingServicing` to run against
/// in every configuration until the transport task ships; it is expected to
/// be replaced wholesale, not conditionally compiled out.
struct MockPairingService: PairingServicing {
    /// Simulated relay round-trip latency.
    var delay: Duration = .seconds(1)

    /// `relayPassword` is accepted for protocol conformance but ignored — the
    /// mock never opens a socket, so there is no relay to gate. Defaulted so
    /// the existing concrete-typed test call sites (`pair(with:)`) still compile.
    func pair(with input: PairingInput, relayPassword: String? = nil) async throws -> PairedDevice {
        try await Task.sleep(for: delay)

        switch input {
        case .code(let code, _):
            guard code == "4729" else {
                throw PairingError.invalidCode
            }
        case .qr(let payload):
            guard !payload.claimToken.isEmpty, !payload.pairingSecret.isEmpty else {
                throw PairingError.malformedQRPayload
            }
        }

        return PairedDevice(
            pairingId: "mock-pairing-\(UUID().uuidString.prefix(8))",
            peerName: "Ruud's MacBook Pro",
            pairedAt: Date()
        )
    }
}
