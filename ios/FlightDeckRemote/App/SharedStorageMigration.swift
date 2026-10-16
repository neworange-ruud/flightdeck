//
//  SharedStorageMigration.swift
//  FlightDeckRemote
//
//  Launch-time move of what the Notification Service Extension needs into the
//  places it can read (remote-control-bza0), so an existing install keeps its
//  pairings and its notification settings without re-pairing:
//
//   - Keychain: the key-agreement private key and every pairing record move
//     from the app's default access group into the shared group
//     (`KeychainGroupMigration`, crash-safe copy → verify → delete). New writes
//     already land there (`KeychainStore` routes by account).
//   - Settings: the notification toggles and per-project mute are copied from
//     `UserDefaults.standard` into the App Group suite, once.
//
//  Idempotent and cheap (one Keychain attribute query when there is nothing to
//  do), so it simply runs on every launch: a write that fell back to the
//  default group (profile briefly missing the entitlement) is picked up on the
//  next launch. Every failure is swallowed — the app keeps working from the old
//  locations, only the extension's decryption is affected.
//

import Foundation
import os

enum SharedStorageMigration {
    private static let logger = Logger(subsystem: "agency.neworange.flightdeck.remote", category: "migration")

    /// Run both migrations against the real Keychain and defaults.
    static func run() {
        migrateKeychain(
            store: SystemKeychainGroupMigrator(service: SharedContainer.keychainService),
            targetGroup: SharedContainer.keychainAccessGroup)
        NotificationSettingsMigration.migrateIfNeeded(from: .standard, to: SharedContainer.defaults)
    }

    /// The Keychain half, injectable for tests.
    @discardableResult
    static func migrateKeychain(store: KeychainGroupMigrating, targetGroup: String) -> KeychainGroupMigration.Report? {
        do {
            let report = try KeychainGroupMigration.run(store: store, targetGroup: targetGroup) { account in
                SharedContainer.isSharedKeychainItem(service: SharedContainer.keychainService, account: account)
            }
            if report.moved > 0 {
                logger.info("moved \(report.moved, privacy: .public) keychain item(s) into the shared group")
            }
            return report
        } catch {
            // Expected in unsigned simulator builds (no Keychain entitlement).
            logger.notice("keychain migration skipped: \(String(describing: error), privacy: .public)")
            return nil
        }
    }
}
