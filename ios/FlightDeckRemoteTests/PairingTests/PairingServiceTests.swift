//
//  PairingServiceTests.swift
//  FlightDeckRemoteTests
//
//  Verifies `MockPairingService`'s accept/reject paths (the stand-in used
//  until the relay transport lands — PairingService.swift) and that a
//  successful `pair(with:)` result, once handed to
//  `PairingStore.completePairing(with:)`, actually flips the store into
//  the paired state (the seam `PairingView` relies on to hand off to
//  `AppRouter`/`RootView`).
//

import Testing
import Foundation
@testable import FlightDeckRemote

struct PairingServiceTests {

    private let relayURL = URL(string: "wss://relay.flightdeck.app/v1")!
    private let noDelay = MockPairingService(delay: .zero)

    /// Asserts `pair(with:)` throws exactly `expected`, mirroring the
    /// `.self`-based `#expect(throws:)` pattern used elsewhere in this test
    /// target (see SecurityTests/E2EChannelVectorsTests.swift), plus an
    /// explicit case check since `PairingError` carries meaningful cases.
    private func expectRejection(
        _ input: PairingInput,
        matches expected: PairingError,
        sourceLocation: SourceLocation = #_sourceLocation
    ) async {
        await #expect(throws: PairingError.self, sourceLocation: sourceLocation) {
            _ = try await noDelay.pair(with: input)
        }
        do {
            _ = try await noDelay.pair(with: input)
            Issue.record("Expected pair(with:) to throw", sourceLocation: sourceLocation)
        } catch let error as PairingError {
            #expect(error == expected, sourceLocation: sourceLocation)
        } catch {
            Issue.record("Expected PairingError, got \(error)", sourceLocation: sourceLocation)
        }
    }

    // MARK: - Code path

    @Test func acceptsTheDocumentedValidCode() async throws {
        let device = try await noDelay.pair(with: .code("4729", relayURL: relayURL))
        #expect(!device.pairingId.isEmpty)
        #expect(!device.peerName.isEmpty)
    }

    @Test func rejectsAnyOtherCode() async {
        await expectRejection(.code("0000", relayURL: relayURL), matches: .invalidCode)
    }

    @Test func rejectsShortOrNonNumericCodeStrings() async {
        for bad in ["", "1", "abcd", "47290"] {
            await expectRejection(.code(bad, relayURL: relayURL), matches: .invalidCode)
        }
    }

    // MARK: - QR path

    @Test func acceptsAnyWellFormedQRPayload() async throws {
        let payload = PairingQRPayload(
            claimToken: "clm_anything",
            pairingSecret: "c2VjcmV0Ynl0ZXM",
            relayURL: relayURL
        )
        let device = try await noDelay.pair(with: .qr(payload))
        #expect(!device.pairingId.isEmpty)
    }

    @Test func rejectsQRPayloadWithEmptyClaimToken() async {
        let payload = PairingQRPayload(claimToken: "", pairingSecret: "c2VjcmV0", relayURL: relayURL)
        await expectRejection(.qr(payload), matches: .malformedQRPayload)
    }

    @Test func rejectsQRPayloadWithEmptySecret() async {
        let payload = PairingQRPayload(claimToken: "clm_1", pairingSecret: "", relayURL: relayURL)
        await expectRejection(.qr(payload), matches: .malformedQRPayload)
    }

    // MARK: - Integration with PairingStore

    @Test func successfulPairingCompletesTheStore() async throws {
        let store = PairingStore(storage: InMemoryPairingStateProvider())
        #expect(store.isPaired == false)

        let device = try await noDelay.pair(with: .code("4729", relayURL: relayURL))
        store.completePairing(with: device)

        #expect(store.isPaired == true)
        #expect(store.pairedDevice == device)
    }

    @Test func rejectedPairingLeavesStoreUnpaired() async {
        let store = PairingStore(storage: InMemoryPairingStateProvider())

        do {
            _ = try await noDelay.pair(with: .code("0000", relayURL: relayURL))
            Issue.record("Expected pairing to fail")
        } catch {
            // Expected — the view never calls completePairing on failure.
        }

        #expect(store.isPaired == false)
        #expect(store.pairedDevice == nil)
    }
}
