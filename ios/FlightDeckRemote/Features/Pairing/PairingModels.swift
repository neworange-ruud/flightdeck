//
//  PairingModels.swift
//  FlightDeckRemote
//
//  Wire types for the pairing bootstrap (REMOTE_PROTOCOL §5.2, PRD §5.6/§9):
//  the desktop displays a QR code and a 4-digit code; both encode the same
//  claim token. The phone redeems the claim token via `pairing_claim` over
//  the relay websocket and receives a `pairing_id` back. The QR additionally
//  carries the `pairing_secret` that bootstraps the E2E channel (that secret
//  never transits the relay — REMOTE_PROTOCOL §5.2) and the relay's address,
//  since a QR is the only path that can convey it out-of-band; manual
//  4-digit entry relies on the relay address being fixed per app build
//  (`PairingDefaults.relayURL`).
//
//  QR PAYLOAD FORMAT (normative for this app; the desktop pairing task
//  mirrors this exactly):
//
//    "fdr1:" + base64url(JSON, no padding)
//
//  where the JSON object is:
//
//    {
//      "claim_token": "<opaque relay-issued token, ASCII>",
//      "pairing_secret": "<base64url, no padding — E2E bootstrap secret>",
//      "relay_url": "<wss:// URL string>"
//    }
//
//  - `fdr1` = "FlightDeck Remote, payload version 1". A future incompatible
//    QR shape bumps this prefix (`fdr2:`, …) so old/new scanners can tell
//    versions apart at a glance, before even attempting to decode.
//  - The prefix is plain ASCII (not itself base64) so a scanner can reject a
//    non-FlightDeck QR code instantly, without attempting a decode.
//  - JSON keys are `snake_case` to match every other wire type in
//    REMOTE_PROTOCOL (§3 convention #2); Swift mirrors them via
//    `CodingKeys`.
//  - `pairing_secret` and the outer base64url blob both use base64url
//    *without* padding, matching `DeviceIdentity`'s `deviceId` encoding
//    convention elsewhere in this app — NOT the base64-standard-padded
//    convention REMOTE_PROTOCOL uses for relay-plane wire fields. The QR
//    payload is a local bootstrap artifact, not a relay frame, so it is free
//    to pick the URL-safe alphabet (no `+`/`/` to escape when the payload is
//    later carried in a URL or pasted as plain text).
//  - The 4-digit manual-entry code IS the claim token for manual entry
//    (REMOTE_PROTOCOL §5.2): a 4-digit decimal string is short enough to
//    type but still a valid `claim_token` value.
//

import Foundation

/// The decoded contents of a FlightDeck Remote pairing QR code.
///
/// See the file-level doc comment above for the exact wire format
/// (`"fdr1:" + base64url(JSON)`) — that format, not this type's Swift
/// property names, is the source of truth for interop with the desktop.
struct PairingQRPayload: Codable, Equatable {
    /// Short-TTL, single-use token minted by the relay via
    /// `pairing_offer_ok` (REMOTE_PROTOCOL §5.2). Redeemed via
    /// `pairing_claim`.
    let claimToken: String
    /// base64url (no padding) shared secret that bootstraps the E2E
    /// channel. Never transits the relay as a frame field — only ever
    /// carried inside this QR payload.
    let pairingSecret: String
    /// The relay endpoint to connect to for this pairing.
    let relayURL: URL

    enum CodingKeys: String, CodingKey {
        case claimToken = "claim_token"
        case pairingSecret = "pairing_secret"
        case relayURL = "relay_url"
    }
}

/// Errors the pairing flow can surface to the UI. Always typed — never a
/// generic `Error` — so `PairingView` can show an honest, specific message
/// (PRD §8 connection honesty) instead of a generic failure.
enum PairingError: Error, Equatable {
    /// The 4-digit code was rejected (wrong digits, or the relay reports the
    /// underlying claim token was bad/expired — REMOTE_PROTOCOL
    /// `pairing_claim_rejected`).
    case invalidCode
    /// A scanned or pasted QR payload didn't parse as a `PairingQRPayload`
    /// (bad prefix, invalid base64url, or invalid/incomplete JSON).
    case malformedQRPayload
    /// The claim token decoded/typed correctly but the relay says it's
    /// expired or already used.
    case expiredOrUsedToken
    /// No route to the relay (offline, DNS failure, etc.).
    case networkUnavailable
    /// The relay round-trip didn't complete in time.
    case timedOut
    /// Camera access was denied/restricted for QR scanning.
    case cameraPermissionDenied
    /// Anything else, carrying a short honest description for display.
    case unknown(String)
}

extension PairingError: LocalizedError {
    var errorDescription: String? {
        switch self {
        case .invalidCode:
            return "That code didn't match. Check the code on your Mac and try again."
        case .malformedQRPayload:
            return "That QR code isn't a FlightDeck pairing code."
        case .expiredOrUsedToken:
            return "That code has expired. Generate a new one on your Mac."
        case .networkUnavailable:
            return "Can't reach the relay right now. Check your connection and try again."
        case .timedOut:
            return "Pairing timed out. Try again."
        case .cameraPermissionDenied:
            return "Camera access is off — enter the code instead, or enable the camera in Settings."
        case .unknown(let message):
            return message
        }
    }
}

/// Fixed configuration for the relay transport.
enum PairingDefaults {
    /// Info.plist key carrying the relay endpoint. The value is committed to
    /// the repo via `ios/project.yml` (`info.properties.FlightDeckRelayURL`),
    /// from which XcodeGen writes `Info.plist`. Changing the relay address is a
    /// one-line plist edit — no Swift source change (remote-control-2mk).
    static let relayURLInfoPlistKey = "FlightDeckRelayURL"

    /// Last-resort fallback, used only if the Info.plist key is absent or
    /// malformed (should never happen in a correctly built app — the plist
    /// carries the canonical value). Mirrors the committed plist value so the
    /// app still reaches the relay even if the key is somehow lost.
    static let fallbackRelayURL = URL(
        string: "wss://relay.flightdeckai.app/ws"
    )!

    /// The hosted relay endpoint (PRD §9.1: operated by New Orange on Azure
    /// Container Apps), read at runtime from the committed Info.plist key
    /// `FlightDeckRelayURL`. Manual 4-digit-code pairing has no other way to
    /// learn the relay address — only the QR payload carries `relay_url`
    /// explicitly (forward-compatible with a future self-hosted relay picker).
    ///
    /// Points at the stable custom domain (`relay.flightdeckai.app`,
    /// remote-control-edn) fronting the Azure Container Apps relay, so it
    /// survives any rename/recreate of the underlying Azure resources — no more
    /// chasing `*.azurecontainerapps.io` hostname churn. Change it (if ever) by
    /// editing the plist value in `ios/project.yml`, not Swift source.
    static let relayURL: URL = resolveRelayURL(
        Bundle.main.object(forInfoDictionaryKey: relayURLInfoPlistKey) as? String
    )

    /// Resolve a raw relay-URL string to a `URL`, falling back to
    /// `fallbackRelayURL` when it is absent, empty, or not a valid `ws`/`wss`
    /// URL with a host. Pure + `internal` so tests can exercise every branch
    /// without constructing a custom bundle.
    static func resolveRelayURL(_ raw: String?) -> URL {
        guard
            let trimmed = raw?.trimmingCharacters(in: .whitespacesAndNewlines),
            !trimmed.isEmpty,
            let url = URL(string: trimmed),
            let scheme = url.scheme?.lowercased(),
            scheme == "wss" || scheme == "ws",
            url.host != nil
        else { return fallbackRelayURL }
        return url
    }
}

/// Encodes/decodes the `"fdr1:" + base64url(JSON)` QR payload format
/// documented at the top of this file.
enum PairingQRCodec {
    /// Scheme marker: "FlightDeck Remote, payload version 1".
    static let schemePrefix = "fdr1:"

    static func encode(_ payload: PairingQRPayload) throws -> String {
        let encoder = JSONEncoder()
        let data = try encoder.encode(payload)
        return schemePrefix + data.base64URLEncodedStringNoPadding()
    }

    static func decode(_ string: String) throws -> PairingQRPayload {
        guard string.hasPrefix(schemePrefix) else {
            throw PairingError.malformedQRPayload
        }
        let base64url = String(string.dropFirst(schemePrefix.count))
        guard !base64url.isEmpty, let data = Data(base64URLEncodedNoPadding: base64url) else {
            throw PairingError.malformedQRPayload
        }
        do {
            return try JSONDecoder().decode(PairingQRPayload.self, from: data)
        } catch {
            throw PairingError.malformedQRPayload
        }
    }
}

extension Data {
    /// Decodes a base64url (RFC 4648 §5), no-padding string — the inverse of
    /// `Data.base64URLEncodedStringNoPadding()` (Security/DeviceIdentity.swift).
    /// Returns `nil` for any string that isn't valid base64url.
    init?(base64URLEncodedNoPadding string: String) {
        var base64 = string
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let remainder = base64.count % 4
        if remainder > 0 {
            base64 += String(repeating: "=", count: 4 - remainder)
        }
        guard let data = Data(base64Encoded: base64) else { return nil }
        self = data
    }
}
