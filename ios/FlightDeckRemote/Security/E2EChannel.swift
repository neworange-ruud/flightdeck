//
//  E2EChannel.swift
//  FlightDeckRemote
//
//  The end-to-end channel crypto (REMOTE_PROTOCOL §7). This is the encryption
//  layer the relay cannot see into: it turns the pairing bootstrap material
//  (§5.2) plus the two devices' P-256 identity keys into a pair of directional
//  AEAD keys, then seals/opens the per-message payloads carried inside an
//  EncryptedEnvelope. The Swift construction is byte-compatible with the Rust
//  implementation in `src/remote/crypto.rs`; the shared vectors at
//  `remote/protocol/tests/fixtures/e2e_crypto/vectors.json` are the contract.
//
//  Scheme (pinned in spec §7):
//   1. IKM  = P-256 static-static ECDH shared-secret x-coordinate (32 bytes),
//             between this device's key-agreement key and the peer's
//             key-agreement key (exchanged at pairing; the desktop reuses its
//             identity key, the phone uses a separate software key because SE
//             signing keys can't ECDH). (v1 uses long-lived keys directly →
//             NO forward secrecy; key rotation is a deferred item, PRD §13.)
//   2. salt = ALWAYS the claim-token UTF-8 bytes, on both the QR and the
//             manual-code pairing paths (spec §7.1 — the desktop cannot
//             observe which path the phone used, so the salt must be
//             path-independent; the QR's `pairing_secret` is wire-compat
//             only and is NOT used in derivation).
//   3. KDF  = HKDF-SHA256(ikm, salt), expanded once per direction:
//             info = "flightdeck-remote-e2e-v1:" + pairingID + ":d2p" | ":p2d".
//   4. AEAD = ChaCha20-Poly1305, fresh random 12-byte nonce per message, AAD =
//             utf8(pairingID + ":" + seq + ":" + sender + ":" + sentAtMs).
//
//  NOTE on the identity key source: CryptoKit's Secure Enclave *signing* keys
//  cannot be reused for key agreement (the scalar is non-extractable). The
//  pairing-flow layer that consumes this API is responsible for providing a
//  P-256 key-agreement private scalar; this type takes raw key material and
//  performs no Keychain / Secure Enclave access itself.
//

import Foundation
import CryptoKit

/// Errors from deriving or using an ``E2EChannel``.
enum E2EChannelError: Error, Equatable {
    /// The private scalar or peer public key could not be parsed.
    case invalidKeyMaterial
    /// A base64 field on the wire could not be decoded.
    case invalidBase64
    /// The nonce was not the required 12 bytes.
    case invalidNonceLength
    /// AEAD open failed: wrong key, tampered ciphertext, or mismatched header.
    case openFailed
}

/// A derived, ready-to-use end-to-end channel for one pairing on one endpoint.
struct E2EChannel {
    /// The fixed protocol label mixed into every derived key (spec §7).
    static let infoPrefix = "flightdeck-remote-e2e-v1"
    /// ChaCha20-Poly1305 nonce length.
    static let nonceLength = 12
    /// Derived AEAD key length.
    static let keyLength = 32

    /// Which endpoint owns this channel. Its raw value matches the wire
    /// `sender` spelling (`desktop` / `phone`) used in the AAD.
    enum Role: String {
        case desktop
        case phone
    }

    private let role: Role
    private let pairingID: String
    private let d2pKey: SymmetricKey
    private let p2dKey: SymmetricKey

    // MARK: - Derivation

    /// Derive the channel from the local device's P-256 private scalar (32-byte
    /// raw representation), the peer's X9.63 public key (65 bytes, `0x04 ‖ x ‖ y`),
    /// the `pairingID`, the bootstrap `salt`, and this endpoint's `role`.
    static func derive(
        identityPrivateScalar: Data,
        peerPublicKeyX963: Data,
        pairingID: String,
        salt: Data,
        role: Role
    ) throws -> E2EChannel {
        let privateKey: P256.KeyAgreement.PrivateKey
        let peerKey: P256.KeyAgreement.PublicKey
        do {
            privateKey = try P256.KeyAgreement.PrivateKey(rawRepresentation: identityPrivateScalar)
            peerKey = try P256.KeyAgreement.PublicKey(x963Representation: peerPublicKeyX963)
        } catch {
            throw E2EChannelError.invalidKeyMaterial
        }

        let shared: SharedSecret
        do {
            shared = try privateKey.sharedSecretFromKeyAgreement(with: peerKey)
        } catch {
            throw E2EChannelError.invalidKeyMaterial
        }
        // The raw shared-secret bytes are the big-endian x-coordinate — the same
        // 32 bytes the Rust `SharedSecret::raw_secret_bytes()` yields.
        let ikm = shared.withUnsafeBytes { Data($0) }

        let d2pKey = Self.expand(ikm: ikm, salt: salt, pairingID: pairingID, direction: "d2p")
        let p2dKey = Self.expand(ikm: ikm, salt: salt, pairingID: pairingID, direction: "p2d")
        return E2EChannel(role: role, pairingID: pairingID, d2pKey: d2pKey, p2dKey: p2dKey)
    }

    private static func expand(
        ikm: Data,
        salt: Data,
        pairingID: String,
        direction: String
    ) -> SymmetricKey {
        let info = Data("\(infoPrefix):\(pairingID):\(direction)".utf8)
        return HKDF<SHA256>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: ikm),
            salt: salt,
            info: info,
            outputByteCount: keyLength
        )
    }

    // MARK: - Seal / open

    /// Seal an outgoing plaintext payload with a fresh random nonce. Returns
    /// `(nonceB64, ciphertextB64)` ready for an EncryptedEnvelope; `ciphertext`
    /// is the AEAD output with its 16-byte tag appended.
    func seal(_ plaintext: Data, seq: UInt64, sentAtMs: Int64) throws -> (nonceB64: String, ciphertextB64: String) {
        let nonce = Data(ChaChaPoly.Nonce()) // CryptoKit generates a random 12-byte nonce.
        return try sealWithNonce(plaintext, seq: seq, sentAtMs: sentAtMs, nonce: nonce)
    }

    /// Seal with a caller-supplied nonce. **Test/vector hook only** — production
    /// code must use ``seal(_:seq:sentAtMs:)`` so every message gets a fresh
    /// random nonce.
    func sealWithNonce(
        _ plaintext: Data,
        seq: UInt64,
        sentAtMs: Int64,
        nonce: Data
    ) throws -> (nonceB64: String, ciphertextB64: String) {
        guard nonce.count == Self.nonceLength else { throw E2EChannelError.invalidNonceLength }
        let key = keyForSender(role)
        let aad = Data(Self.headerAAD(pairingID: pairingID, seq: seq, sender: role, sentAtMs: sentAtMs).utf8)
        let chachaNonce: ChaChaPoly.Nonce
        do {
            chachaNonce = try ChaChaPoly.Nonce(data: nonce)
        } catch {
            throw E2EChannelError.invalidNonceLength
        }
        let box = try ChaChaPoly.seal(plaintext, using: key, nonce: chachaNonce, authenticating: aad)
        // Rust emits ciphertext ‖ tag in the `ciphertext` field; CryptoKit keeps
        // them separate, so concatenate to match the wire form.
        var ctAndTag = Data(box.ciphertext)
        ctAndTag.append(box.tag)
        return (nonce.base64EncodedString(), ctAndTag.base64EncodedString())
    }

    /// Open an incoming envelope. `sender` is the envelope's `sender` field (the
    /// peer's role), which selects the receive key and is bound into the AAD.
    /// Throws ``E2EChannelError/openFailed`` if the key is wrong, the ciphertext
    /// was tampered with, or any AAD header field does not match what was sealed.
    func open(
        seq: UInt64,
        sender: Role,
        sentAtMs: Int64,
        nonceB64: String,
        ciphertextB64: String
    ) throws -> Data {
        guard let nonceData = Data(base64Encoded: nonceB64) else { throw E2EChannelError.invalidBase64 }
        guard nonceData.count == Self.nonceLength else { throw E2EChannelError.invalidNonceLength }
        guard let ctAndTag = Data(base64Encoded: ciphertextB64) else { throw E2EChannelError.invalidBase64 }
        guard ctAndTag.count >= 16 else { throw E2EChannelError.openFailed }

        let key = keyForSender(sender)
        let aad = Data(Self.headerAAD(pairingID: pairingID, seq: seq, sender: sender, sentAtMs: sentAtMs).utf8)
        let ciphertext = Data(ctAndTag.prefix(ctAndTag.count - 16))
        let tag = Data(ctAndTag.suffix(16))

        do {
            let nonce = try ChaChaPoly.Nonce(data: nonceData)
            let box = try ChaChaPoly.SealedBox(nonce: nonce, ciphertext: ciphertext, tag: tag)
            return try ChaChaPoly.open(box, using: key, authenticating: aad)
        } catch {
            throw E2EChannelError.openFailed
        }
    }

    // MARK: - Helpers

    /// The canonical AAD string bound to every message (spec §7).
    static func headerAAD(pairingID: String, seq: UInt64, sender: Role, sentAtMs: Int64) -> String {
        "\(pairingID):\(seq):\(sender.rawValue):\(sentAtMs)"
    }

    /// The directional key a given sender's messages are encrypted under.
    private func keyForSender(_ sender: Role) -> SymmetricKey {
        switch sender {
        case .desktop: return d2pKey
        case .phone: return p2dKey
        }
    }

    /// Test-only: the raw derived keys, so the cross-language proof can assert
    /// them directly against the Rust-generated vectors. Not part of the API the
    /// pairing flow uses.
    func derivedKeysForCrossLanguageProof() -> (d2p: Data, p2d: Data) {
        (d2pKey.withUnsafeBytes { Data($0) }, p2dKey.withUnsafeBytes { Data($0) })
    }
}
