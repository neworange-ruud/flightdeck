//
//  RealPairingService.swift
//  FlightDeckRemote
//
//  The relay-backed `PairingServicing` (REMOTE_PROTOCOL §5.2). It runs one
//  pairing transaction over a fresh WebSocket and, on success, persists a
//  `PairingRecord` so `TransportClient` can reconnect / resume / derive the
//  E2E channel — then returns the `PairedDevice` for `PairingView` to hand to
//  `PairingStore.completePairing`.
//
//  Wire flow (phone side, §5.2, exact ordering):
//    → hello { role: phone, device_id, client }
//    ← hello_ok
//    ← auth_challenge { nonce }
//    → pairing_claim { claim_token, device_id, device_public_key,
//                      key_agreement_public_key, role: phone }
//    ← pairing_claimed { pairing_id, peer_device_id,
//                        peer_key_agreement_public_key }   (or error)
//    → auth_response { device_id, signature(nonce), pairing_ids: [pairing_id] }
//    ← auth_ok
//
//  The phone MUST send its *software* key-agreement public key (KeyAgreementKeys)
//  in `pairing_claim`, distinct from the Secure-Enclave signing identity
//  (§5.2). The E2E salt is ALWAYS the claim-token UTF-8 bytes, on BOTH the QR
//  and manual-code paths (§7.1, reconciled contract): the desktop derives its
//  channel from the `pairing_claimed` notification and cannot know which path
//  the phone used. The QR still carries `pairing_secret` for wire-compat, but
//  it is NOT used in key derivation.
//

import Foundation

/// Relay-backed pairing. Uses the same `WebSocketConnecting` seam as the
/// transport so tests can drive it against a scripted relay.
struct RealPairingService: PairingServicing {
    private let connector: any WebSocketConnecting
    private let recordStore: PairingRecordStore
    private let identityStore: KeychainStoring
    /// Secure store for the OPTIONAL shared relay password (remote-control-uq7).
    /// The password is presented in the pairing `hello` (pairing runs over the
    /// relay) and persisted here on success so `TransportClient` can present it
    /// on every later reconnect.
    private let relayPasswordStore: RelayPasswordStore
    private let clientInfo: Wire.ClientInfo
    private let timeout: Duration
    private let peerName: String
    /// The multi-pairing metadata store (remote-control-b8d.4). `pair(with:)`
    /// appends a `PairedInstance` here on success — ADDING to whatever's
    /// already paired rather than replacing it, so pairing with a second (or
    /// third) Mac doesn't disturb the first. Cap enforcement is deferred to
    /// remote-control-b8d.7. Callers that want this reflected app-wide
    /// (router/feed/transport/push/settings all observing the same list)
    /// must inject the same shared `PairingStore` instance used elsewhere at
    /// the composition root — see `PairingServiceFactory.makeDefault(pairingStore:)`.
    private let pairingStore: PairingStore

    init(
        connector: any WebSocketConnecting = URLSessionWebSocketConnection(),
        recordStore: PairingRecordStore = PairingRecordStore(),
        identityStore: KeychainStoring = KeychainStore(service: DeviceIdentity.service),
        relayPasswordStore: RelayPasswordStore = RelayPasswordStore(),
        clientInfo: Wire.ClientInfo? = nil,
        timeout: Duration = .seconds(15),
        peerName: String = "Your Mac",
        pairingStore: PairingStore = PairingStore()
    ) {
        self.connector = connector
        self.recordStore = recordStore
        self.identityStore = identityStore
        self.relayPasswordStore = relayPasswordStore
        self.clientInfo = clientInfo ?? Wire.ClientInfo(
            appVersion: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "1.0",
            platform: "ios",
            osVersion: nil
        )
        self.timeout = timeout
        self.peerName = peerName
        self.pairingStore = pairingStore
    }

    func pair(with input: PairingInput, relayPassword: String? = nil) async throws -> PairedDevice {
        let params = try resolve(input)

        // Normalize the captured relay password: a blank field means "no
        // password" (local/dev relay), which must present nothing — an
        // unconfigured relay rejects a present-but-empty value.
        let trimmedPassword = relayPassword?.trimmingCharacters(in: .whitespacesAndNewlines)
        let effectivePassword = (trimmedPassword?.isEmpty ?? true) ? nil : trimmedPassword

        // Fresh identity + key-agreement key (create-or-load).
        let identity: DeviceIdentity
        let keyAgreement: KeyAgreementKeys
        do {
            identity = try DeviceIdentity.loadOrCreate(store: identityStore)
            keyAgreement = try KeyAgreementKeys.loadOrCreate(store: identityStore)
        } catch {
            throw PairingError.unknown("Couldn't access the device keys.")
        }

        let channel: any WebSocketChannel
        do {
            channel = try await connector.connect(to: params.relayURL)
        } catch {
            throw PairingError.networkUnavailable
        }
        defer { Task { await channel.close() } }

        do {
            return try await runHandshake(
                channel: channel,
                params: params,
                identity: identity,
                keyAgreement: keyAgreement,
                relayPassword: effectivePassword
            )
        } catch let error as PairingError {
            throw error
        } catch let error as RelayConnectionError {
            switch error {
            case .closed, .notConnected: throw PairingError.networkUnavailable
            default: throw PairingError.unknown("Pairing failed. Try again.")
            }
        } catch is TimeoutError {
            throw PairingError.timedOut
        } catch {
            throw PairingError.unknown("Pairing failed. Try again.")
        }
    }

    // MARK: - Handshake

    private func runHandshake(
        channel: any WebSocketChannel,
        params: Params,
        identity: DeviceIdentity,
        keyAgreement: KeyAgreementKeys,
        relayPassword: String?
    ) async throws -> PairedDevice {
        try await channel.send(.hello(
            protocolVersion: Wire.protocolVersion,
            role: .phone,
            deviceId: Wire.DeviceId(identity.deviceId),
            client: clientInfo,
            relayPassword: relayPassword
        ))

        // hello_ok → auth_challenge (nonce). version_incompatible / error abort.
        _ = try await expect(channel) { if case .helloOk = $0 { return true }; return false }
        let nonce = try await expectChallenge(channel)

        // pairing_claim → pairing_claimed (or a rejection).
        try await channel.send(.pairingClaim(
            claimToken: params.claimToken,
            deviceId: Wire.DeviceId(identity.deviceId),
            devicePublicKey: identity.publicKeyBase64,
            keyAgreementPublicKey: keyAgreement.publicKeyBase64,
            role: .phone
        ))
        let claimed = try await expectClaimed(channel, input: params.input)

        // auth_response (sign the challenge) → auth_ok.
        let signature = try identity.signBase64(nonceBase64: nonce)
        try await channel.send(.authResponse(
            deviceId: Wire.DeviceId(identity.deviceId),
            signature: signature,
            pairingIds: [claimed.pairingId]
        ))
        _ = try await expect(channel) { if case .authOk = $0 { return true }; return false }

        guard let peerKA = claimed.peerKeyAgreementPublicKey, !peerKA.isEmpty else {
            // Without the desktop's KA key the E2E channel can't be derived.
            throw PairingError.unknown("The Mac didn't complete the secure handshake.")
        }

        let record = PairingRecord(
            pairingId: claimed.pairingId.rawValue,
            peerDeviceId: claimed.peerDeviceId?.rawValue,
            peerKeyAgreementPublicKeyB64: peerKA,
            saltB64: params.salt.base64EncodedString(),
            relayURL: params.relayURL.absoluteString
        )
        try? recordStore.save(record)

        // Persist the (accepted) shared relay password so every later reconnect
        // presents it (remote-control-uq7). Only reached after `auth_ok`, so the
        // relay accepted this password; nil clears any stale value (moving to a
        // no-password relay). Never logged.
        try? relayPasswordStore.save(relayPassword)

        // Multi-pairing (remote-control-b8d.4): APPEND this pairing's
        // display/prefs metadata rather than replacing whatever's already
        // paired. `machineNameFromDesktop` is nil until the first post-auth
        // connect reports it (remote-control-b8d.1/.9 wire the phone side of
        // that up); `PairedInstance.displayName` falls back sanely until then.
        pairingStore.add(PairedInstance(
            pairingId: claimed.pairingId.rawValue,
            relayURL: params.relayURL,
            pairedAt: record.pairedAt,
            lastKnownOnline: true
        ))

        return PairedDevice(
            pairingId: claimed.pairingId.rawValue,
            peerName: peerName,
            pairedAt: record.pairedAt
        )
    }

    // MARK: - Frame expectations

    private struct Claimed {
        let pairingId: Wire.PairingId
        let peerDeviceId: Wire.DeviceId?
        let peerKeyAgreementPublicKey: String?
    }

    /// Receive frames until `predicate` matches, mapping fatal frames to typed
    /// `PairingError`s. Bounded by the overall `timeout`.
    private func expect(
        _ channel: any WebSocketChannel,
        matching predicate: @escaping (Wire.RelayFrame) -> Bool
    ) async throws -> Wire.RelayFrame {
        while true {
            let frame = try await receive(channel)
            if predicate(frame) { return frame }
            try mapFatal(frame, input: nil)
            // Otherwise a benign interleaved frame; keep reading.
        }
    }

    private func expectChallenge(_ channel: any WebSocketChannel) async throws -> String {
        while true {
            let frame = try await receive(channel)
            if case let .authChallenge(nonce, _) = frame { return nonce }
            try mapFatal(frame, input: nil)
        }
    }

    private func expectClaimed(_ channel: any WebSocketChannel, input: PairingInput) async throws -> Claimed {
        while true {
            let frame = try await receive(channel)
            if case let .pairingClaimed(pairingId, peerDeviceId, peerKA) = frame {
                return Claimed(pairingId: pairingId, peerDeviceId: peerDeviceId, peerKeyAgreementPublicKey: peerKA)
            }
            try mapFatal(frame, input: input)
        }
    }

    /// Throw a typed error for a fatal relay frame; return normally otherwise.
    private func mapFatal(_ frame: Wire.RelayFrame, input: PairingInput?) throws {
        switch frame {
        case .versionIncompatible:
            throw PairingError.unknown("This app is out of date. Update to pair.")
        case let .error(code, _, _):
            switch code {
            case .pairingClaimRejected:
                if case .qr = input { throw PairingError.expiredOrUsedToken }
                throw PairingError.invalidCode
            case .authFailed:
                throw PairingError.unknown("The Mac rejected this device.")
            case .rateLimited:
                throw PairingError.unknown("Too many attempts. Wait a moment and try again.")
            default:
                throw PairingError.unknown("Pairing failed. Try again.")
            }
        case .bye:
            throw PairingError.networkUnavailable
        default:
            return
        }
    }

    private func receive(_ channel: any WebSocketChannel) async throws -> Wire.RelayFrame {
        try await withThrowingTaskGroup(of: Wire.RelayFrame.self) { group in
            group.addTask { try await channel.receive() }
            group.addTask {
                try await Task.sleep(for: timeout)
                throw TimeoutError()
            }
            guard let frame = try await group.next() else { throw TimeoutError() }
            group.cancelAll()
            return frame
        }
    }

    // MARK: - Input resolution

    private struct Params {
        let input: PairingInput
        let relayURL: URL
        let claimToken: String
        /// The E2E bootstrap salt bytes: ALWAYS the claim-token UTF-8 bytes,
        /// on both the QR and manual-code paths (§7.1, reconciled contract).
        var salt: Data { Data(claimToken.utf8) }
    }

    private func resolve(_ input: PairingInput) throws -> Params {
        switch input {
        case let .qr(payload):
            guard !payload.claimToken.isEmpty else { throw PairingError.malformedQRPayload }
            // The QR still carries `pairing_secret` (fdr1 wire-compat); validate
            // its shape so a truncated/corrupt QR is rejected honestly, but it
            // plays NO role in key derivation (§7.1: salt = claim-token bytes).
            guard let secret = Data(base64URLEncodedNoPadding: payload.pairingSecret), !secret.isEmpty else {
                throw PairingError.malformedQRPayload
            }
            return Params(input: input, relayURL: payload.relayURL, claimToken: payload.claimToken)
        case let .code(code, relayURL):
            let trimmed = code.trimmingCharacters(in: .whitespaces)
            guard !trimmed.isEmpty else { throw PairingError.invalidCode }
            // The 4-digit code IS the claim token.
            return Params(input: input, relayURL: relayURL, claimToken: trimmed)
        }
    }
}

/// Internal marker for a receive that exceeded the pairing deadline.
private struct TimeoutError: Error {}
