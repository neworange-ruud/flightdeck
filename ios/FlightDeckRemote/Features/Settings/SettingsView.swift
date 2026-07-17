//
//  SettingsView.swift
//  FlightDeckRemote
//
//  PRD §5.6 Settings: connected device + honest connection state, the
//  Face-ID app-open gate, unpairing, notification prefs (placeholder — a
//  separate push-dependent task replaces the row below), and the app
//  version.
//
//  Dependencies (all consumed, never rebuilt here):
//   - `TransportStore.linkState`/`latencyMs` via `ConnectionIndicator`
//     (`.full` size) — Features/Connection/ConnectionIndicator.swift.
//   - `PairingStore.pairedDevice` for the connected-device name/paired-at
//     when this launch completed pairing itself; falls back to the
//     Keychain-persisted `PairingRecordStore` (pairing id + paired-at
//     survive relaunch even though `PairedDevice.peerName` currently does
//     not — see `PairingStore`'s doc comment) with a "Paired Mac" name
//     placeholder, and finally to a bare "Paired Mac" placeholder if
//     neither is available (e.g. the DEBUG pairing toggle, which sets
//     neither).
//   - `AppLockController.isLockEnabled`, lifted into the environment by
//     `RootView` (`.environment(appLock)`) — see that file's doc comment.
//   - `LAContextBiometricAuthenticator().canEvaluate` (injectable as
//     `biometricAuthenticator`) to annotate the Face-ID toggle when no
//     device authentication method is available at all.
//   - `SettingsUnpairing` (SettingsUnpairService.swift) for the unpair
//     sequence.
//

import SwiftUI
import LocalAuthentication

/// Pure presentation logic for the "Require Face ID to open" row, factored
/// out so it's unit-testable without instantiating the view (mirrors
/// `ConnectionLatencyPhrase` in ConnectionIndicator.swift).
struct FaceIDRowPresentation: Equatable {
    static let unavailableFootnote = "No device authentication available"

    /// Whether the toggle itself should be interactive.
    let isToggleEnabled: Bool
    /// The footnote to show under the toggle, or `nil` when biometrics/passcode
    /// are available and no annotation is needed.
    let footnote: String?

    static func make(canEvaluateBiometrics: Bool) -> FaceIDRowPresentation {
        FaceIDRowPresentation(
            isToggleEnabled: canEvaluateBiometrics,
            footnote: canEvaluateBiometrics ? nil : unavailableFootnote
        )
    }
}

struct SettingsView: View {
    var router: AppRouter
    var transportStore: TransportStore
    var pairingRecordStore: PairingRecordStore
    var biometricAuthenticator: BiometricAuthenticating
    var notificationPreferences: NotificationPreferences
    private let unpairService: SettingsUnpairing

    @Environment(AppLockController.self) private var appLock

    @State private var deviceName = "Paired Mac"
    @State private var pairedAt: Date?
    @State private var canEvaluateBiometrics = true
    @State private var isPresentingUnpairConfirmation = false

    init(
        router: AppRouter,
        transportStore: TransportStore,
        pairingRecordStore: PairingRecordStore = PairingRecordStore(),
        biometricAuthenticator: BiometricAuthenticating = LAContextBiometricAuthenticator(),
        notificationPreferences: NotificationPreferences,
        unpairService: SettingsUnpairing? = nil
    ) {
        self.router = router
        self.transportStore = transportStore
        self.pairingRecordStore = pairingRecordStore
        self.biometricAuthenticator = biometricAuthenticator
        self.notificationPreferences = notificationPreferences
        self.unpairService = unpairService
            ?? DefaultSettingsUnpairService(transportStore: transportStore, pairingRecordStore: pairingRecordStore)
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.Spacing.xxl) {
                connectionSection
                securitySection
                notificationsSection
                aboutSection
            }
            .padding(Theme.Spacing.xl)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep.ignoresSafeArea())
        .onAppear(perform: loadDeviceInfo)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("SettingsView")
    }

    // MARK: - Connection

    private var connectionSection: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            sectionHeader("Connection")

            VStack(alignment: .leading, spacing: Theme.Spacing.md) {
                Text(deviceName)
                    .typography(Typography.headline)
                    .foregroundStyle(Theme.textPrimary)
                    .accessibilityIdentifier("settings-device-name")

                ConnectionIndicator(linkState: transportStore.linkState, size: .full)

                if let pairedAt {
                    Text("Paired \(Self.pairedAtFormatter.string(from: pairedAt))")
                        .typography(Typography.caption)
                        .foregroundStyle(Theme.textMutedDark)
                        .accessibilityIdentifier("settings-paired-since")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(Theme.Spacing.lg)
            .cardStyle()
            // `.contain` first: an identifier applied to a plain container
            // propagates onto every accessibility element inside it,
            // clobbering the children's own identifiers (same trap MainTabView
            // documents). Making the card a container element scopes the
            // identifier to the card itself.
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("settings-connection-card")
        }
    }

    // MARK: - Security

    private var securitySection: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            sectionHeader("Security")

            VStack(alignment: .leading, spacing: 0) {
                faceIDRow

                Rectangle()
                    .fill(Theme.text.opacity(0.08))
                    .frame(height: 1)

                unpairRow
            }
            .cardStyle()
            // `.contain` first — see the connection card's comment.
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("settings-security-card")
        }
    }

    @ViewBuilder
    private var faceIDRow: some View {
        @Bindable var appLock = appLock
        let presentation = FaceIDRowPresentation.make(canEvaluateBiometrics: canEvaluateBiometrics)

        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            Toggle(isOn: $appLock.isLockEnabled) {
                Text("Require Face ID to open")
                    .typography(Typography.body)
                    .foregroundStyle(presentation.isToggleEnabled ? Theme.textPrimary : Theme.textMutedDark)
            }
            .tint(Theme.accent)
            .disabled(!presentation.isToggleEnabled)
            .accessibilityIdentifier("settings-faceid-toggle")

            if let footnote = presentation.footnote {
                Text(footnote)
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textMutedDark)
                    .accessibilityIdentifier("settings-faceid-footnote")
            }
        }
        .padding(Theme.Spacing.lg)
    }

    private var unpairRow: some View {
        Button(role: .destructive) {
            isPresentingUnpairConfirmation = true
        } label: {
            Text("Unpair this device")
                .typography(Typography.body)
                .foregroundStyle(Theme.statusRed)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(Theme.Spacing.lg)
        .accessibilityIdentifier("settings-unpair-button")
        .confirmationDialog(
            "Unpair this device?",
            isPresented: $isPresentingUnpairConfirmation,
            titleVisibility: .visible
        ) {
            Button("Unpair", role: .destructive) {
                Task { await performUnpair() }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("You'll need to pair again from FlightDeck on your Mac.")
        }
    }

    // MARK: - Notifications (PRD §5.6/§9.2)

    @ViewBuilder
    private var notificationsSection: some View {
        @Bindable var prefs = notificationPreferences

        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            sectionHeader("Notifications")

            // Three INDEPENDENT global toggles (PRD §5.6).
            VStack(alignment: .leading, spacing: 0) {
                notificationToggle(
                    "Agent needs input",
                    isOn: $prefs.agentNeedsInput,
                    identifier: "settings-notif-needsinput")
                rowDivider
                notificationToggle(
                    "Agent finished",
                    isOn: $prefs.agentFinished,
                    identifier: "settings-notif-finished")
                rowDivider
                notificationToggle(
                    "Completion chime",
                    isOn: $prefs.completionChime,
                    identifier: "settings-notif-chime")
            }
            .cardStyle()
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("settings-notifications-card")

            mutedProjectsCard
        }
    }

    /// Per-project mute (PRD §9.2). Shown only when we know the projects (from
    /// the live/cached snapshot); otherwise an honest note.
    @ViewBuilder
    private var mutedProjectsCard: some View {
        let projects = transportStore.snapshot?.projects ?? []

        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            sectionHeader("Mute by project")

            if projects.isEmpty {
                Text("No projects yet — mute is available once your Mac is connected.")
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textMutedDark)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(Theme.Spacing.lg)
                    .cardStyle()
                    .accessibilityIdentifier("settings-notif-mute-empty")
            } else {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(projects.enumerated()), id: \.element.projectId) { index, project in
                        if index != 0 { rowDivider }
                        muteRow(for: project)
                    }
                }
                .cardStyle()
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("settings-notif-mute-card")
            }
        }
    }

    private func muteRow(for project: Wire.ProjectState) -> some View {
        let projectId = project.projectId.rawValue
        // A muted project suppresses its notifications; the toggle reads "muted",
        // so it is ON when notifications are OFF for the project.
        let binding = Binding(
            get: { notificationPreferences.isMuted(projectId: projectId) },
            set: { notificationPreferences.setMuted($0, projectId: projectId) })

        return Toggle(isOn: binding) {
            Text(project.name)
                .typography(Typography.body)
                .foregroundStyle(Theme.textPrimary)
        }
        .tint(Theme.accent)
        .padding(Theme.Spacing.lg)
        .accessibilityIdentifier("settings-notif-mute-\(projectId)")
    }

    private func notificationToggle(
        _ title: String,
        isOn: Binding<Bool>,
        identifier: String
    ) -> some View {
        Toggle(isOn: isOn) {
            Text(title)
                .typography(Typography.body)
                .foregroundStyle(Theme.textPrimary)
        }
        .tint(Theme.accent)
        .padding(Theme.Spacing.lg)
        .accessibilityIdentifier(identifier)
    }

    private var rowDivider: some View {
        Rectangle()
            .fill(Theme.text.opacity(0.08))
            .frame(height: 1)
    }

    // MARK: - About

    private var aboutSection: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            sectionHeader("About")

            HStack {
                Text("Version")
                    .typography(Typography.body)
                    .foregroundStyle(Theme.textPrimary)
                Spacer()
                Text(Self.appVersionString)
                    .typography(Typography.body)
                    .foregroundStyle(Theme.textMuted)
            }
            .padding(Theme.Spacing.lg)
            .cardStyle()
            // `.contain` first — see the connection card's comment.
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("settings-about-card")
        }
    }

    // MARK: - Shared

    private func sectionHeader(_ title: String) -> some View {
        Text(title.uppercased())
            .typography(Typography.captionBold)
            .foregroundStyle(Theme.textMutedDark)
            .padding(.horizontal, Theme.Spacing.xs)
    }

    // MARK: - Behavior

    private func loadDeviceInfo() {
        if let device = router.pairingStore.pairedDevice {
            deviceName = device.peerName
            pairedAt = device.pairedAt
        } else if let record = try? pairingRecordStore.load() {
            deviceName = "Paired Mac"
            pairedAt = record.pairedAt
        } else {
            deviceName = "Paired Mac"
            pairedAt = nil
        }
        canEvaluateBiometrics = biometricAuthenticator.canEvaluate(policy: .deviceOwnerAuthentication).canEvaluate
    }

    @MainActor
    private func performUnpair() async {
        await SettingsUnpairCoordinator.run(service: unpairService, pairingStore: router.pairingStore)
    }

    private static let pairedAtFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .none
        return formatter
    }()

    private static var appVersionString: String {
        let info = Bundle.main.infoDictionary
        let version = info?["CFBundleShortVersionString"] as? String ?? "1.0"
        let build = info?["CFBundleVersion"] as? String ?? "1"
        return "\(version) (\(build))"
    }
}

#Preview {
    let router = AppRouter(pairingStore: PairingStore())
    router.pairingStore.completePairing(
        with: PairedDevice(pairingId: "preview-pairing", peerName: "Ruud's MacBook Pro", pairedAt: Date())
    )
    let transportStore = TransportStoreFactory.makeDefault()
    transportStore.debugSeed(snapshot: .uiTestFixture, linkState: .connected(latencyMs: 42))

    return SettingsView(
        router: router,
        transportStore: transportStore,
        notificationPreferences: NotificationPreferences())
        .environment(AppLockController())
}
