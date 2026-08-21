//
//  DistributionMetadataTests.swift
//  FlightDeckRemoteTests
//
//  Guards the bundle metadata that App Store Connect validates at UPLOAD time.
//  Every failure here is one that otherwise surfaces as a rejected TestFlight
//  upload (or, worse, silently broken push for testers) long after the change
//  that caused it — and none of it is covered by building or running the app.
//
//  These read the built bundle, so they assert what actually shipped rather
//  than what project.yml says.
//

import Foundation
import Testing
@testable import FlightDeckRemote

struct DistributionMetadataTests {

    /// The unit-test target is hosted in the app (`TEST_HOST`), so `Bundle.main`
    /// is the app bundle — the same lookup Settings and the transport use to
    /// read the version.
    private var info: [String: Any] {
        Bundle.main.infoDictionary ?? [:]
    }

    // MARK: - Version

    /// A marketing version must be present and SemVer-ish: App Store Connect
    /// rejects anything that is not a dotted numeric string, and Settings →
    /// About renders it verbatim.
    @Test func marketingVersionIsDottedNumeric() throws {
        let version = try #require(info["CFBundleShortVersionString"] as? String)
        #expect(!version.isEmpty)

        let components = version.split(separator: ".")
        #expect((1...3).contains(components.count))
        // Computed outside `#expect`: `allSatisfy` is `rethrows`, which the
        // macro's autoclosure treats as a throwing call.
        let allNumeric = components.allSatisfy { !$0.isEmpty && $0.allSatisfy(\.isNumber) }
        #expect(allNumeric, "CFBundleShortVersionString must be dotted numeric, got \(version)")
    }

    /// The build number must be a positive integer. It has to strictly increase
    /// per upload, which a non-numeric or zero value makes impossible.
    @Test func buildNumberIsAPositiveInteger() throws {
        let build = try #require(info["CFBundleVersion"] as? String)
        let number = try #require(Int(build), "CFBundleVersion must be an integer, got \(build)")
        #expect(number > 0)
    }

    // MARK: - Export compliance

    /// Declared so App Store Connect stops asking the export-compliance
    /// questions on every upload. All crypto is CryptoKit, which qualifies for
    /// the OS-provided-encryption exemption — if that ever stops being true
    /// (a bundled crypto library, a hand-rolled cipher), this must be revisited
    /// rather than silently flipped.
    @Test func exportComplianceIsDeclared() throws {
        let exempt = try #require(
            info["ITSAppUsesNonExemptEncryption"] as? Bool,
            "ITSAppUsesNonExemptEncryption missing — every upload will prompt")
        #expect(exempt == false)
    }

    // MARK: - Device family

    /// The app is iPhone-only, and saying so is not cosmetic: an iPad-capable
    /// bundle is held to Apple's multitasking rules, which demand all four
    /// interface orientations and reject the upload with ITMS-90474 otherwise.
    ///
    /// This is worth asserting because the setting is easy to lose. XcodeGen
    /// defaults every iOS target to `TARGETED_DEVICE_FAMILY = "1,2"`, and a
    /// target-level setting overrides the project-level one — so the value in
    /// `project.yml`'s project-wide `settings.base` was silently ineffective and
    /// the app shipped as iPad-compatible until an upload failed.
    @Test func targetsIPhoneOnly() throws {
        let families = try #require(
            info["UIDeviceFamily"] as? [Int],
            "UIDeviceFamily missing from the built Info.plist")
        #expect(
            families == [1],
            """
            UIDeviceFamily is \(families), expected [1] (iPhone only). \
            2 means iPad, which triggers ITMS-90474 unless all four orientations \
            are declared. Set TARGETED_DEVICE_FAMILY on the app *target*.
            """)
    }

    /// Guards the pairing that actually produces ITMS-90474: an `~ipad`
    /// orientation list that omits upside-down. With the app iPhone-only there
    /// should be no `~ipad` variant at all.
    @Test func noIPadOrientationOverride() {
        #expect(
            info["UISupportedInterfaceOrientations~ipad"] == nil,
            "iPhone-only app should not declare iPad orientations")
    }

    // MARK: - Privacy manifest

    /// A missing manifest is an ITMS-91053 upload rejection.
    @Test func privacyManifestIsBundled() throws {
        let url = try #require(
            Bundle.main.url(forResource: "PrivacyInfo", withExtension: "xcprivacy"),
            "PrivacyInfo.xcprivacy is not in the app bundle")
        let manifest = try #require(
            NSDictionary(contentsOf: url) as? [String: Any],
            "PrivacyInfo.xcprivacy is not a readable plist")

        // No tracking anywhere in the app, and therefore no tracking domains.
        #expect(manifest["NSPrivacyTracking"] as? Bool == false)
        #expect((manifest["NSPrivacyTrackingDomains"] as? [String])?.isEmpty == true)
    }

    /// The app reads its own `UserDefaults` (app lock, pairing state, dictation
    /// language, notification prefs), which is a required-reason API. Declaring
    /// it with CA92.1 — "access info from same app, per documentation" — is what
    /// keeps uploads clean.
    @Test func privacyManifestDeclaresUserDefaultsReason() throws {
        let url = try #require(
            Bundle.main.url(forResource: "PrivacyInfo", withExtension: "xcprivacy"))
        let manifest = try #require(NSDictionary(contentsOf: url) as? [String: Any])
        let apis = try #require(manifest["NSPrivacyAccessedAPITypes"] as? [[String: Any]])

        let userDefaults = apis.first {
            $0["NSPrivacyAccessedAPIType"] as? String == "NSPrivacyAccessedAPICategoryUserDefaults"
        }
        let entry = try #require(userDefaults, "UserDefaults access is not declared")
        let reasons = try #require(entry["NSPrivacyAccessedAPITypeReasons"] as? [String])
        #expect(reasons.contains("CA92.1"))
    }

    // MARK: - Push environment

    /// The single most breakage-prone pairing in the whole distribution setup:
    /// the compiled push environment and the codesigned `aps-environment` must
    /// agree, or pushes are dropped by APNs with no build-time signal. A DEBUG
    /// test build must report sandbox; a Release build must report production.
    @Test func pushEnvironmentMatchesBuildConfiguration() {
        #if DEBUG
        #expect(PushEnvironment.current == .sandbox)
        #else
        #expect(PushEnvironment.current == .production)
        #endif
    }
}
