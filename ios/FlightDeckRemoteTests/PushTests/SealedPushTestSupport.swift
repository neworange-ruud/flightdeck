//
//  SealedPushTestSupport.swift
//  FlightDeckRemoteTests
//
//  Builders for the sealed-push tests (remote-control-bza0): a fixed desktop ↔
//  phone key pair, a desktop-side sealer written directly against CryptoKit
//  (independent of `PushPayloadCrypto`, so the open path is checked against a
//  second implementation of the spec), and `fd_push` userInfo builders.
//

import CryptoKit
import Foundation
@testable import FlightDeckRemote

enum SealedPushFixtures {
    static let pairingId = "pair_push_000001"
    static let eventId = "8d3f6a52-1c2b-4e5f-9a7b-0c1d2e3f4a5b"
    static let salt = Data("4729-claim-token".utf8)

    static let desktopScalar = Data((1...32).map { UInt8($0) })
    static let phoneScalar = Data((1...32).reversed().map { UInt8($0) })

    static var desktopPublicX963: Data {
        try! P256.KeyAgreement.PrivateKey(rawRepresentation: desktopScalar).publicKey.x963Representation
    }

    static var phonePublicX963: Data {
        try! P256.KeyAgreement.PrivateKey(rawRepresentation: phoneScalar).publicKey.x963Representation
    }

    /// What the phone's extension holds for the fixture pairing.
    static var phoneMaterial: PushKeyMaterial {
        PushKeyMaterial(privateScalar: phoneScalar, peerPublicKeyX963: desktopPublicX963, salt: salt)
    }

    /// The desktop's view of the same pairing.
    static var desktopMaterial: PushKeyMaterial {
        PushKeyMaterial(privateScalar: desktopScalar, peerPublicKeyX963: phonePublicX963, salt: salt)
    }

    /// The desktop's push key, computed straight from the spec text with
    /// CryptoKit (not through `PushPayloadCrypto`).
    static func desktopKey(pairingId: String = pairingId, salt: Data = salt) throws -> SymmetricKey {
        let priv = try P256.KeyAgreement.PrivateKey(rawRepresentation: desktopScalar)
        let peer = try P256.KeyAgreement.PublicKey(x963Representation: phonePublicX963)
        let shared = try priv.sharedSecretFromKeyAgreement(with: peer)
        let ikm = shared.withUnsafeBytes { Data($0) }
        return HKDF<SHA256>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: ikm),
            salt: salt,
            info: Data("flightdeck-remote-e2e-v1:\(pairingId):push".utf8),
            outputByteCount: 32)
    }

    /// A `PushPayload` JSON plaintext, as the desktop serializes it.
    static func payloadJSON(
        eventId: String = eventId,
        eventClass: String = "needs_input",
        title: String = "fd-api needs input",
        body: String = "Allow `rm -rf dist/`?",
        projectId: String = "proj_1",
        sessionId: String = "sess_1",
        itemId: String? = "item_9",
        projectName: String? = "fd-api"
    ) -> Data {
        var link: [String: Any] = ["project_id": projectId, "session_id": sessionId]
        link["item_id"] = itemId ?? NSNull()
        var object: [String: Any] = [
            "event_id": eventId,
            "class": eventClass,
            "title": title,
            "body": body,
            "deep_link": link,
            "occurred_at_ms": 1_752_412_802_000,
        ]
        if let projectName { object["project_name"] = projectName }
        return try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
    }

    /// Seal like the desktop does: ChaCha20-Poly1305, AAD
    /// `push:<pairing>:<event>`, ciphertext ‖ tag, both base64.
    static func desktopSeal(
        _ plaintext: Data,
        pairingId: String = pairingId,
        eventId: String = eventId,
        aadEventId: String? = nil,
        key: SymmetricKey? = nil
    ) throws -> (nonce: String, ciphertext: String) {
        let key = try key ?? desktopKey(pairingId: pairingId)
        let aad = Data("push:\(pairingId):\(aadEventId ?? eventId)".utf8)
        let box = try ChaChaPoly.seal(plaintext, using: key, authenticating: aad)
        var sealed = Data(box.ciphertext)
        sealed.append(box.tag)
        return (Data(box.nonce).base64EncodedString(), sealed.base64EncodedString())
    }

    /// The `userInfo` of the APNs alert the relay builds (spec C3).
    static func userInfo(
        pairingId: String = pairingId,
        eventId: String = eventId,
        eventClass: String = "needs_input",
        nonce: String,
        ciphertext: String,
        version: Any = 1
    ) -> [AnyHashable: Any] {
        [
            "aps": [
                "alert": ["title": "FlightDeck", "body": "An agent needs your input"],
                "mutable-content": 1,
                "sound": "default",
                "interruption-level": "time-sensitive",
                "thread-id": pairingId,
            ] as [String: Any],
            "fd_push": [
                "v": version,
                "pairing_id": pairingId,
                "event_id": eventId,
                "class": eventClass,
                "nonce": nonce,
                "ciphertext": ciphertext,
            ] as [String: Any],
        ]
    }

    /// A complete sealed push for `payload`.
    static func sealedUserInfo(payload: Data = payloadJSON(), eventClass: String = "needs_input") throws
        -> [AnyHashable: Any] {
        let sealed = try desktopSeal(payload)
        return userInfo(eventClass: eventClass, nonce: sealed.nonce, ciphertext: sealed.ciphertext)
    }

    static func hexData(_ hex: String) -> Data {
        var out = Data(capacity: hex.count / 2)
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            out.append(UInt8(hex[index..<next], radix: 16)!)
            index = next
        }
        return out
    }
}

/// `PushKeyMaterialProviding` double: a fixed map, or a thrown error.
struct StubKeyMaterialProvider: PushKeyMaterialProviding {
    var materials: [String: PushKeyMaterial] = [SealedPushFixtures.pairingId: SealedPushFixtures.phoneMaterial]
    var error: Error?

    func keyMaterial(pairingId: String) throws -> PushKeyMaterial? {
        if let error { throw error }
        return materials[pairingId]
    }
}
