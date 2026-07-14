//
//  PairingView.swift
//  FlightDeckRemote
//
//  PRD §5.6 pairing flow: "Pair with your Mac — In FlightDeck on desktop,
//  open Settings → Remote and scan this, or enter the code." A 4-digit
//  segmented code entry (e.g. 4 7 2 9) is the primary path; "Scan QR
//  instead" (QRScannerView) is the alternative. Both redeem a claim token
//  via `PairingServicing` (PairingService.swift) and, on success, call
//  `PairingStore.completePairing(with:)` — `AppRouter`/`RootView` then swap
//  this screen for the main tab container automatically (PRD §5.8), so this
//  view never navigates anywhere on success.
//
//  Carries a DEBUG-only "Toggle Paired" button (navigation task) so the
//  paired/unpaired boundary stays manually testable in the simulator, and
//  so existing UI tests (NavigationUITests) keep working unmodified.
//

import SwiftUI

struct PairingView: View {
    var pairingStore: PairingStore
    // Chosen at the composition root: the deterministic mock under UI tests /
    // in DEBUG, the real relay-backed service otherwise (PairingServiceFactory).
    var service: PairingServicing = PairingServiceFactory.makeDefault()

    @State private var code: String = ""
    @State private var isLoading = false
    @State private var errorMessage: String?
    @State private var isPresentingScanner = false
    @State private var shakeTrigger: CGFloat = 0
    @FocusState private var codeFieldFocused: Bool

    private var isCodeComplete: Bool { code.count == 4 }

    // The root is a plain VStack with `.accessibilityElement(children:
    // .contain)` — the same shape as the original placeholder — so the
    // "PairingView" identifier keeps mapping to an `XCUIElementTypeOther`;
    // the pre-existing smoke test queries it via
    // `app.otherElements["PairingView"]`. (Applying the identifier to a
    // ZStack-over-ScrollView root broke that mapping.)
    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                VStack(spacing: Theme.Spacing.xl) {
                    header

                    codeEntrySection
                        .padding(.top, Theme.Spacing.sm)
                        .modifier(ShakeEffect(animatableData: shakeTrigger))

                    if let errorMessage {
                        Text(errorMessage)
                            .typography(Typography.callout)
                            .foregroundStyle(Theme.statusRed)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, Theme.Spacing.xxl)
                            .accessibilityIdentifier("pairing-error-text")
                    }

                    pairButton

                    dividerRow

                    scanQRButton

                    Spacer(minLength: Theme.Spacing.xxxl)

                    footer

                    #if DEBUG
                    Button("Debug: Toggle Paired") {
                        pairingStore.debugTogglePaired()
                    }
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.statusCyan)
                    .accessibilityIdentifier("debug-toggle-paired-button")
                    #endif
                }
                .padding(.horizontal, Theme.Spacing.xl)
                .padding(.vertical, Theme.Spacing.xxxl)
                .frame(maxWidth: .infinity)
                // The "PairingView" identifier lives on this inner VStack —
                // not the ScrollView/root — because an identifier applied to
                // a scroll-view root surfaces as XCUIElementTypeScrollView,
                // and the pre-existing smoke test queries
                // `app.otherElements["PairingView"]` (Other). A plain
                // contained VStack maps to Other; all the screen's testable
                // controls are its descendants.
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("PairingView")
            }
            .scrollDismissesKeyboard(.interactively)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep.ignoresSafeArea())
        .fullScreenCover(isPresented: $isPresentingScanner) {
            QRScannerView(
                onPayload: { payload in
                    isPresentingScanner = false
                    Task { await pair(with: .qr(payload)) }
                },
                onEnterCodeInstead: {
                    isPresentingScanner = false
                },
                onCancel: {
                    isPresentingScanner = false
                }
            )
        }
    }

    // MARK: - Sections

    private var header: some View {
        VStack(spacing: Theme.Spacing.lg) {
            Image(systemName: "antenna.radiowaves.left.and.right")
                .font(.system(size: 40))
                .foregroundStyle(Theme.accent)

            Text("Pair with your Mac")
                .typography(Typography.largeTitle)
                .foregroundStyle(Theme.textPrimary)
                .multilineTextAlignment(.center)
                .accessibilityIdentifier("pairing-title")

            Text("In FlightDeck on desktop, open Settings → Remote and scan this, or enter the code.")
                .typography(Typography.body)
                .foregroundStyle(Theme.textMuted)
                .multilineTextAlignment(.center)
                .padding(.horizontal, Theme.Spacing.xxl)
        }
    }

    /// Four decorative digit boxes over one real (visually hidden) numeric
    /// `TextField`. Using a single field — rather than four separately
    /// focused fields with manual auto-advance wiring — gets auto-advance,
    /// the numeric keypad, paste (long-press → Paste, or AutoFill from a
    /// code delivered elsewhere), and backspace-across-boxes all "for free"
    /// from the system text field, and stays trivially exercisable from
    /// `XCUITest.typeText`.
    private var codeEntrySection: some View {
        ZStack {
            TextField("", text: $code)
                .keyboardType(.numberPad)
                .textContentType(.oneTimeCode)
                .foregroundStyle(.clear)
                .tint(.clear)
                .frame(width: 244, height: 64)
                .disabled(isLoading)
                .focused($codeFieldFocused)
                .accessibilityLabel("4-digit pairing code")
                .accessibilityIdentifier("code-entry-field")
                .onChange(of: code) { _, newValue in
                    sanitizeCode(newValue)
                }
                .onSubmit {
                    if isCodeComplete { Task { await pair(with: .code(code, relayURL: PairingDefaults.relayURL)) } }
                }

            HStack(spacing: Theme.Spacing.md) {
                ForEach(0..<4, id: \.self) { index in
                    codeDigitBox(at: index)
                }
            }
            .allowsHitTesting(false)
        }
        .contentShape(Rectangle())
        .onTapGesture { codeFieldFocused = true }
    }

    private func codeDigitBox(at index: Int) -> some View {
        let characters = Array(code)
        let hasDigit = index < characters.count
        let isNextToFill = codeFieldFocused && index == characters.count

        return Text(hasDigit ? String(characters[index]) : "")
            .typography(Typography.largeTitle)
            .foregroundStyle(Theme.textPrimary)
            .frame(width: 52, height: 64)
            .background(Theme.bgField, in: RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                    .strokeBorder(
                        isNextToFill ? Theme.accent : Theme.text.opacity(0.12),
                        lineWidth: isNextToFill ? 2 : 1
                    )
            )
            // An *empty* Text produces no accessibility element at all, so
            // the identifier would vanish whenever the box has no digit yet
            // (UI tests assert the four boxes exist on a fresh screen).
            // Force each box to always be an element of its own.
            .accessibilityElement(children: .ignore)
            .accessibilityIdentifier("code-digit-box-\(index)")
    }

    private var pairButton: some View {
        Button {
            Task { await pair(with: .code(code, relayURL: PairingDefaults.relayURL)) }
        } label: {
            ZStack {
                Text("Pair")
                    .typography(Typography.headline)
                    .opacity(isLoading ? 0 : 1)
                if isLoading {
                    ProgressView()
                        .tint(Theme.bgDeep)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, Theme.Spacing.md)
        }
        .buttonStyle(.plain)
        .foregroundStyle(Theme.bgDeep)
        .background(
            (isCodeComplete && !isLoading) ? Theme.accent : Theme.accent.opacity(0.35),
            in: RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
        )
        .disabled(!isCodeComplete || isLoading)
        .accessibilityIdentifier("pair-button")
    }

    private var dividerRow: some View {
        HStack(spacing: Theme.Spacing.md) {
            Rectangle().fill(Theme.text.opacity(0.12)).frame(height: 1)
            Text("OR")
                .typography(Typography.captionBold)
                .foregroundStyle(Theme.textMutedDark)
            Rectangle().fill(Theme.text.opacity(0.12)).frame(height: 1)
        }
    }

    private var scanQRButton: some View {
        Button {
            errorMessage = nil
            isPresentingScanner = true
        } label: {
            Label("Scan QR instead", systemImage: "qrcode.viewfinder")
                .typography(Typography.bodyMedium)
        }
        .foregroundStyle(Theme.textPrimary)
        .disabled(isLoading)
        .accessibilityIdentifier("scan-qr-button")
    }

    private var footer: some View {
        HStack(spacing: Theme.Spacing.xs) {
            Image(systemName: "lock.fill")
                .font(.system(size: 12))
            Text("End-to-end encrypted · unlocked by Face ID")
                .typography(Typography.caption)
        }
        .foregroundStyle(Theme.textMutedDark)
        .accessibilityIdentifier("pairing-footer")
    }

    // MARK: - Behavior

    private func sanitizeCode(_ newValue: String) {
        let digitsOnly = newValue.filter(\.isNumber)
        let capped = String(digitsOnly.prefix(4))
        if capped != code {
            code = capped
        }
        if errorMessage != nil {
            errorMessage = nil
        }
    }

    @MainActor
    private func pair(with input: PairingInput) async {
        errorMessage = nil
        isLoading = true
        defer { isLoading = false }

        do {
            let device = try await service.pair(with: input)
            pairingStore.completePairing(with: device)
            // No further UI work: RootView swaps this screen out the
            // instant `pairingStore.isPaired` flips to true.
        } catch let error as PairingError {
            errorMessage = error.errorDescription
            triggerShake(for: input)
        } catch {
            errorMessage = PairingError.unknown(error.localizedDescription).errorDescription
            triggerShake(for: input)
        }
    }

    private func triggerShake(for input: PairingInput) {
        guard case .code = input else { return }
        withAnimation(.linear(duration: 0.35)) {
            shakeTrigger += 1
        }
    }
}

/// Small horizontal-shake `GeometryEffect` for the bad-code error state.
private struct ShakeEffect: GeometryEffect {
    var animatableData: CGFloat
    var amount: CGFloat = 8
    var shakesPerUnit: CGFloat = 3

    func effectValue(size: CGSize) -> ProjectionTransform {
        let translation = amount * sin(animatableData * .pi * shakesPerUnit)
        return ProjectionTransform(CGAffineTransform(translationX: translation, y: 0))
    }
}

#Preview {
    PairingView(pairingStore: PairingStore())
}
