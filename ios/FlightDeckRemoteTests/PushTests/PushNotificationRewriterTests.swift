//
//  PushNotificationRewriterTests.swift
//  FlightDeckRemoteTests
//
//  What the Notification Service Extension shows for a sealed push
//  (remote-control-bza0): the rewrite per class, the mute/toggle policy, the
//  deep-link userInfo the app's tap handling consumes, and — above all — that
//  every failure leaves the relay's generic notification exactly as it was.
//

import Foundation
import Testing
import UserNotifications
@testable import FlightDeckRemote

struct PushNotificationRewriterTests {
    typealias F = SealedPushFixtures

    private func rewrite(
        _ userInfo: [AnyHashable: Any],
        keys: PushKeyMaterialProviding = StubKeyMaterialProvider(),
        settings: NotificationSettings = NotificationSettings()
    ) -> PushRewriteResult {
        PushNotificationRewriter.rewrite(
            userInfo: userInfo,
            originalTitle: "FlightDeck",
            originalBody: "An agent needs your input",
            keys: keys,
            settings: settings)
    }

    private func rewritten(_ result: PushRewriteResult) throws -> RewrittenPush {
        guard case let .rewritten(push) = result else {
            Issue.record("expected a rewrite, got \(result)")
            throw CancellationError()
        }
        return push
    }

    private func original(_ userInfo: [AnyHashable: Any]) -> UNMutableNotificationContent {
        let content = UNMutableNotificationContent()
        content.title = "FlightDeck"
        content.body = "An agent needs your input"
        content.sound = .default
        content.interruptionLevel = .timeSensitive
        content.threadIdentifier = F.pairingId
        content.userInfo = userInfo
        return content
    }

    // MARK: - Present

    @Test func needsInputIsRewrittenUrgently() throws {
        let push = try rewritten(rewrite(try F.sealedUserInfo()))
        #expect(push.title == "fd-api needs input")
        #expect(push.subtitle == "fd-api")
        #expect(push.body == "Allow `rm -rf dist/`?")
        #expect(push.sound == .needsInput)
        #expect(push.interruption == .timeSensitive)
        #expect(push.threadIdentifier == "sess_1")
        #expect(push.relevanceScore == 1)
        #expect(!push.suppressed)
        #expect(push.pairingId == F.pairingId)
        #expect(push.eventId == F.eventId)
    }

    @Test func doneAndErrorAreActiveWithTheChime() throws {
        for eventClass in ["done", "error"] {
            let info = try F.sealedUserInfo(payload: F.payloadJSON(eventClass: eventClass, title: "fd-api", body: "x"), eventClass: eventClass)
            let push = try rewritten(rewrite(info))
            #expect(push.sound == .standard)
            #expect(push.interruption == .active)
            #expect(push.relevanceScore == 0.5)
        }
    }

    @Test func chimeOffSilencesDoneButNotNeedsInput() throws {
        var settings = NotificationSettings()
        settings.completionChime = false
        let done = try F.sealedUserInfo(payload: F.payloadJSON(eventClass: "done"), eventClass: "done")
        let doneResult = try rewritten(rewrite(done, settings: settings))
        #expect(doneResult.sound == .none)
        #expect(!doneResult.suppressed)
        #expect(doneResult.interruption == .active)

        let needs = try rewritten(rewrite(try F.sealedUserInfo(), settings: settings))
        #expect(needs.sound == .needsInput)
    }

    @Test func emptyTitleFallsBackToTheGenericTitle() throws {
        let info = try F.sealedUserInfo(payload: F.payloadJSON(title: ""))
        #expect(try rewritten(rewrite(info)).title == "FlightDeck")
    }

    // MARK: - Mute / toggles

    @Test func mutedProjectIsDeliveredAsQuietlyAsPossible() throws {
        var settings = NotificationSettings()
        settings.mutedProjectIds = ["proj_1"]
        let push = try rewritten(rewrite(try F.sealedUserInfo(), settings: settings))
        #expect(push.suppressed)
        #expect(push.sound == .none)
        #expect(push.interruption == .passive)
        #expect(push.relevanceScore == 0)
        // No decrypted content leaks into a muted delivery.
        #expect(push.title == "FlightDeck")
        #expect(push.body == "An agent needs your input")
        #expect(push.subtitle.isEmpty)
        #expect(push.threadIdentifier == PushNotificationRewriter.suppressedThreadIdentifier)
        // A tap still deep-links.
        #expect(push.projectId == "proj_1")
        #expect(push.pairingId == F.pairingId)
    }

    @Test func otherProjectsStayUnmuted() throws {
        var settings = NotificationSettings()
        settings.mutedProjectIds = ["proj_other"]
        #expect(try !rewritten(rewrite(try F.sealedUserInfo(), settings: settings)).suppressed)
    }

    @Test func needsInputToggleOffSuppressesOnlyNeedsInput() throws {
        var settings = NotificationSettings()
        settings.agentNeedsInput = false
        #expect(try rewritten(rewrite(try F.sealedUserInfo(), settings: settings)).suppressed)
        let done = try F.sealedUserInfo(payload: F.payloadJSON(eventClass: "done"), eventClass: "done")
        #expect(try !rewritten(rewrite(done, settings: settings)).suppressed)
    }

    @Test func finishedToggleOffSuppressesDoneAndError() throws {
        var settings = NotificationSettings()
        settings.agentFinished = false
        for eventClass in ["done", "error"] {
            let info = try F.sealedUserInfo(payload: F.payloadJSON(eventClass: eventClass), eventClass: eventClass)
            #expect(try rewritten(rewrite(info, settings: settings)).suppressed)
        }
        #expect(try !rewritten(rewrite(try F.sealedUserInfo(), settings: settings)).suppressed)
    }

    @Test func theDecryptedClassWinsOverThePlaintextHint() throws {
        // The envelope's `class` is unauthenticated; a relay (or attacker)
        // relabelling a needs_input as done must not change the policy.
        var settings = NotificationSettings()
        settings.agentFinished = false
        let info = try F.sealedUserInfo(payload: F.payloadJSON(eventClass: "needs_input"), eventClass: "done")
        let push = try rewritten(rewrite(info, settings: settings))
        #expect(!push.suppressed)
        #expect(push.interruption == .timeSensitive)
    }

    // MARK: - Fallbacks: never drop, never partially rewrite

    @Test func everyFailureFallsBackWithItsReason() throws {
        let good = try F.sealedUserInfo()
        let unknown = StubKeyMaterialProvider(materials: [:])
        let broken = StubKeyMaterialProvider(error: KeychainError.unexpectedStatus(-25308))
        let corrupt = StubKeyMaterialProvider(error: PairingRecordError.corruptStoredRecord)
        let wrongKey = StubKeyMaterialProvider(materials: [
            F.pairingId: PushKeyMaterial(privateScalar: F.desktopScalar, peerPublicKeyX963: F.desktopPublicX963, salt: F.salt),
        ])
        var tampered = good
        var push = try #require(tampered["fd_push"] as? [String: Any])
        push["ciphertext"] = Data(repeating: 1, count: 64).base64EncodedString()
        tampered["fd_push"] = push

        #expect(rewrite(["aps": ["alert": "x"]]) == .fallback(.missingEnvelope))
        #expect(rewrite(good, keys: unknown) == .fallback(.unknownPairing))
        #expect(rewrite(good, keys: broken) == .fallback(.keychainUnavailable))
        #expect(rewrite(good, keys: corrupt) == .fallback(.invalidKeyMaterial))
        #expect(rewrite(good, keys: wrongKey) == .fallback(.openFailed))
        #expect(rewrite(tampered) == .fallback(.openFailed))
        let badJSON = try F.desktopSeal(Data("{".utf8))
        #expect(rewrite(F.userInfo(nonce: badJSON.nonce, ciphertext: badJSON.ciphertext)) == .fallback(.malformedPayload))
        let oversized = F.userInfo(nonce: "AAAA", ciphertext: String(repeating: "A", count: 5000))
        #expect(rewrite(oversized) == .fallback(.ciphertextTooLarge))
    }

    @Test func fallbackReturnsTheOriginalContentObjectUntouched() throws {
        let content = original(try F.sealedUserInfo())
        let (result, outcome) = PushNotificationRewriter.content(
            for: content, keys: StubKeyMaterialProvider(materials: [:]), settings: NotificationSettings())
        #expect(outcome == .fallback(.unknownPairing))
        #expect(result === content)
        #expect(result.title == "FlightDeck")
        #expect(result.body == "An agent needs your input")
        #expect(result.sound == .default)
        #expect(result.interruptionLevel == .timeSensitive)
        #expect(result.userInfo["fd_push"] != nil)
    }

    // MARK: - Applying to UNNotificationContent

    @Test func rewrittenContentCarriesTheAppsTapPayload() throws {
        let content = original(try F.sealedUserInfo())
        let (result, _) = PushNotificationRewriter.content(
            for: content, keys: StubKeyMaterialProvider(), settings: NotificationSettings())
        #expect(result !== content)
        #expect(result.title == "fd-api needs input")
        #expect(result.subtitle == "fd-api")
        #expect(result.body == "Allow `rm -rf dist/`?")
        #expect(result.interruptionLevel == .timeSensitive)
        #expect(result.threadIdentifier == "sess_1")
        #expect(result.sound != nil)
        #expect(result.relevanceScore == 1)

        // The sealed envelope is dropped, the relay's `aps` kept.
        #expect(result.userInfo["fd_push"] == nil)
        #expect(result.userInfo["aps"] != nil)

        // The exact shape the app's tap handler parses (`PushPayload` →
        // `PushRouting`), including the originating pairing.
        let payload = try #require(PushPayload(userInfo: result.userInfo))
        #expect(payload.eventId == F.eventId)
        #expect(payload.pairingId == F.pairingId)
        #expect(payload.deepLink == Wire.DeepLink(
            projectId: Wire.ProjectId("proj_1"),
            sessionId: Wire.SessionId("sess_1"),
            itemId: Wire.ItemId("item_9")))
        #expect(payload.appDeepLink == DeepLink(projectId: "proj_1", sessionId: "sess_1", pairingId: F.pairingId))

        // And it matches, key for key, what a local notification for the same
        // event would carry.
        let local = PushPayload.userInfo(eventId: F.eventId, deepLink: payload.deepLink, pairingId: F.pairingId)
        #expect(PushPayload(userInfo: local) == payload)
    }

    @Test func suppressedContentHasNoSoundAndIsPassive() throws {
        var settings = NotificationSettings()
        settings.mutedProjectIds = ["proj_1"]
        let (result, _) = PushNotificationRewriter.content(
            for: original(try F.sealedUserInfo()), keys: StubKeyMaterialProvider(), settings: settings)
        #expect(result.sound == nil)
        #expect(result.interruptionLevel == .passive)
        #expect(result.relevanceScore == 0)
        #expect(result.title == "FlightDeck")
        #expect(result.body == "An agent needs your input")
        #expect(PushPayload(userInfo: result.userInfo) != nil)
    }

    @Test func nullItemIdRoundTripsAsNSNull() throws {
        let info = try F.sealedUserInfo(payload: F.payloadJSON(itemId: nil))
        let (result, _) = PushNotificationRewriter.content(
            for: original(info), keys: StubKeyMaterialProvider(), settings: NotificationSettings())
        let link = try #require(result.userInfo["deep_link"] as? [String: Any])
        #expect(link["item_id"] is NSNull)
        #expect(try #require(PushPayload(userInfo: result.userInfo)).deepLink.itemId == nil)
    }

    // MARK: - Keychain-backed key material

    @Test func keychainProviderReadsTheRecordAndTheKAKey() throws {
        let store = InMemoryKeychainStore()
        let record = PairingRecord(
            pairingId: F.pairingId,
            peerDeviceId: "dev_mac",
            peerKeyAgreementPublicKeyB64: F.desktopPublicX963.base64EncodedString(),
            saltB64: F.salt.base64EncodedString(),
            relayURL: "wss://relay.example/ws")
        try PairingRecordStore(store: store).save(record)
        try store.set(F.phoneScalar, account: SharedContainer.keyAgreementAccount)

        let provider = KeychainPushKeyMaterialProvider(store: store)
        #expect(try provider.keyMaterial(pairingId: F.pairingId) == F.phoneMaterial)
        #expect(try provider.keyMaterial(pairingId: "pair_unknown") == nil)

        // End to end through the real provider.
        let push = try rewritten(rewrite(try F.sealedUserInfo(), keys: provider))
        #expect(push.title == "fd-api needs input")
    }

    @Test func keychainProviderNeverWritesAndNeverMigrates() throws {
        // A legacy single-record item must not be migrated by the extension
        // (that is the app's job); the read is a plain miss.
        let store = RecordingKeychainStore()
        let record = PairingRecord(
            pairingId: F.pairingId, peerDeviceId: nil,
            peerKeyAgreementPublicKeyB64: F.desktopPublicX963.base64EncodedString(),
            saltB64: F.salt.base64EncodedString(), relayURL: "wss://relay.example/ws")
        store.items[PairingRecordStore.legacyAccount] = try JSONEncoder().encode(record)
        store.items[SharedContainer.keyAgreementAccount] = F.phoneScalar

        #expect(try KeychainPushKeyMaterialProvider(store: store).keyMaterial(pairingId: F.pairingId) == nil)
        #expect(store.writes == 0)
    }

    @Test func keychainProviderThrowsWhenTheKAKeyIsMissing() throws {
        let store = InMemoryKeychainStore()
        try PairingRecordStore(store: store).save(PairingRecord(
            pairingId: F.pairingId, peerDeviceId: nil,
            peerKeyAgreementPublicKeyB64: F.desktopPublicX963.base64EncodedString(),
            saltB64: F.salt.base64EncodedString(), relayURL: "wss://relay.example/ws"))
        #expect(throws: SealedPushError.invalidKeyMaterial) {
            _ = try KeychainPushKeyMaterialProvider(store: store).keyMaterial(pairingId: F.pairingId)
        }
        #expect(rewrite(try F.sealedUserInfo(), keys: KeychainPushKeyMaterialProvider(store: store)) == .fallback(.invalidKeyMaterial))
    }
}

/// Counts writes, to prove the extension's read path is read-only.
final class RecordingKeychainStore: KeychainStoring {
    var items: [String: Data] = [:]
    private(set) var writes = 0
    func get(account: String) throws -> Data? { items[account] }
    func set(_ data: Data, account: String) throws { writes += 1; items[account] = data }
    func delete(account: String) throws { writes += 1; items[account] = nil }
    func accounts() throws -> [String] { Array(items.keys) }
}
