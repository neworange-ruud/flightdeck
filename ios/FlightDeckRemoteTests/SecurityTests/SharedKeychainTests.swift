//
//  SharedKeychainTests.swift
//  FlightDeckRemoteTests
//
//  The shared Keychain group the Notification Service Extension reads
//  (remote-control-bza0): which items are routed there, how the group name is
//  resolved, and the crash-safe launch migration that moves an existing
//  install's pairing material into it — so nobody has to re-pair.
//
//  The unsigned simulator test host has no Keychain entitlement, so the
//  migration runs against an in-memory, access-group-aware double.
//

import Foundation
import Security
import Testing
@testable import FlightDeckRemote

/// In-memory Keychain keyed by (access group, account), like the real one.
final class MultiGroupKeychain: KeychainGroupMigrating {
    struct Key: Hashable {
        let group: String?
        let account: String
    }

    var items: [Key: Data] = [:]
    /// Ordered like the real enumeration (insertion order).
    private var order: [Key] = []
    var addError: Error?
    var dropTargetWrites = false
    private(set) var deletes: [Key] = []

    func put(_ data: Data, account: String, group: String?) {
        let key = Key(group: group, account: account)
        if items[key] == nil { order.append(key) }
        items[key] = data
    }

    func locations() throws -> [KeychainItemLocation] {
        order.filter { items[$0] != nil }.map { KeychainItemLocation(account: $0.account, accessGroup: $0.group) }
    }

    func data(account: String, accessGroup: String?) throws -> Data? {
        items[Key(group: accessGroup, account: account)]
    }

    func add(_ data: Data, account: String, accessGroup: String) throws -> Bool {
        if let addError { throw addError }
        let key = Key(group: accessGroup, account: account)
        if items[key] != nil { return false }
        if dropTargetWrites { return true } // simulate a write that never lands
        put(data, account: account, group: accessGroup)
        return true
    }

    func delete(account: String, accessGroup: String?) throws {
        let key = Key(group: accessGroup, account: account)
        deletes.append(key)
        items[key] = nil
    }

    func groups(of account: String) -> Set<String?> {
        Set(items.keys.filter { $0.account == account }.map(\.group))
    }
}

struct SharedKeychainTests {
    private let appGroup = "7NKCS4AZS9.agency.neworange.flightdeck.remote"
    private let shared = "7NKCS4AZS9.agency.neworange.flightdeck.remote.shared"
    private let isShared: (String) -> Bool = {
        SharedContainer.isSharedKeychainItem(service: SharedContainer.keychainService, account: $0)
    }

    // MARK: - Routing

    @Test func onlyThePushKeyMaterialIsShared() {
        let service = SharedContainer.keychainService
        #expect(SharedContainer.isSharedKeychainItem(service: service, account: KeyAgreementKeys.keyAccount))
        #expect(SharedContainer.isSharedKeychainItem(service: service, account: PairingRecordStore.account(for: "pair_a")))
        // Not the Secure Enclave identity blob, the relay password, or the
        // legacy single record (migrated into a keyed one first).
        #expect(!SharedContainer.isSharedKeychainItem(service: service, account: DeviceIdentity.keyAccount))
        #expect(!SharedContainer.isSharedKeychainItem(service: service, account: RelayPasswordStore.account))
        #expect(!SharedContainer.isSharedKeychainItem(service: service, account: PairingRecordStore.legacyAccount))
        // And nothing under another service.
        #expect(!SharedContainer.isSharedKeychainItem(service: "other", account: KeyAgreementKeys.keyAccount))
        #expect(SharedContainer.accessGroup(service: service, account: DeviceIdentity.keyAccount) == nil)
    }

    @Test func storesKeepTheirAccountsInLockstepWithTheSharedConstants() {
        #expect(DeviceIdentity.service == SharedContainer.keychainService)
        #expect(KeyAgreementKeys.service == SharedContainer.keychainService)
        #expect(PairingRecordStore.service == SharedContainer.keychainService)
        #expect(KeyAgreementKeys.keyAccount == "device-key-agreement-key")
        #expect(PairingRecordStore.accountPrefix == "pairing-record-v1.")
    }

    @Test func accessGroupResolvesFromInfoPlistWithAFallback() {
        let key = SharedContainer.keychainGroupInfoKey
        #expect(SharedContainer.keychainAccessGroup(from: [key: "ABCDE12345.agency.neworange.flightdeck.remote.shared"])
            == "ABCDE12345.agency.neworange.flightdeck.remote.shared")
        // Unsigned builds leave $(AppIdentifierPrefix) empty; an unexpanded
        // reference or a missing key must not produce a bogus group.
        #expect(SharedContainer.keychainAccessGroup(from: [key: "agency.neworange.flightdeck.remote.shared"]) == shared)
        #expect(SharedContainer.keychainAccessGroup(from: [key: "$(AppIdentifierPrefix)agency.neworange.flightdeck.remote.shared"]) == shared)
        #expect(SharedContainer.keychainAccessGroup(from: [key: "evil.group"]) == shared)
        #expect(SharedContainer.keychainAccessGroup(from: [:]) == shared)
    }

    @Test func theBuiltBundlesDeclareTheSharedGroup() throws {
        let value = try #require(Bundle.main.infoDictionary?[SharedContainer.keychainGroupInfoKey] as? String)
        #expect(value.hasSuffix(SharedContainer.keychainGroupSuffix))
        #expect(SharedContainer.keychainAccessGroup.hasSuffix(SharedContainer.keychainGroupSuffix))
    }

    // MARK: - Migration

    private func seededLegacyInstall() -> MultiGroupKeychain {
        let keychain = MultiGroupKeychain()
        keychain.put(Data("se-blob".utf8), account: DeviceIdentity.keyAccount, group: appGroup)
        keychain.put(Data("ka-scalar".utf8), account: KeyAgreementKeys.keyAccount, group: appGroup)
        keychain.put(Data("record-a".utf8), account: PairingRecordStore.account(for: "pair_a"), group: appGroup)
        keychain.put(Data("record-b".utf8), account: PairingRecordStore.account(for: "pair_b"), group: appGroup)
        keychain.put(Data("pw".utf8), account: RelayPasswordStore.account, group: appGroup)
        return keychain
    }

    @Test func migrationMovesExactlyThePushMaterialAndKeepsTheData() throws {
        let keychain = seededLegacyInstall()
        let report = try KeychainGroupMigration.run(store: keychain, targetGroup: shared, shouldMove: isShared)
        #expect(report == .init(moved: 3, alreadyShared: 0))

        #expect(keychain.groups(of: KeyAgreementKeys.keyAccount) == [shared])
        #expect(keychain.groups(of: PairingRecordStore.account(for: "pair_a")) == [shared])
        #expect(keychain.groups(of: PairingRecordStore.account(for: "pair_b")) == [shared])
        #expect(try keychain.data(account: KeyAgreementKeys.keyAccount, accessGroup: shared) == Data("ka-scalar".utf8))
        #expect(try keychain.data(account: PairingRecordStore.account(for: "pair_b"), accessGroup: shared) == Data("record-b".utf8))
        // Untouched: identity key and relay password stay in the app's group.
        #expect(keychain.groups(of: DeviceIdentity.keyAccount) == [appGroup])
        #expect(keychain.groups(of: RelayPasswordStore.account) == [appGroup])
    }

    @Test func migrationIsIdempotent() throws {
        let keychain = seededLegacyInstall()
        try KeychainGroupMigration.run(store: keychain, targetGroup: shared, shouldMove: isShared)
        let again = try KeychainGroupMigration.run(store: keychain, targetGroup: shared, shouldMove: isShared)
        #expect(again == .init(moved: 0, alreadyShared: 3))
    }

    @Test func anInterruptedMigrationCompletesAndKeepsTheSharedCopy() throws {
        // A previous run copied the record into the shared group, then the app
        // died before deleting the original. The shared copy is authoritative.
        let keychain = MultiGroupKeychain()
        let account = PairingRecordStore.account(for: "pair_a")
        keychain.put(Data("old".utf8), account: account, group: appGroup)
        keychain.put(Data("new".utf8), account: account, group: shared)

        let report = try KeychainGroupMigration.run(store: keychain, targetGroup: shared, shouldMove: isShared)
        #expect(report.moved == 1)
        #expect(keychain.groups(of: account) == [shared])
        #expect(try keychain.data(account: account, accessGroup: shared) == Data("new".utf8))
    }

    @Test func aMissingEntitlementAbortsWithTheOriginalsUntouched() throws {
        let keychain = seededLegacyInstall()
        keychain.addError = KeychainError.unexpectedStatus(errSecMissingEntitlement)
        #expect(throws: KeychainError.unexpectedStatus(errSecMissingEntitlement)) {
            try KeychainGroupMigration.run(store: keychain, targetGroup: shared, shouldMove: isShared)
        }
        #expect(keychain.deletes.isEmpty)
        #expect(keychain.groups(of: KeyAgreementKeys.keyAccount) == [appGroup])
        // The app-level wrapper swallows it (retried next launch).
        #expect(SharedStorageMigration.migrateKeychain(store: keychain, targetGroup: shared) == nil)
    }

    @Test func aCopyThatDoesNotReadBackIsNeverFollowedByADelete() throws {
        let keychain = seededLegacyInstall()
        keychain.dropTargetWrites = true
        let report = try KeychainGroupMigration.run(store: keychain, targetGroup: shared, shouldMove: isShared)
        #expect(report.moved == 0)
        #expect(keychain.deletes.isEmpty)
        #expect(keychain.groups(of: PairingRecordStore.account(for: "pair_a")) == [appGroup])
    }

    @Test func enumerationDuringAMoveListsEachAccountOnce() {
        // Mid-migration an account exists in two groups; `PairingRecordStore`
        // must still see one pairing, not two.
        #expect(KeychainGroupMigration.uniqueAccounts(["a", "b", "a", "c", "b"]) == ["a", "b", "c"])
    }
}

struct NotificationSettingsMigrationTests {
    private func freshDefaults() -> UserDefaults {
        let name = "test.notif-migration.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name)!
        defaults.removePersistentDomain(forName: name)
        return defaults
    }

    @Test func copiesAnExistingInstallsSettingsIntoTheAppGroup() {
        let standard = freshDefaults()
        let group = freshDefaults()
        UserDefaultsNotificationSettingsStore(defaults: standard).save(NotificationSettings(
            agentNeedsInput: false, agentFinished: true, completionChime: false, mutedProjectIds: ["p1", "p2"]))

        #expect(NotificationSettingsMigration.migrateIfNeeded(from: standard, to: group))
        let migrated = UserDefaultsNotificationSettingsStore(defaults: group).load()
        #expect(migrated == NotificationSettings(
            agentNeedsInput: false, agentFinished: true, completionChime: false, mutedProjectIds: ["p1", "p2"]))
        // Source left intact (a downgrade still finds its settings).
        #expect(UserDefaultsNotificationSettingsStore(defaults: standard).load().agentNeedsInput == false)
    }

    @Test func runsOnceAndNeverOverwritesNewerGroupValues() {
        let standard = freshDefaults()
        let group = freshDefaults()
        standard.set(false, forKey: UserDefaultsNotificationSettingsStore.Key.chime)
        group.set(true, forKey: UserDefaultsNotificationSettingsStore.Key.chime)

        #expect(NotificationSettingsMigration.migrateIfNeeded(from: standard, to: group))
        #expect(UserDefaultsNotificationSettingsStore(defaults: group).load().completionChime == true)

        // Later changes in the old domain are not re-copied.
        standard.set(false, forKey: UserDefaultsNotificationSettingsStore.Key.needsInput)
        #expect(!NotificationSettingsMigration.migrateIfNeeded(from: standard, to: group))
        #expect(UserDefaultsNotificationSettingsStore(defaults: group).load().agentNeedsInput == true)
    }

    @Test func aFreshInstallMigratesNothingAndKeepsDefaults() {
        let group = freshDefaults()
        #expect(NotificationSettingsMigration.migrateIfNeeded(from: freshDefaults(), to: group))
        #expect(UserDefaultsNotificationSettingsStore(defaults: group).load() == NotificationSettings())
    }

    @Test func theDefaultStoreIsTheAppGroupSuite() {
        // What the Settings screen writes is what the extension reads.
        let group = SharedContainer.defaults
        let key = UserDefaultsNotificationSettingsStore.Key.muted
        let previous = group.object(forKey: key)
        defer { group.set(previous, forKey: key) }
        group.set(["proj_shared_probe"], forKey: key)
        #expect(UserDefaultsNotificationSettingsStore().load().mutedProjectIds.contains("proj_shared_probe"))
    }
}
