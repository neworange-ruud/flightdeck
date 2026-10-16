//
//  SealedPush.swift
//  FlightDeckRemote + FlightDeckNotificationService
//
//  Push payload sealing (REMOTE_PROTOCOL §7.2, remote-control-bza0). The relay
//  delivers a VISIBLE alert with generic text and `mutable-content: 1`; the
//  real content rides alongside, end-to-end encrypted by the desktop, in the
//  top-level `fd_push` key:
//
//      "fd_push": { "v": 1, "pairing_id": "...", "event_id": "...",
//                   "class": "needs_input"|"error"|"done",
//                   "nonce": "<b64 12 bytes>", "ciphertext": "<b64 ct‖tag>" }
//
//  The Notification Service Extension opens it with a key only the two paired
//  devices can derive:
//
//   - key   = HKDF-SHA256(ikm  = static-static P-256 ECDH x-coordinate (§7.1 step 1),
//                         salt = the pairing's claim-token UTF-8 bytes (§7.1 step 2),
//                         info = "flightdeck-remote-e2e-v1:" ‖ pairing_id ‖ ":push") → 32 bytes.
//             Direction-independent: the desktop seals, the phone opens.
//   - AEAD  = ChaCha20-Poly1305, ciphertext = AEAD output ‖ 16-byte tag.
//   - AAD   = "push:" ‖ pairing_id ‖ ":" ‖ event_id.
//   - plaintext = UTF-8 JSON `PushPayload` (`DecryptedPushPayload` here).
//
//  Byte-compatible with the desktop's `src/remote/crypto.rs`; the shared vectors
//  at `remote/protocol/tests/fixtures/e2e_crypto/push_vectors.json` are the
//  contract. Kept free of app types (no `Wire`) so the extension stays small.
//

import CryptoKit
import Foundation

/// Why a sealed push could not be opened. Every case means "show the generic
/// relay text unchanged" — the extension never drops a notification.
enum SealedPushError: Error, Equatable {
    /// No `fd_push` dictionary in the notification.
    case missingEnvelope
    /// `fd_push.v` is not a version this build understands.
    case unsupportedVersion
    /// A required `fd_push` field is missing, empty or the wrong type.
    case malformedEnvelope
    /// The ciphertext exceeds the protocol cap (relay rejects > 3000 chars).
    case ciphertextTooLarge
    /// A base64 field did not decode.
    case invalidBase64
    /// The nonce is not 12 bytes.
    case invalidNonceLength
    /// No pairing record for `pairing_id` on this device (unpaired, or the
    /// record was written outside the shared Keychain group).
    case unknownPairing
    /// The Keychain could not be read (e.g. before first unlock after boot).
    case keychainUnavailable
    /// The stored private scalar or peer public key did not parse.
    case invalidKeyMaterial
    /// AEAD open failed: wrong key/pairing, wrong AAD, or tampered ciphertext.
    case openFailed
    /// The plaintext is not a valid `PushPayload` JSON.
    case malformedPayload
    /// The plaintext names a different event than the envelope.
    case eventMismatch
}

/// The `fd_push` envelope as it arrives in the notification's `userInfo`.
struct SealedPushEnvelope: Equatable {
    /// The `userInfo` key the relay puts the envelope under.
    static let userInfoKey = "fd_push"
    /// The only envelope version this build opens.
    static let supportedVersion = 1
    /// Mirrors the relay's cap on the base64 `ciphertext` (§5.5). A 2048-byte
    /// plaintext seals to ~2752 base64 chars, so anything larger is not ours.
    static let maxCiphertextB64Length = 3000

    let pairingId: String
    let eventId: String
    /// The plaintext class hint (unauthenticated; the decrypted payload's own
    /// `class` is what counts).
    let classHint: String
    let nonceB64: String
    let ciphertextB64: String

    init(pairingId: String, eventId: String, classHint: String, nonceB64: String, ciphertextB64: String) {
        self.pairingId = pairingId
        self.eventId = eventId
        self.classHint = classHint
        self.nonceB64 = nonceB64
        self.ciphertextB64 = ciphertextB64
    }

    /// Parse the envelope out of a notification's `userInfo`.
    init(userInfo: [AnyHashable: Any]) throws {
        guard let raw = userInfo[Self.userInfoKey] as? [AnyHashable: Any] else {
            throw SealedPushError.missingEnvelope
        }
        guard let version = raw["v"] as? Int, version == Self.supportedVersion else {
            throw SealedPushError.unsupportedVersion
        }
        func field(_ key: String) throws -> String {
            guard let value = raw[key] as? String, !value.isEmpty else {
                throw SealedPushError.malformedEnvelope
            }
            return value
        }
        let ciphertext = try field("ciphertext")
        guard ciphertext.utf8.count <= Self.maxCiphertextB64Length else {
            throw SealedPushError.ciphertextTooLarge
        }
        self.init(
            pairingId: try field("pairing_id"),
            eventId: try field("event_id"),
            classHint: (raw["class"] as? String) ?? "",
            nonceB64: try field("nonce"),
            ciphertextB64: ciphertext)
    }
}

/// The decrypted `PushPayload` (spec §7.2).
struct DecryptedPushPayload: Decodable, Equatable {
    struct DeepLink: Decodable, Equatable {
        let projectId: String
        let sessionId: String
        let itemId: String?

        enum CodingKeys: String, CodingKey {
            case projectId = "project_id"
            case sessionId = "session_id"
            case itemId = "item_id"
        }
    }

    let eventId: String
    let eventClass: PushEventClass
    let title: String
    let body: String
    let deepLink: DeepLink
    let projectName: String?
    let occurredAtMs: Int64

    enum CodingKeys: String, CodingKey {
        case eventId = "event_id"
        case eventClass = "class"
        case title, body
        case deepLink = "deep_link"
        case projectName = "project_name"
        case occurredAtMs = "occurred_at_ms"
    }
}

/// The key material a pairing's push key is derived from.
struct PushKeyMaterial: Equatable {
    /// The phone's P-256 key-agreement private scalar (32 bytes raw).
    let privateScalar: Data
    /// The desktop's key-agreement public key, X9.63 (65 bytes).
    let peerPublicKeyX963: Data
    /// The pairing's bootstrap salt (the claim-token UTF-8 bytes).
    let salt: Data
}

/// Push key derivation + AEAD (spec §7.2).
enum PushPayloadCrypto {
    /// The protocol label, shared with the envelope channel (`E2EChannel`).
    static let infoPrefix = "flightdeck-remote-e2e-v1"
    static let nonceLength = 12
    static let tagLength = 16
    static let keyLength = 32

    /// The HKDF `info` for a pairing's push key.
    static func info(pairingId: String) -> Data {
        Data("\(infoPrefix):\(pairingId):push".utf8)
    }

    /// The AEAD associated data binding a ciphertext to its pairing and event.
    static func aad(pairingId: String, eventId: String) -> Data {
        Data("push:\(pairingId):\(eventId)".utf8)
    }

    /// Derive `pairingId`'s push key from this device's KA private scalar and
    /// the peer's KA public key.
    static func deriveKey(material: PushKeyMaterial, pairingId: String) throws -> SymmetricKey {
        let shared: SharedSecret
        do {
            let privateKey = try P256.KeyAgreement.PrivateKey(rawRepresentation: material.privateScalar)
            let peerKey = try P256.KeyAgreement.PublicKey(x963Representation: material.peerPublicKeyX963)
            shared = try privateKey.sharedSecretFromKeyAgreement(with: peerKey)
        } catch {
            throw SealedPushError.invalidKeyMaterial
        }
        // The raw shared secret is the big-endian x-coordinate — the same 32
        // bytes Rust's `SharedSecret::raw_secret_bytes()` yields.
        let ikm = shared.withUnsafeBytes { Data($0) }
        return HKDF<SHA256>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: ikm),
            salt: material.salt,
            info: info(pairingId: pairingId),
            outputByteCount: keyLength)
    }

    /// Open a sealed push payload. Throws a `SealedPushError` on any failure.
    static func open(
        nonceB64: String,
        ciphertextB64: String,
        key: SymmetricKey,
        pairingId: String,
        eventId: String
    ) throws -> Data {
        guard let nonceData = Data(base64Encoded: nonceB64),
              let sealed = Data(base64Encoded: ciphertextB64) else {
            throw SealedPushError.invalidBase64
        }
        guard nonceData.count == nonceLength else { throw SealedPushError.invalidNonceLength }
        guard sealed.count >= tagLength else { throw SealedPushError.openFailed }
        do {
            let box = try ChaChaPoly.SealedBox(
                nonce: try ChaChaPoly.Nonce(data: nonceData),
                ciphertext: sealed.prefix(sealed.count - tagLength),
                tag: sealed.suffix(tagLength))
            return try ChaChaPoly.open(box, using: key, authenticating: aad(pairingId: pairingId, eventId: eventId))
        } catch {
            throw SealedPushError.openFailed
        }
    }

    /// Seal a payload — the desktop's half, kept here so tests (and the vector
    /// check) can produce byte-exact ciphertexts. `nonce` defaults to fresh
    /// random bytes; pass one only for vectors.
    static func seal(
        _ plaintext: Data,
        key: SymmetricKey,
        pairingId: String,
        eventId: String,
        nonce: Data? = nil
    ) throws -> (nonceB64: String, ciphertextB64: String) {
        let chachaNonce = try nonce.map { try ChaChaPoly.Nonce(data: $0) } ?? ChaChaPoly.Nonce()
        let box = try ChaChaPoly.seal(
            plaintext, using: key, nonce: chachaNonce,
            authenticating: aad(pairingId: pairingId, eventId: eventId))
        var sealed = Data(box.ciphertext)
        sealed.append(box.tag)
        return (Data(chachaNonce).base64EncodedString(), sealed.base64EncodedString())
    }

    /// Envelope → validated payload: derive, open, decode, cross-check.
    static func openEnvelope(_ envelope: SealedPushEnvelope, material: PushKeyMaterial) throws -> DecryptedPushPayload {
        let key = try deriveKey(material: material, pairingId: envelope.pairingId)
        let plaintext = try open(
            nonceB64: envelope.nonceB64,
            ciphertextB64: envelope.ciphertextB64,
            key: key,
            pairingId: envelope.pairingId,
            eventId: envelope.eventId)
        let payload: DecryptedPushPayload
        do {
            payload = try JSONDecoder().decode(DecryptedPushPayload.self, from: plaintext)
        } catch {
            throw SealedPushError.malformedPayload
        }
        // The AAD already binds the event id; this catches a desktop bug that
        // sealed one event's payload under another's id.
        guard payload.eventId == envelope.eventId else { throw SealedPushError.eventMismatch }
        guard !payload.deepLink.projectId.isEmpty, !payload.deepLink.sessionId.isEmpty else {
            throw SealedPushError.malformedPayload
        }
        return payload
    }
}

/// Where the extension finds a pairing's key material. A seam so the rewrite
/// logic is testable without a Keychain.
protocol PushKeyMaterialProviding {
    /// The key material for `pairingId`, or `nil` if this device has no such
    /// pairing. Throws when the store itself cannot be read.
    func keyMaterial(pairingId: String) throws -> PushKeyMaterial?
}

/// The real provider: the pairing record and the KA private key, read (never
/// written) from the shared Keychain group.
struct KeychainPushKeyMaterialProvider: PushKeyMaterialProviding {
    private let store: KeychainStoring

    init(store: KeychainStoring = KeychainStore(service: SharedContainer.keychainService)) {
        self.store = store
    }

    func keyMaterial(pairingId: String) throws -> PushKeyMaterial? {
        guard let record = try PairingRecordStore(store: store).peek(pairingId: pairingId) else {
            return nil
        }
        guard let scalar = try store.get(account: SharedContainer.keyAgreementAccount),
              let peerKey = Data(base64Encoded: record.peerKeyAgreementPublicKeyB64) else {
            throw SealedPushError.invalidKeyMaterial
        }
        return PushKeyMaterial(privateScalar: scalar, peerPublicKeyX963: peerKey, salt: record.salt)
    }
}
