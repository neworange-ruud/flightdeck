//
//  PushTokenRegistrationTests.swift
//  FlightDeckRemoteTests
//
//  `TransportClient.registerPushToken` sends the `register_push_token` relay
//  frame for the pairing once the session is authenticated, and re-sends it on
//  a token that arrives while already live (spec §5.5).
//

import Testing
import Foundation
@testable import FlightDeckRemote

@Suite struct PushTokenRegistrationTests {

    private func makeClient(
        keychain: InMemoryKeychainStore,
        channel: ScriptedChannel
    ) throws -> TransportClient {
        let (peer, ka) = try TransportFixtures.makePeer(keychain: keychain)
        let recordStore = PairingRecordStore(store: keychain)
        try recordStore.save(peer.record)
        let identity = try DeviceIdentity.loadOrCreate(store: keychain)
        var config = TransportClient.Config()
        config.pingInterval = .seconds(999)
        return TransportClient(
            identity: identity,
            keyAgreement: ka,
            recordStore: recordStore,
            connector: ScriptedConnector(channel: channel),
            clientInfo: Wire.ClientInfo(appVersion: "test", platform: "ios", osVersion: nil),
            config: config,
            jitter: { 0 },
            now: { 1_752_000_100_000 })
    }

    private func handshake(_ channel: ScriptedChannel, client: TransportClient) async {
        await channel.push(.helloOk(protocolVersion: 1, serverTimeMs: 1, connectionId: "c1"))
        await channel.push(.authChallenge(nonce: TransportFixtures.nonceB64(), serverTimeMs: 1))
        await channel.push(.authOk(pairingIds: [Wire.PairingId("pair_test_1")]))
        _ = await waitUntil { if case .connected = await client.currentLinkState() { return true }; return false }
    }

    private func registeredToken(in frames: [Wire.RelayFrame]) -> (String, Wire.ApnsEnvironment)? {
        for frame in frames {
            if case let .registerPushToken(pairingId, token, environment) = frame {
                #expect(pairingId.rawValue == "pair_test_1")
                return (token, environment)
            }
        }
        return nil
    }

    @Test func tokenKnownBeforeAuthIsSentOnAuthOk() async throws {
        let keychain = InMemoryKeychainStore()
        let channel = ScriptedChannel()
        let client = try makeClient(keychain: keychain, channel: channel)

        // Token arrives before the link is live (the common case: AppDelegate
        // registers early).
        await client.registerPushToken("tok_hex", environment: .sandbox)
        await client.start()
        await handshake(channel, client: client)

        let found = await waitUntil {
            await self.registeredToken(in: channel.sentFrames()) != nil
        }
        #expect(found)
        let token = await registeredToken(in: channel.sentFrames())
        #expect(token?.0 == "tok_hex")
        #expect(token?.1 == .sandbox)
    }

    @Test func tokenArrivingWhileLiveIsSentImmediately() async throws {
        let keychain = InMemoryKeychainStore()
        let channel = ScriptedChannel()
        let client = try makeClient(keychain: keychain, channel: channel)

        await client.start()
        await handshake(channel, client: client)

        // No token yet → none sent during handshake.
        let beforeCount = await channel.sentFrames().filter {
            if case .registerPushToken = $0 { return true }; return false
        }.count
        #expect(beforeCount == 0)

        // Token arrives now → sent right away.
        await client.registerPushToken("tok_live", environment: .production)
        let found = await waitUntil {
            await self.registeredToken(in: channel.sentFrames())?.0 == "tok_live"
        }
        #expect(found)
        let token = await registeredToken(in: channel.sentFrames())
        #expect(token?.1 == .production)
    }
}
