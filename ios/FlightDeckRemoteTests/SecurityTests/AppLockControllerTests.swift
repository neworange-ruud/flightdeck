//
//  AppLockControllerTests.swift
//  FlightDeckRemoteTests
//
//  Verifies `AppLockController`'s persistence (via `AppLockSettingsProviding`,
//  mirroring `PairingStoreTests`'s pattern), the lock/unlock state machine
//  driven through a mock `BiometricAuthenticating`, the "auto-attempt once"
//  rule, and the fail-open safety valve (PRD §9 Face-ID gate — never brick
//  the app on a passcode-less device).
//

import Testing
import LocalAuthentication
@testable import FlightDeckRemote

/// In-memory `AppLockSettingsProviding` — mirrors `InMemoryPairingStateProvider`.
final class InMemoryAppLockSettingsProvider: AppLockSettingsProviding {
    private var stored: Bool

    init(initial: Bool = false) {
        stored = initial
    }

    func loadIsLockEnabled() -> Bool { stored }
    func saveIsLockEnabled(_ isEnabled: Bool) { stored = isEnabled }
}

/// Configurable mock `BiometricAuthenticating` for deterministic tests —
/// mirrors the `InMemoryKeychainStore` pattern used by `DeviceIdentityTests`.
final class MockBiometricAuthenticator: BiometricAuthenticating {
    var canEvaluateResult: (Bool, Error?) = (true, nil)
    var evaluateResult: Result<Void, Error> = .success(())
    private(set) var evaluateCallCount = 0

    func canEvaluate(policy: LAPolicy) -> (canEvaluate: Bool, error: Error?) {
        canEvaluateResult
    }

    func evaluate(policy: LAPolicy, reason: String) async -> Result<Void, Error> {
        evaluateCallCount += 1
        return evaluateResult
    }
}

private struct StubError: Error {}

struct AppLockControllerTests {

    // MARK: - Defaults & persistence

    @Test func defaultsToDisabledAndUnlocked() {
        let controller = AppLockController(
            settings: InMemoryAppLockSettingsProvider(),
            authenticator: MockBiometricAuthenticator()
        )
        #expect(controller.isLockEnabled == false)
        #expect(controller.lockState == .unlocked)
    }

    @Test func enablingLockPersistsToProvider() {
        let provider = InMemoryAppLockSettingsProvider()
        let controller = AppLockController(settings: provider, authenticator: MockBiometricAuthenticator())

        controller.isLockEnabled = true
        #expect(provider.loadIsLockEnabled() == true)

        // A second controller reading the same provider observes the change.
        let reloaded = AppLockController(settings: provider, authenticator: MockBiometricAuthenticator())
        #expect(reloaded.isLockEnabled == true)
    }

    @Test func startsLockedWhenPersistedEnabled() {
        let provider = InMemoryAppLockSettingsProvider(initial: true)
        let controller = AppLockController(settings: provider, authenticator: MockBiometricAuthenticator())
        #expect(controller.lockState == .locked)
    }

    // Note: `-uitest-reset-applock` (mirrors `-uitest-reset-pairing`) reads
    // the real process's `ProcessInfo.processInfo.arguments`, so — like
    // `-uitest-enable-applock` above it, which also has no unit test — it
    // isn't unit-testable in isolation. It's exercised end-to-end by
    // `SettingsUITests.testFaceIDTogglePersistsAcrossRelaunch`.

    @Test func disablingLockUnlocksImmediately() {
        let provider = InMemoryAppLockSettingsProvider(initial: true)
        let controller = AppLockController(settings: provider, authenticator: MockBiometricAuthenticator())
        #expect(controller.lockState == .locked)

        controller.isLockEnabled = false
        #expect(controller.lockState == .unlocked)
    }

    // MARK: - Unlock paths

    @Test func unlockSucceedsWithMockAuthenticator() async {
        let mock = MockBiometricAuthenticator()
        mock.evaluateResult = .success(())
        let controller = AppLockController(
            settings: InMemoryAppLockSettingsProvider(initial: true),
            authenticator: mock
        )

        await controller.unlock()
        #expect(controller.lockState == .unlocked)
        #expect(mock.evaluateCallCount == 1)
    }

    @Test func unlockFailurePreservesLockAndReportsMessage() async {
        let mock = MockBiometricAuthenticator()
        mock.evaluateResult = .failure(StubError())
        let controller = AppLockController(
            settings: InMemoryAppLockSettingsProvider(initial: true),
            authenticator: mock
        )

        await controller.unlock()
        guard case .failed = controller.lockState else {
            Issue.record("Expected .failed lock state after a failed evaluation, got \(controller.lockState)")
            return
        }
    }

    @Test func unlockNoOpsWhenDisabled() async {
        let mock = MockBiometricAuthenticator()
        let controller = AppLockController(
            settings: InMemoryAppLockSettingsProvider(initial: false),
            authenticator: mock
        )

        await controller.unlock()
        #expect(controller.lockState == .unlocked)
        #expect(mock.evaluateCallCount == 0, "Should not evaluate biometrics when the gate is disabled")
    }

    // MARK: - Fail-open

    @Test func failsOpenWhenDeviceCannotEvaluateAnyPolicy() async {
        let mock = MockBiometricAuthenticator()
        mock.canEvaluateResult = (false, StubError())
        let controller = AppLockController(
            settings: InMemoryAppLockSettingsProvider(initial: true),
            authenticator: mock
        )

        await controller.unlock()
        // v1 fail-open decision: never brick the app if there's no
        // authentication method (e.g. no device passcode) to satisfy at all.
        #expect(controller.lockState == .unlocked)
        #expect(mock.evaluateCallCount == 0, "Should not attempt evaluate() when canEvaluate reports false")
    }

    // MARK: - scenePhase lock trigger (lockIfEnabled)

    @Test func lockIfEnabledIsNoOpWhenGateDisabled() {
        let controller = AppLockController(
            settings: InMemoryAppLockSettingsProvider(initial: false),
            authenticator: MockBiometricAuthenticator()
        )
        controller.lockIfEnabled()
        #expect(controller.lockState == .unlocked)
    }

    @Test func lockIfEnabledLocksAfterAnUnlockedSession() async {
        let mock = MockBiometricAuthenticator()
        let controller = AppLockController(
            settings: InMemoryAppLockSettingsProvider(initial: true),
            authenticator: mock
        )

        await controller.unlock()
        #expect(controller.lockState == .unlocked)

        // Simulates the scenePhase → .background transition.
        controller.lockIfEnabled()
        #expect(controller.lockState == .locked)
    }

    // MARK: - Auto-unlock-once

    @Test func autoUnlockAttemptsOnceThenIgnoresRepeatCalls() async {
        let mock = MockBiometricAuthenticator()
        mock.evaluateResult = .failure(StubError())
        let controller = AppLockController(
            settings: InMemoryAppLockSettingsProvider(initial: true),
            authenticator: mock
        )

        await controller.autoUnlockIfNeeded()
        #expect(mock.evaluateCallCount == 1)

        // A second auto-attempt (e.g. a SwiftUI re-render) must not
        // re-trigger biometrics automatically — only a manual `unlock()`
        // call (the retry button) may.
        await controller.autoUnlockIfNeeded()
        #expect(mock.evaluateCallCount == 1)
    }

    @Test func lockIfEnabledResetsTheAutoUnlockOnceGate() async {
        let mock = MockBiometricAuthenticator()
        mock.evaluateResult = .failure(StubError())
        let controller = AppLockController(
            settings: InMemoryAppLockSettingsProvider(initial: true),
            authenticator: mock
        )

        await controller.autoUnlockIfNeeded()
        #expect(mock.evaluateCallCount == 1)
        guard case .failed = controller.lockState else {
            Issue.record("Expected .failed lock state after the auto-attempt failed")
            return
        }

        // A fresh lock episode (scenePhase → .background, then viewed
        // again) resets the "auto once" gate, so the next auto-attempt
        // fires again rather than being suppressed forever.
        mock.evaluateResult = .success(())
        controller.lockIfEnabled()
        #expect(controller.lockState == .locked)

        await controller.autoUnlockIfNeeded()
        #expect(mock.evaluateCallCount == 2)
        #expect(controller.lockState == .unlocked)
    }
}
