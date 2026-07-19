//
//  PushMuteTests.swift
//  FlightDeckRemoteTests
//
//  Per-machine push mute at the `TransportClient` layer (remote-control-b8d.10,
//  spec §5.5): a muted pairing never registers its APNs token; muting a live
//  pairing actively deregisters it (`unregister_push_token`); unmuting
//  re-registers the held token; and a repeat register of the same token while
//  live never double-registers.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@Suite struct PushMuteTests {

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

    private func registerCount(_ frames: [Wire.RelayFrame]) -> Int {
        frames.filter { if case .registerPushToken = $0 { return true }; return false }.count
    }

    private func unregisterCount(_ frames: [Wire.RelayFrame]) -> Int {
        frames.filter { if case .unregisterPushToken = $0 { return true }; return false }.count
    }

    // MARK: - Muted → never registers

    @Test func mutedPairingDoesNotRegisterOnAuthOk() async throws {
        let keychain = InMemoryKeychainStore()
        let channel = ScriptedChannel()
        let client = try makeClient(keychain: keychain, channel: channel)

        // A token is known, but the pairing is muted BEFORE the link goes live.
        await client.registerPushToken("tok_hex", environment: .sandbox)
        await client.setPushMuted(true)
        await client.start()
        await handshake(channel, client: client)

        // Give the auth_ok path time to (not) send anything push-related.
        _ = await waitUntil { await self.registerCount(channel.sentFrames()) > 0 }
        #expect(await registerCount(channel.sentFrames()) == 0)
        #expect(await unregisterCount(channel.sentFrames()) == 0)
    }

    // MARK: - Muted while live → deregisters, unmute re-registers

    @Test func mutingWhileLiveDeregistersAndUnmutingReRegisters() async throws {
        let keychain = InMemoryKeychainStore()
        let channel = ScriptedChannel()
        let client = try makeClient(keychain: keychain, channel: channel)

        await client.registerPushToken("tok_hex", environment: .sandbox)
        await client.start()
        await handshake(channel, client: client)

        // Registered once on auth_ok.
        _ = await waitUntil { await self.registerCount(channel.sentFrames()) == 1 }
        #expect(await registerCount(channel.sentFrames()) == 1)

        // Mute while live → an unregister_push_token goes out.
        await client.setPushMuted(true)
        let deregistered = await waitUntil { await self.unregisterCount(channel.sentFrames()) == 1 }
        #expect(deregistered)

        // Unmute → the held token is re-registered (register count climbs to 2).
        await client.setPushMuted(false)
        let reRegistered = await waitUntil { await self.registerCount(channel.sentFrames()) == 2 }
        #expect(reRegistered)
        #expect(await unregisterCount(channel.sentFrames()) == 1)
    }

    // MARK: - No double-register on a repeated same-token call while live

    @Test func repeatSameTokenWhileLiveDoesNotDoubleRegister() async throws {
        let keychain = InMemoryKeychainStore()
        let channel = ScriptedChannel()
        let client = try makeClient(keychain: keychain, channel: channel)

        await client.registerPushToken("tok_hex", environment: .sandbox)
        await client.start()
        await handshake(channel, client: client)
        _ = await waitUntil { await self.registerCount(channel.sentFrames()) == 1 }

        // Same token handed over again while live → suppressed (still 1).
        await client.registerPushToken("tok_hex", environment: .sandbox)
        // Let any spurious send land before asserting it didn't.
        _ = await waitUntil { await self.registerCount(channel.sentFrames()) > 1 }
        #expect(await registerCount(channel.sentFrames()) == 1)

        // A DIFFERENT token, however, does register (count climbs to 2).
        await client.registerPushToken("tok_new", environment: .sandbox)
        let changed = await waitUntil { await self.registerCount(channel.sentFrames()) == 2 }
        #expect(changed)
    }

    // MARK: - Muting with nothing registered is a no-op

    @Test func mutingWhenNothingRegisteredSendsNoUnregister() async throws {
        let keychain = InMemoryKeychainStore()
        let channel = ScriptedChannel()
        let client = try makeClient(keychain: keychain, channel: channel)

        // No token ever registered, then mute while live.
        await client.start()
        await handshake(channel, client: client)
        await client.setPushMuted(true)

        _ = await waitUntil { await self.unregisterCount(channel.sentFrames()) > 0 }
        #expect(await unregisterCount(channel.sentFrames()) == 0)
    }
}
