//
//  PairingModelsTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the "fdr1:" QR payload format round-trips through
//  `PairingQRCodec` (PairingModels.swift) and that malformed input is
//  rejected with a typed `PairingError`, never a crash or a silent nil.
//

import Testing
import Foundation
@testable import FlightDeckRemote

struct PairingModelsTests {

    @Test func qrPayloadEncodeDecodeRoundTrips() throws {
        let payload = PairingQRPayload(
            claimToken: "clm_9f3a7c21",
            pairingSecret: "c29tZS1zZWNyZXQtYnl0ZXM",
            relayURL: URL(string: "wss://relay.flightdeck.app/v1")!
        )

        let encoded = try PairingQRCodec.encode(payload)
        #expect(encoded.hasPrefix("fdr1:"))

        let decoded = try PairingQRCodec.decode(encoded)
        #expect(decoded == payload)
    }

    @Test func encodedPayloadCarriesSnakeCaseJSONKeys() throws {
        let payload = PairingQRPayload(
            claimToken: "clm_9f3a7c21",
            pairingSecret: "c29tZS1zZWNyZXQtYnl0ZXM",
            relayURL: URL(string: "wss://relay.flightdeck.app/v1")!
        )
        let encoded = try PairingQRCodec.encode(payload)
        let base64url = String(encoded.dropFirst(PairingQRCodec.schemePrefix.count))
        let jsonData = try #require(Data(base64URLEncodedNoPadding: base64url))
        let jsonString = try #require(String(data: jsonData, encoding: .utf8))

        #expect(jsonString.contains("\"claim_token\""))
        #expect(jsonString.contains("\"pairing_secret\""))
        #expect(jsonString.contains("\"relay_url\""))
    }

    /// Asserts `decode` throws exactly `PairingError.malformedQRPayload` for
    /// the given raw string.
    private func expectMalformed(_ raw: String, sourceLocation: SourceLocation = #_sourceLocation) throws {
        #expect(throws: PairingError.self, sourceLocation: sourceLocation) {
            try PairingQRCodec.decode(raw)
        }
        do {
            _ = try PairingQRCodec.decode(raw)
            Issue.record("Expected decode to throw", sourceLocation: sourceLocation)
        } catch let error as PairingError {
            #expect(error == .malformedQRPayload, sourceLocation: sourceLocation)
        } catch {
            Issue.record("Expected PairingError, got \(error)", sourceLocation: sourceLocation)
        }
    }

    @Test func decodeRejectsWrongSchemePrefix() throws {
        try expectMalformed("fdr2:whatever")
    }

    @Test func decodeRejectsInvalidBase64() throws {
        try expectMalformed("fdr1:not-valid-base64url!!!")
    }

    @Test func decodeRejectsValidBase64ButNotJSON() throws {
        let garbage = "not json at all".data(using: .utf8)!.base64URLEncodedStringNoPadding()
        try expectMalformed("fdr1:" + garbage)
    }

    @Test func decodeRejectsEmptyPayload() throws {
        try expectMalformed("fdr1:")
    }

    @Test func decodeRejectsCompletelyUnrelatedString() throws {
        try expectMalformed("https://example.com/not-a-pairing-code")
    }
}
