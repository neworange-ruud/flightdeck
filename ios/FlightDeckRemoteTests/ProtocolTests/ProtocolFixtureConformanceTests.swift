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

        guard case let .permissionPrompt(_, promptId1, kind1, command1, options1, allowFreeText1, multiSelect1, _, _) =
            feed.items[4] else {
            Issue.record("expected item[4] to be a permission_prompt"); return
        }
        #expect(promptId1 == Wire.PromptId("prompt_ab12"))
        #expect(kind1 == .permission)
        #expect(command1 == "rm -rf dist/")
        #expect(allowFreeText1 == false)
        #expect(multiSelect1 == false)
        #expect(options1.count == 2)
        #expect(options1[0] == Wire.PermissionOption(index: 0, choice: .allowOnce, label: "Allow once"))
        #expect(options1[1] == Wire.PermissionOption(index: 1, choice: .deny, label: "Deny"))

        guard case let .permissionPrompt(_, promptId2, kind2, command2, options2, allowFreeText2, multiSelect2, _, _) =
            feed.items[5] else {
            Issue.record("expected item[5] to be a permission_prompt"); return
        }
        #expect(promptId2 == Wire.PromptId("prompt_cd34"))
        #expect(kind2 == .question)
        #expect(command2 == "Which database should the login service use?")
        #expect(allowFreeText2 == true)
        #expect(multiSelect2 == false)
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
        guard case let .permissionPrompt(_, _, kind, _, options, allowFreeText, multiSelect, _, _) = item else {
            Issue.record("expected .permissionPrompt"); return
        }
        #expect(kind == .permission)
        #expect(allowFreeText == false)
        // Absent `multi_select` (a pre-v3 desktop) defaults to single-select.
        #expect(multiSelect == false)
        // Options with no `index` key default to 0 (Rust `#[serde(default)]`).
        #expect(options[0].index == 0)
        #expect(options[1].index == 0)
    }

    // MARK: - Multi-question prompts (remote-control-kym, protocol v4)

    /// A prompt carrying several `questions` decodes them all, each with its own
    /// options and `multi_select` flag, in tab order.
    @Test func permissionPromptDecodesMultipleQuestions() throws {
        let json = Data("""
        {
          "type": "permission_prompt",
          "item_id": "item_mq",
          "prompt_id": "prompt_mq",
          "kind": "question",
          "command": "Which database should the login service use?",
          "options": [
            {"index": 0, "label": "Postgres"},
            {"index": 1, "label": "SQLite"}
          ],
          "allow_free_text": false,
          "multi_select": false,
          "questions": [
            {
              "header": "Database",
              "question": "Which database should the login service use?",
              "options": [
                {"index": 0, "label": "Postgres", "description": "Shared cluster."},
                {"index": 1, "label": "SQLite"}
              ],
              "multi_select": false
            },
            {
              "header": "Checks",
              "question": "Which checks should run before merge?",
              "options": [
                {"index": 0, "label": "Tests"},
                {"index": 1, "label": "Fmt"},
                {"index": 2, "label": "Clippy"}
              ],
              "multi_select": true
            }
          ],
          "at_ms": 42
        }
        """.utf8)
        let item = try JSONDecoder().decode(Wire.TranscriptItem.self, from: json)
        guard case let .permissionPrompt(_, _, _, _, _, _, _, questions, _) = item else {
            Issue.record("expected .permissionPrompt"); return
        }
        #expect(questions.count == 2)

        #expect(questions[0].header == "Database")
        #expect(questions[0].question == "Which database should the login service use?")
        #expect(questions[0].multiSelect == false)
        #expect(questions[0].options.count == 2)
        #expect(questions[0].options[0] == Wire.PermissionOption(
            index: 0, choice: nil, label: "Postgres", description: "Shared cluster."))
        #expect(questions[0].options[1].label == "SQLite")

        #expect(questions[1].header == "Checks")
        #expect(questions[1].question == "Which checks should run before merge?")
        #expect(questions[1].multiSelect == true)
        #expect(questions[1].options.map(\.label) == ["Tests", "Fmt", "Clippy"])
    }

    /// A prompt WITHOUT the `questions` key (a pre-v4 desktop) decodes to an
    /// empty `questions` list, so the phone falls back to the single flat
    /// question (`command`/`options`/`multi_select`).
    @Test func permissionPromptWithoutQuestionsFallsBackToFlatFields() throws {
        let json = Data("""
        {
          "type": "permission_prompt",
          "item_id": "item_fb",
          "prompt_id": "prompt_fb",
          "kind": "question",
          "command": "Which database should the login service use?",
          "options": [
            {"index": 0, "label": "Postgres"},
            {"index": 1, "label": "SQLite"}
          ],
          "allow_free_text": true,
          "multi_select": false,
          "at_ms": 7
        }
        """.utf8)
        let item = try JSONDecoder().decode(Wire.TranscriptItem.self, from: json)
        guard case let .permissionPrompt(_, promptId, kind, command, options, allowFreeText, multiSelect, questions, _) = item else {
            Issue.record("expected .permissionPrompt"); return
        }
        // Absent `questions` → empty list; the flat single-question fields carry
        // the one question exactly as before (no regression).
        #expect(questions.isEmpty)
        #expect(promptId == Wire.PromptId("prompt_fb"))
        #expect(kind == .question)
        #expect(command == "Which database should the login service use?")
        #expect(allowFreeText == true)
        #expect(multiSelect == false)
        #expect(options.map(\.label) == ["Postgres", "SQLite"])
    }

    /// A `questions` PromptQuestion round-trips: `multi_select` is always
    /// emitted; an absent `header` is omitted (never null); decoding the
    /// re-encoded value equals the original.
    @Test func promptQuestionRoundTripsOmittingAbsentHeader() throws {
        let q = Wire.PromptQuestion(
            header: nil,
            question: "Pick one",
            options: [Wire.PermissionOption(index: 0, label: "A")],
            multiSelect: false)
        let encoded = try JSONEncoder().encode(q)
        let object = try #require(
            try JSONSerialization.jsonObject(with: encoded) as? NSDictionary)
        #expect(object["header"] == nil, "absent header must be omitted, not null")
        #expect(object["multi_select"] as? Bool == false)
        let decoded = try JSONDecoder().decode(Wire.PromptQuestion.self, from: encoded)
        #expect(decoded == q)
    }

    /// A multi-question `answers` decision encodes an `answers` array (one entry
    /// per question, in order) and omits every single-question field
    /// (`choice`/`option_index`/`option_indices`/`free_text`); it round-trips.
    @Test func answersDecisionEncodesAnswersOmittingSingleQuestionFields() throws {
        let command = Wire.PhoneCommand(
            commandId: Wire.CommandId("cmd_ans"),
            issuedAtMs: 1_752_412_811_000,
            body: .permissionDecision(
                sessionId: Wire.SessionId("sess_fix_login"),
                promptId: Wire.PromptId("prompt_mq"),
                choice: nil, optionIndex: nil, optionIndices: nil, freeText: nil,
                answers: [
                    Wire.QuestionAnswer(optionIndices: [0]),
                    Wire.QuestionAnswer(optionIndices: [1, 2]),
                ]))
        let encoded = try JSONEncoder().encode(command)
        let object = try #require(
            try JSONSerialization.jsonObject(with: encoded) as? NSDictionary)

        #expect(object["type"] as? String == "permission_decision")
        let answers = try #require(object["answers"] as? [[String: Any]])
        #expect(answers.count == 2)
        #expect(answers[0]["option_indices"] as? [Int] == [0])
        #expect(answers[1]["option_indices"] as? [Int] == [1, 2])
        #expect(object["choice"] == nil)
        #expect(object["option_index"] == nil)
        #expect(object["option_indices"] == nil)
        #expect(object["free_text"] == nil)

        let decoded = try JSONDecoder().decode(Wire.PhoneCommand.self, from: encoded)
        #expect(decoded == command)
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
                choice: nil, optionIndex: 2, optionIndices: nil, freeText: nil, answers: nil))
        let encoded = try JSONEncoder().encode(command)
        let object = try #require(
            try JSONSerialization.jsonObject(with: encoded) as? NSDictionary)

        #expect(object["type"] as? String == "permission_decision")
        #expect(object["option_index"] as? Int == 2)
        #expect(object["choice"] == nil, "binary choice key must be omitted, not null")
        #expect(object["option_indices"] == nil, "option_indices key must be omitted, not null")
        #expect(object["free_text"] == nil, "free_text key must be omitted, not null")
        #expect(object["answers"] == nil, "answers key must be omitted, not null")

        let decoded = try JSONDecoder().decode(Wire.PhoneCommand.self, from: encoded)
        #expect(decoded == command)
    }

    /// Encoding a multi-select `option_indices` decision omits the single-select
    /// `option_index`/`choice`/`free_text` keys, and round-trips.
    @Test func optionIndicesDecisionEncodesWithoutOtherFields() throws {
        let command = Wire.PhoneCommand(
            commandId: Wire.CommandId("cmd_ois"),
            issuedAtMs: 1_752_412_811_000,
            body: .permissionDecision(
                sessionId: Wire.SessionId("sess_fix_login"),
                promptId: Wire.PromptId("prompt_ms1"),
                choice: nil, optionIndex: nil,
                optionIndices: [0, 2], freeText: nil, answers: nil))
        let encoded = try JSONEncoder().encode(command)
        let object = try #require(
            try JSONSerialization.jsonObject(with: encoded) as? NSDictionary)

        #expect(object["type"] as? String == "permission_decision")
        #expect(object["option_indices"] as? [Int] == [0, 2])
        #expect(object["choice"] == nil)
        #expect(object["option_index"] == nil)
        #expect(object["free_text"] == nil)

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
                choice: nil, optionIndex: nil, optionIndices: nil, freeText: "Use CockroachDB instead.", answers: nil))
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
            choice: .deny, optionIndex: nil, optionIndices: nil, freeText: nil, answers: nil))

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

    // MARK: - Hello relay_password (remote-control-uq7)

    /// A `hello` with no relay password must OMIT the `relay_password` key
    /// entirely (mirrors the Rust `skip_serializing_if`), so an unconfigured
    /// relay — and older clients — keep working. NOT emitted as explicit null.
    @Test func helloOmitsRelayPasswordKeyWhenNil() throws {
        let frame = Wire.RelayFrame.hello(
            protocolVersion: Wire.protocolVersion,
            role: .phone,
            deviceId: Wire.DeviceId("dev_x"),
            client: Wire.ClientInfo(appVersion: "1.0", platform: "ios", osVersion: nil),
            relayPassword: nil)
        let object = try #require(
            try JSONSerialization.jsonObject(with: JSONEncoder().encode(frame)) as? NSDictionary)
        #expect(object["type"] as? String == "hello")
        #expect(object["relay_password"] == nil, "relay_password must be absent when nil, not explicit null")
        #expect(object.allKeys.contains { ($0 as? String) == "relay_password" } == false)
    }

    /// A `hello` WITH a password emits it under the snake_case `relay_password`
    /// key the relay reads.
    @Test func helloEmitsRelayPasswordKeyWhenSet() throws {
        let frame = Wire.RelayFrame.hello(
            protocolVersion: Wire.protocolVersion,
            role: .phone,
            deviceId: Wire.DeviceId("dev_x"),
            client: Wire.ClientInfo(appVersion: "1.0", platform: "ios", osVersion: nil),
            relayPassword: "s3cret")
        let object = try #require(
            try JSONSerialization.jsonObject(with: JSONEncoder().encode(frame)) as? NSDictionary)
        #expect(object["relay_password"] as? String == "s3cret")
    }

    /// Round-trips both presence and absence of the password through
    /// decode(encode(·)).
    @Test func helloRelayPasswordRoundTrips() throws {
        for password in [nil, "hunter2"] as [String?] {
            let frame = Wire.RelayFrame.hello(
                protocolVersion: Wire.protocolVersion,
                role: .phone,
                deviceId: Wire.DeviceId("dev_x"),
                client: Wire.ClientInfo(appVersion: "1.0", platform: "ios", osVersion: nil),
                relayPassword: password)
            let decoded = try JSONDecoder().decode(
                Wire.RelayFrame.self, from: JSONEncoder().encode(frame))
            guard case let .hello(_, _, _, _, decodedPassword) = decoded else {
                Issue.record("expected .hello, got \(decoded)")
                return
            }
            #expect(decodedPassword == password)
        }
    }

    /// A hello JSON WITHOUT the key (an older client / unconfigured relay)
    /// decodes to `relayPassword == nil` rather than failing.
    @Test func helloDecodesToNilWhenKeyAbsent() throws {
        let json = """
        {"type":"hello","protocol_version":2,"role":"phone","device_id":"dev_x",\
        "client":{"app_version":"1.0","platform":"ios","os_version":null}}
        """
        let decoded = try JSONDecoder().decode(Wire.RelayFrame.self, from: Data(json.utf8))
        guard case let .hello(_, _, _, _, relayPassword) = decoded else {
            Issue.record("expected .hello, got \(decoded)")
            return
        }
        #expect(relayPassword == nil)
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
