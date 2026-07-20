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

    // MARK: - PromptKind / multi-option prompts (remote-control-zag, protocol v2)

    @Test func transcriptDecodesBinaryPermissionAndQuestionPrompts() throws {
        let data = try fixture("desktop_to_phone", "transcript")
        let message = try JSONDecoder().decode(Wire.DesktopToPhone.self, from: data)

        guard case let .transcript(feed) = message else {
            Issue.record("expected .transcript, got \(message)")
            return
        }
        #expect(feed.items.count == 6)

        guard case let .permissionPrompt(_, promptId1, kind1, command1, options1, allowFreeText1, _) =
            feed.items[4] else {
            Issue.record("expected item[4] to be a permission_prompt"); return
        }
        #expect(promptId1 == Wire.PromptId("prompt_ab12"))
        #expect(kind1 == .permission)
        #expect(command1 == "rm -rf dist/")
        #expect(allowFreeText1 == false)
        #expect(options1.count == 2)
        #expect(options1[0] == Wire.PermissionOption(index: 0, choice: .allowOnce, label: "Allow once"))
        #expect(options1[1] == Wire.PermissionOption(index: 1, choice: .deny, label: "Deny"))

        guard case let .permissionPrompt(_, promptId2, kind2, command2, options2, allowFreeText2, _) =
            feed.items[5] else {
            Issue.record("expected item[5] to be a permission_prompt"); return
        }
        #expect(promptId2 == Wire.PromptId("prompt_cd34"))
        #expect(kind2 == .question)
        #expect(command2 == "Which database should the login service use?")
        #expect(allowFreeText2 == true)
        #expect(options2.count == 3)
        #expect(options2[0] == Wire.PermissionOption(
            index: 0, choice: nil, label: "Postgres",
            description: "Use the existing shared Postgres cluster."))
        #expect(options2[1] == Wire.PermissionOption(
            index: 1, choice: nil, label: "SQLite",
            description: "Embed a local SQLite file for now."))
        #expect(options2[2] == Wire.PermissionOption(
            index: 2, choice: nil, label: "Redis",
            description: "Store sessions in Redis with a short TTL."))
        // Question options carry no `choice` — must round-trip to `nil`, never
        // a synthesized binary value.
        #expect(options2.allSatisfy { $0.choice == nil })
    }

    /// A prompt kind absent from the JSON (`#[serde(default)]` on the Rust
    /// side) decodes as `.permission` — pre-v2 desktop compatibility.
    @Test func permissionPromptWithoutKindDefaultsToPermission() throws {
        let json = Data("""
        {
          "type": "permission_prompt",
          "item_id": "item_x",
          "prompt_id": "prompt_x",
          "command": "rm -rf dist/",
          "options": [
            {"choice": "allow_once", "label": "Allow once"},
            {"choice": "deny", "label": "Deny"}
          ],
          "at_ms": 1
        }
        """.utf8)
        let item = try JSONDecoder().decode(Wire.TranscriptItem.self, from: json)
        guard case let .permissionPrompt(_, _, kind, _, options, allowFreeText, _) = item else {
            Issue.record("expected .permissionPrompt"); return
        }
        #expect(kind == .permission)
        #expect(allowFreeText == false)
        // Options with no `index` key default to 0 (Rust `#[serde(default)]`).
        #expect(options[0].index == 0)
        #expect(options[1].index == 0)
    }

    /// Encoding an `option_index` decision omits `choice`/`free_text`
    /// entirely (matches Rust `skip_serializing_if`), and decoding it back
    /// round-trips.
    @Test func optionIndexDecisionEncodesWithoutChoiceOrFreeText() throws {
        let command = Wire.PhoneCommand(
            commandId: Wire.CommandId("cmd_oi"),
            issuedAtMs: 1_752_412_811_000,
            body: .permissionDecision(
                sessionId: Wire.SessionId("sess_fix_login"),
                promptId: Wire.PromptId("prompt_q1"),
                choice: nil, optionIndex: 2, freeText: nil))
        let encoded = try JSONEncoder().encode(command)
        let object = try #require(
            try JSONSerialization.jsonObject(with: encoded) as? NSDictionary)

        #expect(object["type"] as? String == "permission_decision")
        #expect(object["option_index"] as? Int == 2)
        #expect(object["choice"] == nil, "binary choice key must be omitted, not null")
        #expect(object["free_text"] == nil, "free_text key must be omitted, not null")

        let decoded = try JSONDecoder().decode(Wire.PhoneCommand.self, from: encoded)
        #expect(decoded == command)
    }

    /// Encoding a `free_text` decision omits `choice`/`option_index`.
    @Test func freeTextDecisionEncodesWithoutChoiceOrOptionIndex() throws {
        let command = Wire.PhoneCommand(
            commandId: Wire.CommandId("cmd_ft"),
            issuedAtMs: 1_752_412_811_000,
            body: .permissionDecision(
                sessionId: Wire.SessionId("sess_fix_login"),
                promptId: Wire.PromptId("prompt_q1"),
                choice: nil, optionIndex: nil, freeText: "Use CockroachDB instead."))
        let encoded = try JSONEncoder().encode(command)
        let object = try #require(
            try JSONSerialization.jsonObject(with: encoded) as? NSDictionary)

        #expect(object["type"] as? String == "permission_decision")
        #expect(object["free_text"] as? String == "Use CockroachDB instead.")
        #expect(object["choice"] == nil)
        #expect(object["option_index"] == nil)

        let decoded = try JSONDecoder().decode(Wire.PhoneCommand.self, from: encoded)
        #expect(decoded == command)
    }

    /// Regression: the binary allow/deny decision still encodes ONLY
    /// `choice` (no `option_index`/`free_text` keys at all), byte-stable
    /// with the pre-v2 wire shape.
    @Test func binaryChoiceDecisionEncodesChoiceOnly() throws {
        let data = try fixture("phone_to_desktop", "permission_decision")
        let decoded = try JSONDecoder().decode(Wire.PhoneCommand.self, from: data)
        #expect(decoded.body == .permissionDecision(
            sessionId: Wire.SessionId("sess_fix_login"),
            promptId: Wire.PromptId("prompt_ab12"),
            choice: .deny, optionIndex: nil, freeText: nil))

        let encoded = try JSONEncoder().encode(decoded)
        let object = try #require(
            try JSONSerialization.jsonObject(with: encoded) as? NSDictionary)
        #expect(object["choice"] as? String == "deny")
        #expect(object["option_index"] == nil)
        #expect(object["free_text"] == nil)
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
