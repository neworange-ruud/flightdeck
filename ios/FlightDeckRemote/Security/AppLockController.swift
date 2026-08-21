//
//  AppLockController.swift
//  FlightDeckRemote
//
//  Face-ID app-open gate (PRD §5.6/§9: "Face-ID gates app-open (optional
//  toggle in Settings)" — pairing itself persists until unpaired; this is a
//  separate, independently-toggleable local gate on the app's UI).
//
//  Persistence goes through `AppLockSettingsProviding` (a small seam, same
//  shape as `PairingStateProviding`) so the Settings feature task can wire
//  its toggle straight to `isLockEnabled` without touching storage, and so
//  tests can inject an in-memory provider.
//
//  Biometric evaluation goes through `BiometricAuthenticating` rather than
//  calling `LAContext` directly, so tests can drive every path (success,
//  failure, "can't evaluate at all") without a real biometric prompt.
//
//  Policy choice — `.deviceOwnerAuthentication` (deliberate): this policy
//  evaluates biometrics first and falls back to the device passcode if
//  biometrics are unavailable, not enrolled, or fail enough times. We choose
//  it over `.deviceOwnerAuthenticationWithBiometrics` specifically so a user
//  who hasn't enrolled Face ID (or whose Face ID is temporarily unusable,
//  e.g. a mask) is never locked out of their own app — the passcode fallback
//  is the escape hatch.
//
//  Fail-open (v1 decision, deliberate): if the device has **no passcode at
//  all** set, `canEvaluatePolicy` fails for every policy — there is then no
//  authentication method the user could ever satisfy. Bricking the app in
//  that state would be strictly worse than the (rare — this is a
//  passcode-less device) risk of skipping the gate, so `unlock()` logs a
//  warning and unlocks anyway. This mirrors how most consumer apps treat a
//  passcode-less device.
//

import Foundation
import Observation
import LocalAuthentication

/// Persistence seam for the Face-ID app-open toggle. Mirrors
/// `PairingStateProviding`'s shape.
protocol AppLockSettingsProviding {
    func loadIsLockEnabled() -> Bool
    func saveIsLockEnabled(_ isEnabled: Bool)
}

/// `UserDefaults`-backed implementation. The Settings feature task's "Require
/// Face ID to open" toggle is expected to bind directly to
/// `AppLockController.isLockEnabled`, which persists through this.
struct UserDefaultsAppLockSettingsProvider: AppLockSettingsProviding {
    private let defaults: UserDefaults
    private let key = "agency.neworange.flightdeck.remote.isLockEnabled"

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    func loadIsLockEnabled() -> Bool {
        defaults.bool(forKey: key)
    }

    func saveIsLockEnabled(_ isEnabled: Bool) {
        defaults.set(isEnabled, forKey: key)
    }
}

/// Abstraction over `LAContext` so tests can exercise every outcome
/// (success, failure, "no authentication method available") without a real
/// biometric prompt.
protocol BiometricAuthenticating {
    /// Whether `policy` can be evaluated right now, mirroring
    /// `LAContext.canEvaluatePolicy(_:error:)`. `error` carries the reason
    /// when `canEvaluate` is `false` (e.g. no biometrics AND no passcode).
    func canEvaluate(policy: LAPolicy) -> (canEvaluate: Bool, error: Error?)

    /// Evaluates `policy`, mirroring
    /// `LAContext.evaluatePolicy(_:localizedReason:reply:)`.
    func evaluate(policy: LAPolicy, reason: String) async -> Result<Void, Error>
}

/// Real `LAContext`-backed implementation. A fresh `LAContext` is used per
/// call (contexts are cheap and not meant to be reused across unrelated
/// evaluations).
struct LAContextBiometricAuthenticator: BiometricAuthenticating {
    func canEvaluate(policy: LAPolicy) -> (canEvaluate: Bool, error: Error?) {
        let context = LAContext()
        var error: NSError?
        let can = context.canEvaluatePolicy(policy, error: &error)
        return (can, error)
    }

    func evaluate(policy: LAPolicy, reason: String) async -> Result<Void, Error> {
        let context = LAContext()
        return await withCheckedContinuation { continuation in
            context.evaluatePolicy(policy, localizedReason: reason) { success, error in
                if success {
                    continuation.resume(returning: .success(()))
                } else {
                    continuation.resume(returning: .failure(error ?? LAError(.authenticationFailed)))
                }
            }
        }
    }
}

#if DEBUG
/// DEBUG-only mock used by the `-uitest-enable-applock` launch-argument hook
/// (see `AppLockController.init`): always reports it can evaluate, and
/// always succeeds instantly. There is no real biometric sensor in the
/// simulator/CI, so UI tests drive the "Unlock with Face ID" button and
/// expect it to deterministically succeed.
struct AlwaysSucceedingBiometricAuthenticator: BiometricAuthenticating {
    func canEvaluate(policy: LAPolicy) -> (canEvaluate: Bool, error: Error?) {
        (true, nil)
    }

    func evaluate(policy: LAPolicy, reason: String) async -> Result<Void, Error> {
        .success(())
    }
}
#endif

/// What the lock overlay should currently show.
enum AppLockState: Equatable {
    /// Underlying app content is visible; no overlay.
    case unlocked
    /// The overlay is up, awaiting a successful `unlock()`.
    case locked
    /// A biometric/passcode prompt is in flight.
    case authenticating
    /// The last attempt failed; `String` is a short user-facing message.
    case failed(String)
}

/// Owns the Face-ID app-open gate: whether it's enabled, its persistence,
/// and the lock/unlock state machine. `RootView` observes `lockState` and
/// `isLockEnabled` to decide whether to show `AppLockView` over everything
/// else, and calls `lockIfEnabled()` on the scenePhase→background
/// transition.
@Observable
final class AppLockController {
    private let settings: AppLockSettingsProviding
    private let authenticator: BiometricAuthenticating

    /// Whether "Require Face ID to open" is on. Persisted via `settings`.
    /// Turning this off immediately reveals the app (there is nothing left
    /// to gate).
    var isLockEnabled: Bool {
        didSet {
            guard isLockEnabled != oldValue else { return }
            settings.saveIsLockEnabled(isLockEnabled)
            if !isLockEnabled {
                lockState = .unlocked
            }
        }
    }

    /// Current overlay state. External code should not set this directly —
    /// use `lockIfEnabled()` / `unlock()` / `autoUnlockIfNeeded()`.
    private(set) var lockState: AppLockState

    /// Whether the automatic "attempt unlock once" has already fired for the
    /// current lock episode (since app launch or the last `lockIfEnabled()`
    /// call). Reset whenever the app (re-)locks.
    private var hasAutoAttemptedThisLock = false

    /// The policy used everywhere: biometrics first, device passcode as
    /// fallback (see file-level doc comment for the rationale).
    private static let policy: LAPolicy = .deviceOwnerAuthentication

    init(
        settings: AppLockSettingsProviding = UserDefaultsAppLockSettingsProvider(),
        authenticator: BiometricAuthenticating = LAContextBiometricAuthenticator()
    ) {
        self.settings = settings

        var isLockEnabledResolved = settings.loadIsLockEnabled()
        var resolvedAuthenticator = authenticator
        var suppressInitialAutoUnlock = false

        #if DEBUG
        // UI-test hook: `-uitest-reset-applock` clears the persisted
        // "Require Face ID to open" toggle at launch, mirroring
        // `-uitest-reset-pairing` (`PairingStore`) — so a Settings UI test
        // that verifies the toggle persists across relaunch always starts
        // from a known disabled state, regardless of what an earlier test
        // run (or a previous launch in the same test) left behind.
        if ProcessInfo.processInfo.arguments.contains("-uitest-reset-applock") {
            isLockEnabledResolved = false
            settings.saveIsLockEnabled(false)
        }

        // UI-test hook: `-uitest-mock-biometrics` swaps in the
        // always-succeeding mock WITHOUT forcing the gate on — for the
        // Settings UI test that verifies the persisted toggle itself
        // (`-uitest-enable-applock` would mask persistence by forcing
        // `isLockEnabled = true`). Whether the lock screen appears at all is
        // then decided purely by the persisted toggle; the initial
        // auto-attempt is suppressed for the same determinism reason as
        // `-uitest-enable-applock` below.
        if ProcessInfo.processInfo.arguments.contains("-uitest-mock-biometrics") {
            resolvedAuthenticator = AlwaysSucceedingBiometricAuthenticator()
            suppressInitialAutoUnlock = true
        }

        // UI-test hook: `-uitest-enable-applock` forces the gate on and
        // swaps in a mock that always succeeds, so tests can drive the lock
        // screen deterministically without a real Face ID prompt. The
        // automatic "attempt unlock once" (below) is suppressed in this mode
        // — otherwise it would race the test's assertion that the lock
        // screen is visible, since the mock succeeds effectively instantly.
        // The test instead drives the retry button explicitly via
        // `unlock()`, which is never suppressed.
        if ProcessInfo.processInfo.arguments.contains("-uitest-enable-applock") {
            isLockEnabledResolved = true
            resolvedAuthenticator = AlwaysSucceedingBiometricAuthenticator()
            suppressInitialAutoUnlock = true
        }
        #endif

        self.authenticator = resolvedAuthenticator
        self.isLockEnabled = isLockEnabledResolved
        self.lockState = isLockEnabledResolved ? .locked : .unlocked
        self.hasAutoAttemptedThisLock = suppressInitialAutoUnlock
    }

    /// Arms the lock if the gate is enabled. Intended to be called from the
    /// scenePhase → `.background` transition (PRD §9): the app shows the
    /// lock overlay the next time it's viewed. No-op if the gate is
    /// disabled, or if already locked (idempotent).
    func lockIfEnabled() {
        guard isLockEnabled else { return }
        guard lockState != .locked else { return }
        lockState = .locked
        hasAutoAttemptedThisLock = false
    }

    /// Attempts the automatic "unlock once" on first appearance of the lock
    /// screen for this lock episode (app launch, or the first appearance
    /// after `lockIfEnabled()`). Subsequent appearances (e.g. SwiftUI
    /// re-renders) do not re-trigger biometrics automatically — only a
    /// manual `unlock()` call (the retry button) does.
    @MainActor
    func autoUnlockIfNeeded() async {
        guard isLockEnabled, lockState == .locked, !hasAutoAttemptedThisLock else { return }
        hasAutoAttemptedThisLock = true
        await unlock()
    }

    /// Runs one biometric/passcode evaluation and updates `lockState` with
    /// the outcome. Safe to call repeatedly (e.g. the retry button); ignored
    /// while an evaluation is already in flight.
    @MainActor
    func unlock() async {
        guard isLockEnabled else {
            lockState = .unlocked
            return
        }
        guard lockState != .authenticating else { return }

        lockState = .authenticating

        let capability = authenticator.canEvaluate(policy: Self.policy)
        guard capability.canEvaluate else {
            // Fail-open: see file-level doc comment. There is no
            // authentication method left for the user to satisfy, so we
            // never brick the app — log loudly and let them in.
            print(
                "AppLockController: WARNING fail-open — no authentication "
                + "method available (\(String(describing: capability.error))); "
                + "unlocking without authentication."
            )
            lockState = .unlocked
            return
        }

        let result = await authenticator.evaluate(
            policy: Self.policy,
            reason: "Unlock FlightDeck Remote"
        )
        switch result {
        case .success:
            lockState = .unlocked
        case .failure(let error):
            lockState = .failed(Self.message(for: error))
        }
    }

    private static func message(for error: Error) -> String {
        if let laError = error as? LAError {
            switch laError.code {
            case .userCancel, .systemCancel, .appCancel, .userFallback:
                return "Authentication canceled."
            case .biometryNotAvailable, .biometryNotEnrolled, .biometryLockout:
                return "Face ID unavailable. Try again or use your passcode."
            default:
                return "Authentication failed. Try again."
            }
        }
        return "Authentication failed. Try again."
    }
}
