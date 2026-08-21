//
//  KeyAgreementKeysTests.swift
//  FlightDeckRemoteTests
//
//  The software P-256 key-agreement keypair (REMOTE_PROTOCOL §5.2/§7.1):
//  create/load/destroy round-trips, exact public-key byte shape, and a full
//  ECDH → E2EChannel round-trip proving it derives a working channel with a
//  desktop peer.
//

import Testing
import Foundation
import CryptoKit
@testable import FlightDeckRemote

@Suite struct KeyAgreementKeysTests {

    @Test func loadOrCreateIsStableAndDestroyRotates() throws {
        let store = InMemoryKeychainStore()
        let a = try KeyAgreementKeys.loadOrCreate(store: store)
        let b = try KeyAgreementKeys.loadOrCreate(store: store)
        #expect(a.publicKeyBase64 == b.publicKeyBase64)
        #expect(a.privateScalar == b.privateScalar)

        try KeyAgreementKeys.destroy(store: store)
        let c = try KeyAgreementKeys.loadOrCreate(store: store)
        #expect(c.publicKeyBase64 != a.publicKeyBase64)
    }

    @Test func publicKeyHasExactByteShape() throws {
        let store = InMemoryKeychainStore()
        let keys = try KeyAgreementKeys.loadOrCreate(store: store)
        let pub = try #require(Data(base64Encoded: keys.publicKeyBase64))
        #expect(pub.count == 65)
        #expect(pub.first == 0x04) // uncompressed SEC1 marker
        #expect(keys.privateScalar.count == 32)
    }

    @Test func isDistinctFromTheSigningIdentity() throws {
        let store = InMemoryKeychainStore()
        let identity = try DeviceIdentity.loadOrCreate(store: store)
        let ka = try KeyAgreementKeys.loadOrCreate(store: store)
        // Different Keychain accounts → different keys.
        #expect(identity.publicKeyBase64 != ka.publicKeyBase64)
    }

    @Test func derivesAWorkingChannelWithADesktopPeer() throws {
        let store = InMemoryKeychainStore()
        let phone = try KeyAgreementKeys.loadOrCreate(store: store)

        let desktopPriv = P256.KeyAgreement.PrivateKey()
        let salt = Data("bootstrap".utf8)
        let pairingID = "pair_ka_1"

        let phoneChannel = try E2EChannel.derive(
            identityPrivateScalar: phone.privateScalar,
            peerPublicKeyX963: desktopPriv.publicKey.x963Representation,
            pairingID: pairingID, salt: salt, role: .phone)
        let desktopChannel = try E2EChannel.derive(
            identityPrivateScalar: desktopPriv.rawRepresentation,
            peerPublicKeyX963: Data(base64Encoded: phone.publicKeyBase64)!,
            pairingID: pairingID, salt: salt, role: .desktop)

        let plaintext = Data(#"{"type":"reply"}"#.utf8)
        let sealed = try phoneChannel.seal(plaintext, seq: 1, sentAtMs: 1)
        let opened = try desktopChannel.open(
            seq: 1, sender: .phone, sentAtMs: 1,
            nonceB64: sealed.nonceB64, ciphertextB64: sealed.ciphertextB64)
        #expect(opened == plaintext)
    }
}
