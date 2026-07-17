//
//  AppLockView.swift
//  FlightDeckRemote
//
//  Full-screen lock overlay for the Face-ID app-open gate (PRD §5.6/§9).
//  Rendered by `RootView` on top of *everything* — pairing and the main tab
//  container alike — since either can carry sensitive content (agent
//  transcripts, pairing codes). Never assume which screen is underneath.
//
//  On appear, attempts the one automatic unlock for this lock episode
//  (`AppLockController.autoUnlockIfNeeded()`); the "Unlock with Face ID"
//  button re-attempts on demand (e.g. after a failed/canceled attempt).
//

import SwiftUI

struct AppLockView: View {
    var appLock: AppLockController

    var body: some View {
        VStack(spacing: Theme.Spacing.xxl) {
            Spacer()

            Image(systemName: "lock.shield.fill")
                .font(.system(size: 56))
                .foregroundStyle(Theme.accent)
                .shadow(color: Theme.accent.opacity(0.5), radius: 16)
                .accessibilityHidden(true)

            VStack(spacing: Theme.Spacing.sm) {
                Text("FlightDeck Remote")
                    .typography(Typography.title)
                    .foregroundStyle(Theme.textPrimary)

                statusText
                    .typography(Typography.callout)
                    .multilineTextAlignment(.center)
            }
            .padding(.horizontal, Theme.Spacing.xxl)

            unlockButton

            Spacer()

            Label("Locked · end-to-end encrypted", systemImage: "shield.fill")
                .typography(Typography.caption)
                .foregroundStyle(Theme.textDim)
                .padding(.bottom, Theme.Spacing.xxl)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep.ignoresSafeArea())
        .task {
            await appLock.autoUnlockIfNeeded()
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("AppLockView")
    }

    @ViewBuilder
    private var statusText: some View {
        switch appLock.lockState {
        case .authenticating:
            Text("Authenticating…")
                .foregroundStyle(Theme.textMuted)
        case .failed(let message):
            Text(message)
                .foregroundStyle(Theme.statusOrange)
        case .locked, .unlocked:
            Text("Face ID required to continue")
                .foregroundStyle(Theme.textMuted)
        }
    }

    private var unlockButton: some View {
        Button {
            Task { await appLock.unlock() }
        } label: {
            Label("Unlock with Face ID", systemImage: "faceid")
                .typography(Typography.bodyMedium)
                .foregroundStyle(Theme.bgDeep)
                .padding(.horizontal, Theme.Spacing.xl)
                .padding(.vertical, Theme.Spacing.md)
                .background(Theme.accent, in: Capsule())
        }
        .disabled(appLock.lockState == .authenticating)
        .accessibilityIdentifier("applock-unlock-button")
    }
}

#Preview("Locked") {
    AppLockView(appLock: AppLockController(
        settings: PreviewLockedSettingsProvider(),
        authenticator: LAContextBiometricAuthenticator()
    ))
}

/// Preview-only settings provider that starts locked without touching real
/// `UserDefaults`.
private struct PreviewLockedSettingsProvider: AppLockSettingsProviding {
    func loadIsLockEnabled() -> Bool { true }
    func saveIsLockEnabled(_ isEnabled: Bool) {}
}
