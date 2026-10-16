//
//  SharedContainer.swift
//  FlightDeckRemote + FlightDeckNotificationService
//
//  The identifiers the app and its Notification Service Extension share
//  (remote-control-bza0). Compiled into BOTH targets so neither can drift.
//
//  Two sharing mechanisms, because the extension is a separate process with its
//  own sandbox:
//
//   - **Keychain access group** `<AppIdentifierPrefix>agency.neworange.flightdeck.remote.shared`
//     holds exactly what the extension needs to open a sealed push (spec §7.2):
//     the phone's key-agreement private scalar and the per-pairing records
//     (pairing id, peer KA public key, salt). Everything else (the Secure
//     Enclave identity blob, the relay password) stays in the app's default
//     group, which the extension cannot read. Routing is by account name, see
//     `accessGroup(service:account:)`, so no call site has to know.
//   - **App Group** `group.agency.neworange.flightdeck.remote` backs a shared
//     `UserDefaults` suite for the notification preferences (the three toggles
//     and per-project mute) the extension must honour.
//
//  Both must be registered for team 7NKCS4AZS9 and present in the signing
//  profiles of both bundle ids (see ios/README.md "Push: Notification Service
//  Extension"). In an unsigned simulator build neither entitlement exists; the
//  keychain wrapper falls back to the default group and the suite to a local
//  domain, so the app keeps working there.
//

import Foundation

enum SharedContainer {
    /// The App Group shared by the app and the Notification Service Extension.
    static let appGroupId = "group.agency.neworange.flightdeck.remote"

    /// The Keychain service every FlightDeck Remote item lives under (shared
    /// with `DeviceIdentity`, `KeyAgreementKeys`, `PairingRecordStore`).
    static let keychainService = "agency.neworange.flightdeck.remote"

    /// Keychain account of the phone's software key-agreement private scalar
    /// (`KeyAgreementKeys`). One per device, used by every pairing.
    static let keyAgreementAccount = "device-key-agreement-key"

    /// Account prefix of the per-pairing records (`PairingRecordStore`).
    static let pairingRecordAccountPrefix = "pairing-record-v1."

    /// Info.plist key carrying the fully-qualified shared keychain group
    /// (`$(AppIdentifierPrefix)` is only known at build time).
    static let keychainGroupInfoKey = "FlightDeckSharedKeychainGroup"

    /// The unqualified shared keychain group, appended to the team prefix.
    static let keychainGroupSuffix = "agency.neworange.flightdeck.remote.shared"

    /// The fully-qualified shared keychain access group, e.g.
    /// `7NKCS4AZS9.agency.neworange.flightdeck.remote.shared`. Read from the
    /// bundle's Info.plist so the team prefix is never hardcoded; falls back to
    /// the known team id if the key is missing or was left unsubstituted.
    static var keychainAccessGroup: String {
        keychainAccessGroup(from: Bundle.main.infoDictionary ?? [:])
    }

    /// Testable core of `keychainAccessGroup`.
    static func keychainAccessGroup(from info: [String: Any]) -> String {
        if let value = info[keychainGroupInfoKey] as? String,
           value.hasSuffix(keychainGroupSuffix),
           !value.contains("$("),
           value.count > keychainGroupSuffix.count {
            return value
        }
        return "7NKCS4AZS9." + keychainGroupSuffix
    }

    /// Whether an item belongs in the shared keychain group: only the material
    /// the extension needs to derive a pairing's push key.
    static func isSharedKeychainItem(service: String, account: String) -> Bool {
        guard service == keychainService else { return false }
        return account == keyAgreementAccount || account.hasPrefix(pairingRecordAccountPrefix)
    }

    /// The access group a new item should be written into: the shared group for
    /// the extension's material, `nil` (the app's default group) otherwise.
    static func accessGroup(service: String, account: String) -> String? {
        isSharedKeychainItem(service: service, account: account) ? keychainAccessGroup : nil
    }

    /// The shared `UserDefaults` suite. `UserDefaults(suiteName:)` only returns
    /// `nil` for invalid names, so in practice this is always the group suite;
    /// without the entitlement (unsigned simulator) it is a process-local domain.
    static var defaults: UserDefaults {
        UserDefaults(suiteName: appGroupId) ?? .standard
    }
}
