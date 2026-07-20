//
//  ProtocolFixtureConformanceTests.swift
//  FlightDeckRemoteTests
//
//  The acceptance proof for the Swift wire-protocol mirror: every golden
//  fixture in remote/protocol/tests/fixtures/ (embedded via
//  FixturesGenerated.swift — regenerate with ios/scripts/sync-fixtures.sh)
//  is decoded to its top-level Swift type, re-encoded, and compared
//  **semantically** against the original: both sides are parsed with
//  JSONSerialization and compared as NSDictionary, so key order is
//  irrelevant but explicit-null presence is REQUIRED (a dropped
//  `"field": null` key fails the equality).
//
//  Category -> type mapping (spec §8/§13):
//    relay            -> Wire.RelayFrame
//    desktop_to_phone -> Wire.DesktopToPhone
//    phone_to_desktop -> Wire.PhoneCommand
//  (e2e_crypto/vectors.json is a crypto vector file, not a message fixture,
//  and is excluded by the sync script.)
//

import Testing
import Foundation
@testable import FlightDeckRemote

extension ProtocolFixture: CustomTestStringConvertible {
    var testDescription: String { "\(category)/\(name)" }
}

struct ProtocolFixtureConformanceTests {

    // MARK: - Inventory

    @Test func fixtureInventoryIsComplete() {
        #expect(ProtocolFixtures.all.count == ProtocolFixtures.expectedCount)
        // 23 relay + 10 desktop_to_phone + 17 phone_to_desktop golden files
        // (relay gained machine_name, unregister_push_token, revoke,
        // pairing_revoked as their owning iOS issues landed).
        #expect(ProtocolFixtures.all.count >= 48)
        for category in ["relay", "desktop_to_phone", "phone_to_desktop"] {
            #expect(ProtocolFixtures.all.contains { $0.category == category },
                    "no fixtures embedded for category \(category)")
        }
        // The crypto vector file must never be embedded as a message fixture.
        #expect(!ProtocolFixtures.all.contains { $0.category == "e2e_crypto" })
    }

    // MARK: - Decode -> re-encode -> semantic compare, for EVERY fixture

    @Test(arguments: ProtocolFixtures.all)
    func fixtureRoundTripsSemantically(fixture: ProtocolFixture) throws {
        let original = fixture.data
        let reencoded: Data

        switch fixture.category {
        case "relay":
            let frame = try JSONDecoder().decode(Wire.RelayFrame.self, from: original)
            reencoded = try JSONEncoder().encode(frame)
        case "desktop_to_phone":
            let message = try JSONDecoder().decode(Wire.DesktopToPhone.self, from: original)
            reencoded = try JSONEncoder().encode(message)
        case "phone_to_desktop":
            let command = try JSONDecoder().decode(Wire.PhoneCommand.self, from: original)
            reencoded = try JSONEncoder().encode(command)
        default:
            Issue.record("unknown fixture category: \(fixture.category)")
            return
        }

        let expected = try #require(
            try JSONSerialization.jsonObject(with: original) as? NSDictionary,
            "fixture \(fixture.testDescription) is not a JSON object")
        let actual = try #require(
            try JSONSerialization.jsonObject(with: reencoded) as? NSDictionary,
            "re-encoded \(fixture.testDescription) is not a JSON object")

        // NSDictionary equality: key order irrelevant; a key that holds
        // `null` in the fixture must be present (as NSNull) after re-encode.
        #expect(actual == expected,
                "semantic mismatch for \(fixture.testDescription)")
    }

    // MARK: - Spot checks on decoded shapes

    private func fixture(_ category: String, _ name: String) throws -> Data {
        try #require(ProtocolFixtures.all.first {
            $0.category == category && $0.name == name
        }).data
    }

    @Test func snapshotDecodesProjectsAndSessions() throws {
        let data = try fixture("desktop_to_phone", "snapshot")
        let message = try JSONDecoder().decode(Wire.DesktopToPhone.self, from: data)

        guard case .snapshot(let snapshot) = message else {
            Issue.record("expected .snapshot, got \(message)")
            return
        }
        #expect(snapshot.serverTimeMs == 1_752_412_800_000)
        let project = try #require(snapshot.projects.first)
        #expect(project.projectId == Wire.ProjectId("proj_flightdeck"))
        #expect(project.rollup.dot == .needsInput)
        #expect(project.rollup.summary == "1 needs input · 1 working · 3 agents")
        #expect(project.sessions.count == 2)

        let first = project.sessions[0]
        #expect(first.status == .needsInput)
        #expect(first.agentType == .claudeCode)
        #expect(first.pendingQuestion == "Allow rm -rf dist/ ?")
        #expect(first.git.branch == "flightdeck/fix-login")
        #expect(first.git.drift == 2)
        #expect(!first.git.isClean)

        let second = project.sessions[1]
        #expect(second.status == .working)
        #expect(second.pendingQuestion == nil)
        #expect(second.git.hasUpstream == false)
    }

    @Test func statusUpdateDecodesManualStateTag() throws {
        let data = try fixture("desktop_to_phone", "status_update")
        let message = try JSONDecoder().decode(Wire.DesktopToPhone.self, from: data)

        guard case .statusUpdate(let update) = message else {
            Issue.record("expected .statusUpdate, got \(message)")
            return
        }
        #expect(update.updates.count == 2)
        #expect(update.updates[0].status == .idle)
        #expect(update.updates[0].runningTimeSecs == 540)
        #expect(update.updates[1].status == .manual(label: "reviewing by hand"))
        #expect(update.updates[1].runningTimeSecs == nil)
    }

    @Test func phoneCommandFlattensBodyNextToCommandId() throws {
        let data = try fixture("phone_to_desktop", "reply")
        let command = try JSONDecoder().decode(Wire.PhoneCommand.self, from: data)

        #expect(command.commandId == Wire.CommandId("cmd_00000001"))
        #expect(command.issuedAtMs == 1_752_412_810_000)
        #expect(command.body == .reply(
            sessionId: Wire.SessionId("sess_fix_login"),
            text: "Yes, run it. Then rebuild."))
    }

    @Test func machineNameFrameDecodesPairingIdAndName() throws {
        let data = try fixture("relay", "machine_name")
        let frame = try JSONDecoder().decode(Wire.RelayFrame.self, from: data)

        guard case let .machineName(pairingId, machineName) = frame else {
            Issue.record("expected .machineName, got \(frame)")
            return
        }
        #expect(pairingId == Wire.PairingId("pair_ruud_mbp"))
        #expect(machineName == "Ruud's MacBook Pro")
    }

    // MARK: - Machine-name sanitization (§5.7, remote-control-b8d.9)

    @Test func sanitizeMachineNameTrimsSurroundingWhitespace() {
        #expect(Wire.sanitizeMachineName("  Ruud's Mac  ") == "Ruud's Mac")
    }

    @Test func sanitizeMachineNameTreatsEmptyOrWhitespaceAsNoName() {
        #expect(Wire.sanitizeMachineName("") == nil)
        #expect(Wire.sanitizeMachineName("   ") == nil)
        #expect(Wire.sanitizeMachineName("\n\t ") == nil)
    }

    @Test func sanitizeMachineNameBoundsToSixtyFourCharacters() throws {
        let long = String(repeating: "a", count: 200)
        let bounded = try #require(Wire.sanitizeMachineName(long))
        #expect(bounded.count == 64)
        #expect(bounded == String(repeating: "a", count: 64))
    }

    @Test func sanitizeMachineNameLeavesAShortNameUntouched() {
        #expect(Wire.sanitizeMachineName("Work Mac") == "Work Mac")
    }

    // MARK: - Unpair / revoke (§5.8, remote-control-b8d.11)

    @Test func revokeFrameDecodesPairingId() throws {
        let data = try fixture("relay", "revoke")
        let frame = try JSONDecoder().decode(Wire.RelayFrame.self, from: data)

        guard case let .revoke(pairingId) = frame else {
            Issue.record("expected .revoke, got \(frame)")
            return
        }
        #expect(pairingId == Wire.PairingId("pair_ruud_mbp"))
    }

    @Test func revokeFrameEncodesToSnakeCaseType() throws {
        let frame = Wire.RelayFrame.revoke(pairingId: Wire.PairingId("pair_x"))
        let object = try #require(
            try JSONSerialization.jsonObject(
                with: JSONEncoder().encode(frame)) as? NSDictionary)
        #expect(object["type"] as? String == "revoke")
        #expect(object["pairing_id"] as? String == "pair_x")
    }

    @Test func pairingRevokedFrameDecodesPairingId() throws {
        let data = try fixture("relay", "pairing_revoked")
        let frame = try JSONDecoder().decode(Wire.RelayFrame.self, from: data)

        guard case let .pairingRevoked(pairingId) = frame else {
            Issue.record("expected .pairingRevoked, got \(frame)")
            return
        }
        #expect(pairingId == Wire.PairingId("pair_ruud_mbp"))
    }

    @Test func envelopeFrameFlattensEncryptedEnvelope() throws {
        let data = try fixture("relay", "envelope")
        let frame = try JSONDecoder().decode(Wire.RelayFrame.self, from: data)

        guard case .envelope(let envelope) = frame else {
            Issue.record("expected .envelope, got \(frame)")
            return
        }
        #expect(envelope.pairingId == Wire.PairingId("pair_ruud_mbp"))
        #expect(envelope.seq == 42)
        #expect(envelope.sender == .desktop)
        #expect(envelope.sentAtMs == 1_752_412_802_000)
    }

    // MARK: - Explicit-null emission (spec §3.5)

    @Test func nilOptionalsAreEncodedAsExplicitNull() throws {
        let command = Wire.PhoneCommand(
            commandId: Wire.CommandId("cmd_test"),
            issuedAtMs: 1,
            body: .requestTranscript(
                sessionId: Wire.SessionId("sess_x"), fromIndex: nil))
        let encoded = try JSONEncoder().encode(command)
        let object = try #require(
            try JSONSerialization.jsonObject(with: encoded) as? NSDictionary)

        // The key must be PRESENT and hold JSON null — not be omitted.
        #expect(object["from_index"] as? NSNull == NSNull())

        let frame = Wire.RelayFrame.bye(reason: nil)
        let frameObject = try #require(
            try JSONSerialization.jsonObject(
                with: JSONEncoder().encode(frame)) as? NSDictionary)
        #expect(frameObject["reason"] as? NSNull == NSNull())
    }

    @Test func unknownTagFailsToDecode() {
        let bogus = Data(#"{"type":"warp_drive","factor":9}"#.utf8)
        #expect(throws: DecodingError.self) {
            _ = try JSONDecoder().decode(Wire.RelayFrame.self, from: bogus)
        }
        #expect(throws: DecodingError.self) {
            _ = try JSONDecoder().decode(Wire.DesktopToPhone.self, from: bogus)
        }
    }
}
