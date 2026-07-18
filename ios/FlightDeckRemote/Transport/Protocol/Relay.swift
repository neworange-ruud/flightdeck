//
//  Relay.swift
//  FlightDeckRemote
//
//  Swift mirror of `remote/protocol/src/relay.rs`: the relay plane —
//  plaintext, content-free frames exchanged between an endpoint and the
//  hosted relay (version negotiation, auth, pairing bootstrap, presence,
//  queued delivery, acks, ping/pong, push tokens, errors). Application
//  content only ever travels inside the opaque `EncryptedEnvelope`.
//
//  `RelayFrame` is internally tagged by `type` with flattened variant fields
//  (spec §3), so its Codable is written by hand; the `envelope` newtype
//  variant flattens `EncryptedEnvelope`'s fields next to the tag. Optionals
//  are emitted as explicit `null` (see Common.swift).
//

import Foundation

extension Wire {

    // MARK: - Enums

    /// Presence of the peer endpoint for a pairing.
    enum PresenceState: String, Codable, Hashable, Sendable {
        case connected
        case disconnected
    }

    /// APNs environment a push token belongs to.
    enum ApnsEnvironment: String, Codable, Hashable, Sendable {
        case sandbox
        case production
    }

    /// Machine-readable relay error codes.
    enum RelayErrorCode: String, Codable, Hashable, Sendable {
        case unsupportedVersion = "unsupported_version"
        case authFailed = "auth_failed"
        case unknownPairing = "unknown_pairing"
        case notAuthenticated = "not_authenticated"
        case pairingClaimRejected = "pairing_claim_rejected"
        case peerUnavailable = "peer_unavailable"
        case rateLimited = "rate_limited"
        case badFrame = "bad_frame"
        /// The sender's envelope `seq` diverged from the relay's expected next
        /// value — usually the relay lost its in-memory watermark across a
        /// restart while we kept our persisted cursor. Unlike `badFrame` this is
        /// recoverable: the endpoint re-syncs its stream (restart from seq 1)
        /// instead of tearing the link down (remote-control-bbf).
        case seqViolation = "seq_violation"
        case internalError = "internal"
        /// A code this build does not recognize — a newer relay sent a code added
        /// after this app shipped. Decoding maps unknown wire values here so an
        /// error frame never fails to parse; treated as a non-fatal advisory.
        case unknown

        init(from decoder: Decoder) throws {
            let raw = try decoder.singleValueContainer().decode(String.self)
            self = RelayErrorCode(rawValue: raw) ?? .unknown
        }
    }

    // MARK: - Structs

    /// Non-secret client build metadata, sent in `hello` for diagnostics.
    struct ClientInfo: Codable, Hashable, Sendable {
        /// App/build version string, e.g. `1.7.1`.
        var appVersion: String
        /// Platform, e.g. `ios`, `macos`.
        var platform: String
        /// OS version string, if known.
        var osVersion: String?

        private enum CodingKeys: String, CodingKey {
            case platform
            case appVersion = "app_version"
            case osVersion = "os_version"
        }

        func encode(to encoder: Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            try container.encode(appVersion, forKey: .appVersion)
            try container.encode(platform, forKey: .platform)
            try container.encode(osVersion, forKey: .osVersion) // explicit null
        }
    }

    /// An opaque, end-to-end-encrypted application payload. The relay routes
    /// it by `pairingId` and never decrypts it. The header fields are bound
    /// into the AEAD's AAD by the sealing layer (spec §7.1).
    struct EncryptedEnvelope: Codable, Hashable, Sendable {
        /// Pairing this payload belongs to.
        var pairingId: PairingId
        /// Monotonic per-pairing, per-sender sequence number (starts at 1).
        var seq: UInt64
        /// Which role sealed this payload.
        var sender: Role
        /// Sender's wall-clock time (unix milliseconds) when sealed.
        var sentAtMs: Int64
        /// Base64 (standard, padded) AEAD nonce.
        var nonce: String
        /// Base64 (standard, padded) ciphertext of a serialized E2E message
        /// (`DesktopToPhone` or `PhoneCommand`).
        var ciphertext: String

        private enum CodingKeys: String, CodingKey {
            case seq, sender, nonce, ciphertext
            case pairingId = "pairing_id"
            case sentAtMs = "sent_at_ms"
        }
    }

    // MARK: - Relay frame

    /// A single frame on the relay plane. Internally tagged by `type`.
    enum RelayFrame: Codable, Hashable, Sendable {
        /// endpoint -> relay. First frame; opens version negotiation.
        case hello(protocolVersion: UInt16, role: Role, deviceId: DeviceId,
                   client: ClientInfo)
        /// relay -> endpoint. Accepts the connection at a negotiated version.
        case helloOk(protocolVersion: UInt16, serverTimeMs: Int64,
                     connectionId: String)
        /// relay -> endpoint. Version outside supported range; socket closes.
        case versionIncompatible(yourVersion: UInt16, minSupported: UInt16,
                                 maxSupported: UInt16)
        /// relay -> endpoint. Challenge nonce to sign with the identity key.
        case authChallenge(nonce: String, serverTimeMs: Int64)
        /// endpoint -> relay. Signature over the nonce + pairings to activate.
        case authResponse(deviceId: DeviceId, signature: String,
                          pairingIds: [PairingId])
        /// relay -> endpoint. Authentication succeeded; pairings are active.
        case authOk(pairingIds: [PairingId])
        /// endpoint (desktop) -> relay. Mint a claim token for pairing.
        case pairingOffer(deviceId: DeviceId, devicePublicKey: String,
                          keyAgreementPublicKey: String, role: Role)
        /// relay -> endpoint (desktop). Pairing provisioned; token to display.
        case pairingOfferOk(pairingId: PairingId, claimToken: String,
                            expiresAtMs: Int64)
        /// endpoint -> relay. Redeem the code/QR token shown on the desktop.
        case pairingClaim(claimToken: String, deviceId: DeviceId,
                          devicePublicKey: String,
                          keyAgreementPublicKey: String, role: Role)
        /// relay -> endpoint. Pairing bootstrap succeeded.
        case pairingClaimed(pairingId: PairingId, peerDeviceId: DeviceId?,
                            peerKeyAgreementPublicKey: String?)
        /// relay -> endpoint. The peer connected or disconnected.
        case peerPresence(pairingId: PairingId, peer: Role,
                          state: PresenceState, atMs: Int64)
        /// Both directions. Carries an opaque E2E payload (fields flattened).
        case envelope(EncryptedEnvelope)
        /// Both directions. Cumulative ack up to and including `cursor`.
        case ack(pairingId: PairingId, cursor: UInt64)
        /// endpoint -> relay. Replay queued envelopes with `seq > from_seq`.
        case resume(pairingId: PairingId, fromSeq: UInt64)
        /// endpoint -> relay. Latency probe.
        case ping(clientTimeMs: Int64)
        /// relay -> endpoint. Echo + relay time.
        case pong(clientTimeMs: Int64, serverTimeMs: Int64)
        /// phone -> relay. Registers/refreshes the APNs token for a pairing.
        case registerPushToken(pairingId: PairingId, token: String,
                               environment: ApnsEnvironment)
        /// relay -> endpoint. Confirms a push token was stored.
        case pushTokenAck(pairingId: PairingId)
        /// relay -> endpoint. A relay-plane error.
        case error(code: RelayErrorCode, message: String, pairingId: PairingId?)
        /// Both directions. Graceful shutdown notice.
        case bye(reason: String?)

        private enum CodingKeys: String, CodingKey {
            case type, role, client, nonce, signature, peer, state, cursor
            case token, environment, code, message, reason
            case protocolVersion = "protocol_version"
            case deviceId = "device_id"
            case serverTimeMs = "server_time_ms"
            case connectionId = "connection_id"
            case yourVersion = "your_version"
            case minSupported = "min_supported"
            case maxSupported = "max_supported"
            case pairingIds = "pairing_ids"
            case devicePublicKey = "device_public_key"
            case keyAgreementPublicKey = "key_agreement_public_key"
            case pairingId = "pairing_id"
            case claimToken = "claim_token"
            case expiresAtMs = "expires_at_ms"
            case peerDeviceId = "peer_device_id"
            case peerKeyAgreementPublicKey = "peer_key_agreement_public_key"
            case atMs = "at_ms"
            case fromSeq = "from_seq"
            case clientTimeMs = "client_time_ms"
        }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let type = try c.decode(String.self, forKey: .type)
            switch type {
            case "hello":
                self = .hello(
                    protocolVersion: try c.decode(UInt16.self, forKey: .protocolVersion),
                    role: try c.decode(Role.self, forKey: .role),
                    deviceId: try c.decode(DeviceId.self, forKey: .deviceId),
                    client: try c.decode(ClientInfo.self, forKey: .client))
            case "hello_ok":
                self = .helloOk(
                    protocolVersion: try c.decode(UInt16.self, forKey: .protocolVersion),
                    serverTimeMs: try c.decode(Int64.self, forKey: .serverTimeMs),
                    connectionId: try c.decode(String.self, forKey: .connectionId))
            case "version_incompatible":
                self = .versionIncompatible(
                    yourVersion: try c.decode(UInt16.self, forKey: .yourVersion),
                    minSupported: try c.decode(UInt16.self, forKey: .minSupported),
                    maxSupported: try c.decode(UInt16.self, forKey: .maxSupported))
            case "auth_challenge":
                self = .authChallenge(
                    nonce: try c.decode(String.self, forKey: .nonce),
                    serverTimeMs: try c.decode(Int64.self, forKey: .serverTimeMs))
            case "auth_response":
                self = .authResponse(
                    deviceId: try c.decode(DeviceId.self, forKey: .deviceId),
                    signature: try c.decode(String.self, forKey: .signature),
                    pairingIds: try c.decode([PairingId].self, forKey: .pairingIds))
            case "auth_ok":
                self = .authOk(
                    pairingIds: try c.decode([PairingId].self, forKey: .pairingIds))
            case "pairing_offer":
                self = .pairingOffer(
                    deviceId: try c.decode(DeviceId.self, forKey: .deviceId),
                    devicePublicKey: try c.decode(String.self, forKey: .devicePublicKey),
                    keyAgreementPublicKey: try c.decode(String.self, forKey: .keyAgreementPublicKey),
                    role: try c.decode(Role.self, forKey: .role))
            case "pairing_offer_ok":
                self = .pairingOfferOk(
                    pairingId: try c.decode(PairingId.self, forKey: .pairingId),
                    claimToken: try c.decode(String.self, forKey: .claimToken),
                    expiresAtMs: try c.decode(Int64.self, forKey: .expiresAtMs))
            case "pairing_claim":
                self = .pairingClaim(
                    claimToken: try c.decode(String.self, forKey: .claimToken),
                    deviceId: try c.decode(DeviceId.self, forKey: .deviceId),
                    devicePublicKey: try c.decode(String.self, forKey: .devicePublicKey),
                    keyAgreementPublicKey: try c.decode(String.self, forKey: .keyAgreementPublicKey),
                    role: try c.decode(Role.self, forKey: .role))
            case "pairing_claimed":
                self = .pairingClaimed(
                    pairingId: try c.decode(PairingId.self, forKey: .pairingId),
                    peerDeviceId: try c.decodeIfPresent(DeviceId.self, forKey: .peerDeviceId),
                    peerKeyAgreementPublicKey: try c.decodeIfPresent(
                        String.self, forKey: .peerKeyAgreementPublicKey))
            case "peer_presence":
                self = .peerPresence(
                    pairingId: try c.decode(PairingId.self, forKey: .pairingId),
                    peer: try c.decode(Role.self, forKey: .peer),
                    state: try c.decode(PresenceState.self, forKey: .state),
                    atMs: try c.decode(Int64.self, forKey: .atMs))
            case "envelope":
                self = .envelope(try EncryptedEnvelope(from: decoder)) // flattened
            case "ack":
                self = .ack(
                    pairingId: try c.decode(PairingId.self, forKey: .pairingId),
                    cursor: try c.decode(UInt64.self, forKey: .cursor))
            case "resume":
                self = .resume(
                    pairingId: try c.decode(PairingId.self, forKey: .pairingId),
                    fromSeq: try c.decode(UInt64.self, forKey: .fromSeq))
            case "ping":
                self = .ping(
                    clientTimeMs: try c.decode(Int64.self, forKey: .clientTimeMs))
            case "pong":
                self = .pong(
                    clientTimeMs: try c.decode(Int64.self, forKey: .clientTimeMs),
                    serverTimeMs: try c.decode(Int64.self, forKey: .serverTimeMs))
            case "register_push_token":
                self = .registerPushToken(
                    pairingId: try c.decode(PairingId.self, forKey: .pairingId),
                    token: try c.decode(String.self, forKey: .token),
                    environment: try c.decode(ApnsEnvironment.self, forKey: .environment))
            case "push_token_ack":
                self = .pushTokenAck(
                    pairingId: try c.decode(PairingId.self, forKey: .pairingId))
            case "error":
                self = .error(
                    code: try c.decode(RelayErrorCode.self, forKey: .code),
                    message: try c.decode(String.self, forKey: .message),
                    pairingId: try c.decodeIfPresent(PairingId.self, forKey: .pairingId))
            case "bye":
                self = .bye(reason: try c.decodeIfPresent(String.self, forKey: .reason))
            default:
                throw DecodingError.dataCorruptedError(
                    forKey: .type, in: c,
                    debugDescription: "unknown relay frame type: \(type)")
            }
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .hello(protocolVersion, role, deviceId, client):
                try c.encode("hello", forKey: .type)
                try c.encode(protocolVersion, forKey: .protocolVersion)
                try c.encode(role, forKey: .role)
                try c.encode(deviceId, forKey: .deviceId)
                try c.encode(client, forKey: .client)
            case let .helloOk(protocolVersion, serverTimeMs, connectionId):
                try c.encode("hello_ok", forKey: .type)
                try c.encode(protocolVersion, forKey: .protocolVersion)
                try c.encode(serverTimeMs, forKey: .serverTimeMs)
                try c.encode(connectionId, forKey: .connectionId)
            case let .versionIncompatible(yourVersion, minSupported, maxSupported):
                try c.encode("version_incompatible", forKey: .type)
                try c.encode(yourVersion, forKey: .yourVersion)
                try c.encode(minSupported, forKey: .minSupported)
                try c.encode(maxSupported, forKey: .maxSupported)
            case let .authChallenge(nonce, serverTimeMs):
                try c.encode("auth_challenge", forKey: .type)
                try c.encode(nonce, forKey: .nonce)
                try c.encode(serverTimeMs, forKey: .serverTimeMs)
            case let .authResponse(deviceId, signature, pairingIds):
                try c.encode("auth_response", forKey: .type)
                try c.encode(deviceId, forKey: .deviceId)
                try c.encode(signature, forKey: .signature)
                try c.encode(pairingIds, forKey: .pairingIds)
            case let .authOk(pairingIds):
                try c.encode("auth_ok", forKey: .type)
                try c.encode(pairingIds, forKey: .pairingIds)
            case let .pairingOffer(deviceId, devicePublicKey, keyAgreementPublicKey, role):
                try c.encode("pairing_offer", forKey: .type)
                try c.encode(deviceId, forKey: .deviceId)
                try c.encode(devicePublicKey, forKey: .devicePublicKey)
                try c.encode(keyAgreementPublicKey, forKey: .keyAgreementPublicKey)
                try c.encode(role, forKey: .role)
            case let .pairingOfferOk(pairingId, claimToken, expiresAtMs):
                try c.encode("pairing_offer_ok", forKey: .type)
                try c.encode(pairingId, forKey: .pairingId)
                try c.encode(claimToken, forKey: .claimToken)
                try c.encode(expiresAtMs, forKey: .expiresAtMs)
            case let .pairingClaim(claimToken, deviceId, devicePublicKey,
                                   keyAgreementPublicKey, role):
                try c.encode("pairing_claim", forKey: .type)
                try c.encode(claimToken, forKey: .claimToken)
                try c.encode(deviceId, forKey: .deviceId)
                try c.encode(devicePublicKey, forKey: .devicePublicKey)
                try c.encode(keyAgreementPublicKey, forKey: .keyAgreementPublicKey)
                try c.encode(role, forKey: .role)
            case let .pairingClaimed(pairingId, peerDeviceId, peerKeyAgreementPublicKey):
                try c.encode("pairing_claimed", forKey: .type)
                try c.encode(pairingId, forKey: .pairingId)
                try c.encode(peerDeviceId, forKey: .peerDeviceId) // explicit null
                try c.encode(peerKeyAgreementPublicKey,
                             forKey: .peerKeyAgreementPublicKey) // explicit null
            case let .peerPresence(pairingId, peer, state, atMs):
                try c.encode("peer_presence", forKey: .type)
                try c.encode(pairingId, forKey: .pairingId)
                try c.encode(peer, forKey: .peer)
                try c.encode(state, forKey: .state)
                try c.encode(atMs, forKey: .atMs)
            case let .envelope(envelope):
                try c.encode("envelope", forKey: .type)
                try envelope.encode(to: encoder) // flattened
            case let .ack(pairingId, cursor):
                try c.encode("ack", forKey: .type)
                try c.encode(pairingId, forKey: .pairingId)
                try c.encode(cursor, forKey: .cursor)
            case let .resume(pairingId, fromSeq):
                try c.encode("resume", forKey: .type)
                try c.encode(pairingId, forKey: .pairingId)
                try c.encode(fromSeq, forKey: .fromSeq)
            case let .ping(clientTimeMs):
                try c.encode("ping", forKey: .type)
                try c.encode(clientTimeMs, forKey: .clientTimeMs)
            case let .pong(clientTimeMs, serverTimeMs):
                try c.encode("pong", forKey: .type)
                try c.encode(clientTimeMs, forKey: .clientTimeMs)
                try c.encode(serverTimeMs, forKey: .serverTimeMs)
            case let .registerPushToken(pairingId, token, environment):
                try c.encode("register_push_token", forKey: .type)
                try c.encode(pairingId, forKey: .pairingId)
                try c.encode(token, forKey: .token)
                try c.encode(environment, forKey: .environment)
            case let .pushTokenAck(pairingId):
                try c.encode("push_token_ack", forKey: .type)
                try c.encode(pairingId, forKey: .pairingId)
            case let .error(code, message, pairingId):
                try c.encode("error", forKey: .type)
                try c.encode(code, forKey: .code)
                try c.encode(message, forKey: .message)
                try c.encode(pairingId, forKey: .pairingId) // explicit null
            case let .bye(reason):
                try c.encode("bye", forKey: .type)
                try c.encode(reason, forKey: .reason) // explicit null
            }
        }
    }
}
