//
//  RelayPasswordStoreTests.swift
//  FlightDeckRemoteTests
//
//  Verifies the shared relay password (remote-control-uq7) round-trips through
//  secure storage: save/load, empty/blank normalizes to nil (never a
//  present-but-empty value a configured relay would reject), whitespace is
//  trimmed, and clearing removes the item. Uses the in-memory KeychainStoring
//  so the test is hermetic (no simulator Keychain-entitlement flakiness).
//

import Testing
import Foundation
@testable import FlightDeckRemote

struct RelayPasswordStoreTests {

    @Test func savesAndLoadsAPassword() throws {
        let store = RelayPasswordStore(store: InMemoryKeychainStore())
        #expect(store.load() == nil, "no password stored initially")

        try store.save("hunter2")
        #expect(store.load() == "hunter2")
    }

    @Test func emptyAndBlankNormalizeToNil() throws {
        let keychain = InMemoryKeychainStore()
        let store = RelayPasswordStore(store: keychain)

        try store.save("hunter2")
        #expect(store.load() == "hunter2")

        // Saving empty/blank clears it rather than storing "".
        try store.save("")
        #expect(store.load() == nil)

        try store.save("hunter2")
        try store.save("   ")
        #expect(store.load() == nil)

        try store.save("hunter2")
        try store.save(nil)
        #expect(store.load() == nil)
    }

    @Test func trimsSurroundingWhitespace() throws {
        let store = RelayPasswordStore(store: InMemoryKeychainStore())
        try store.save("  s3cret\n")
        #expect(store.load() == "s3cret")
    }

    @Test func deleteRemovesTheStoredPassword() throws {
        let store = RelayPasswordStore(store: InMemoryKeychainStore())
        try store.save("hunter2")
        try store.delete()
        #expect(store.load() == nil)
    }

    @Test func loadReturnsNilForNonUTF8Bytes() throws {
        let keychain = InMemoryKeychainStore()
        // Write raw invalid-UTF8 bytes directly under the store's account.
        try keychain.set(Data([0xFF, 0xFE, 0xFD]), account: RelayPasswordStore.account)
        let store = RelayPasswordStore(store: keychain)
        #expect(store.load() == nil)
    }
}
