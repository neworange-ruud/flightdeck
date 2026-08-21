//
//  PairingServiceFactoryTests.swift
//  FlightDeckRemoteTests
//
//  Locks in the service-selection rules that stop MockPairingService from
//  being a silent footgun (remote-control-lae): UI tests stay hermetic on the
//  mock, an explicit env override wins, and the DEBUG default is the mock only
//  on the simulator — a physical DEBUG device takes the real relay path.
//

import Testing
import Foundation
@testable import FlightDeckRemote

struct PairingServiceFactoryTests {

    @Test func uiTestArgumentAlwaysSelectsTheMock() {
        // Hermetic UI tests: `-uitest…` pins the deterministic mock even if the
        // environment asks for the real service.
        #expect(
            PairingServiceFactory.isMock(
                arguments: ["xctest", "-uitest-fixture-snapshot"],
                environment: ["FLIGHTDECK_PAIRING": "real"]
            )
        )
        #expect(
            PairingServiceFactory.makeDefault(
                arguments: ["xctest", "-uitest"],
                environment: [:]
            ) is MockPairingService
        )
    }

    @Test func realOverrideSelectsTheRealService() {
        #expect(
            !PairingServiceFactory.isMock(arguments: [], environment: ["FLIGHTDECK_PAIRING": "real"])
        )
        #expect(
            !(PairingServiceFactory.makeDefault(arguments: [], environment: ["FLIGHTDECK_PAIRING": "real"])
                is MockPairingService)
        )
    }

    @Test func mockOverrideSelectsTheMock() {
        #expect(
            PairingServiceFactory.isMock(arguments: [], environment: ["FLIGHTDECK_PAIRING": "mock"])
        )
    }

    #if DEBUG && targetEnvironment(simulator)
    @Test func debugSimulatorDefaultsToMock() {
        // On the simulator with no args/env the developer default is the mock
        // (no paired Mac needed) — but it is surfaced loudly + badged.
        #expect(PairingServiceFactory.isMock(arguments: [], environment: [:]))
    }
    #endif
}
