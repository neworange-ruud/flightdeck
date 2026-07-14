//
//  ShellView.swift
//  FlightDeckRemote
//
//  The terminal surface for one session (PRD §5.4 v1 minimal terminal). It is
//  embeddable: the Shell TAB (`ShellTabView`) mounts it after a session pick,
//  and the Chat `Agent · Shell` switcher mounts it for the current session —
//  both feed it the same live `TransportStore`.
//
//  Phases (driven by `ShellSessionModel` / `ShellStateMachine`):
//   * no shell / closed → "Open shell in <worktree>" CTA (fits cols/rows, then
//     `shell_open`);
//   * opening → spinner;
//   * live → the SwiftTerm renderer + a Copy/Close toolbar + our `ShellKeyBar`;
//   * exited(code) → the frozen output + a "process exited (code N)" banner
//     with Close / Reopen;
//   * rejectedAlreadyOpen → an honest message + Close existing / Reopen.
//
//  Connection honesty (PRD §8): while the link isn't live, a paused note shows
//  and the key bar + open CTA are disabled — nothing is sent blind.
//

import SwiftUI

struct ShellView: View {
    let sessionId: Wire.SessionId
    let sessionName: String
    var store: TransportStore?

    @State private var model: ShellSessionModel
    @State private var controller = ShellTerminalController()
    @State private var didConfigure = false

    #if DEBUG
    @State private var scriptedSender = ScriptedShellCommandSender()
    #endif

    init(sessionId: Wire.SessionId, sessionName: String, store: TransportStore? = nil) {
        self.sessionId = sessionId
        self.sessionName = sessionName
        self.store = store
        _model = State(wrappedValue: ShellSessionModel(sessionId: sessionId, sessionName: sessionName))
    }

    var body: some View {
        // Read observable dependencies in the tracked top scope so Observation
        // fires our ingest `onChange`s (see ShellSessionModel's doc comment).
        let phase = model.phase
        let paused = model.commandsPaused
        let eventCount = store?.shellEvents[sessionId]?.count ?? 0
        let outputCount = model.shellId.flatMap { store?.shellOutput[$0]?.count } ?? 0
        let delivery = model.openHandleDelivery

        return VStack(spacing: 0) {
            if paused {
                pausedNote
            }
            content(phase: phase, paused: paused)
            if case .live = phase {
                ShellKeyBar(ctrlArmed: model.ctrlArmed, disabled: paused) { key in
                    model.tapKey(key)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep)
        .onAppear { configureIfNeeded() }
        .onChange(of: eventCount) { _, _ in model.ingestStoreEvents() }
        .onChange(of: outputCount) { _, _ in model.ingestStoreOutput() }
        .onChange(of: delivery) { _, _ in model.reconcileOpenDelivery() }
        #if DEBUG
        .background(debugSentLabel)
        #endif
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("ShellView")
    }

    // MARK: - Content by phase

    @ViewBuilder
    private func content(phase: ShellPhase, paused: Bool) -> some View {
        switch phase {
        case .noShell, .closed:
            openCTA(paused: paused)
        case .opening:
            openingState
        case .live:
            liveTerminal(exited: false)
        case let .exited(code):
            VStack(spacing: 0) {
                liveTerminal(exited: true)
                exitedBanner(code: code, paused: paused)
            }
        case let .rejectedAlreadyOpen(message):
            rejectedState(message: message, paused: paused)
        }
    }

    private func openCTA(paused: Bool) -> some View {
        VStack(spacing: Theme.Spacing.lg) {
            Image(systemName: "terminal")
                .font(.system(size: 44))
                .foregroundStyle(Theme.textMuted)
            Text("No shell open")
                .typography(Typography.headline)
                .foregroundStyle(Theme.textPrimary)
            Button {
                model.open()
            } label: {
                Text("Open shell in \(sessionName)")
                    .typography(Typography.bodyMedium)
                    .foregroundStyle(Theme.bgDeep)
                    .padding(.horizontal, Theme.Spacing.lg)
                    .padding(.vertical, Theme.Spacing.md)
                    .background(Capsule().fill(Theme.accent))
            }
            .buttonStyle(.plain)
            .disabled(paused)
            .opacity(paused ? 0.5 : 1)
            .accessibilityIdentifier("shell-open-cta")
            if paused {
                Text("Link down — can't open a shell until reconnected.")
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textDim)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(Theme.Spacing.xl)
    }

    private var openingState: some View {
        VStack(spacing: Theme.Spacing.md) {
            ProgressView()
                .tint(Theme.accent)
            Text("Opening shell in \(sessionName)…")
                .typography(Typography.callout)
                .foregroundStyle(Theme.textMuted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityIdentifier("shell-opening")
    }

    private func liveTerminal(exited: Bool) -> some View {
        VStack(spacing: 0) {
            terminalToolbar(exited: exited)
            // The renderer's UIKit view carries the "shell-terminal"
            // accessibility identifier itself (collapsed to one element —
            // see ShellTerminalRenderer.makeUIView).
            ShellTerminalRenderer(model: model, controller: controller)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func terminalToolbar(exited: Bool) -> some View {
        HStack(spacing: Theme.Spacing.md) {
            Button {
                controller.copySelection()
            } label: {
                Label("Copy", systemImage: "doc.on.doc")
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textMuted)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("shell-copy")

            Spacer(minLength: 0)

            if !exited {
                Button(role: .destructive) {
                    model.close()
                } label: {
                    Label("Close", systemImage: "xmark.circle")
                        .typography(Typography.caption)
                        .foregroundStyle(Theme.statusRed)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("shell-close")
            }
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.vertical, Theme.Spacing.sm)
        .background(Theme.bgRaised)
    }

    private func exitedBanner(code: Int32?, paused: Bool) -> some View {
        HStack(spacing: Theme.Spacing.md) {
            Text(exitText(code))
                .typography(Typography.callout)
                .foregroundStyle(Theme.textPrimary)
            Spacer(minLength: 0)
            Button("Close") { model.close() }
                .buttonStyle(.plain)
                .foregroundStyle(Theme.textMuted)
                .accessibilityIdentifier("shell-exit-close")
            Button("Reopen") { model.reopen() }
                .buttonStyle(.plain)
                .foregroundStyle(Theme.accent)
                .disabled(paused)
                .accessibilityIdentifier("shell-exit-reopen")
        }
        .padding(Theme.Spacing.lg)
        .background(Theme.bgField)
        .accessibilityIdentifier("shell-exited-banner")
    }

    private func rejectedState(message: String, paused: Bool) -> some View {
        VStack(spacing: Theme.Spacing.md) {
            Image(systemName: "exclamationmark.triangle")
                .font(.system(size: 36))
                .foregroundStyle(Theme.statusOrange)
            Text(message)
                .typography(Typography.callout)
                .foregroundStyle(Theme.textPrimary)
                .multilineTextAlignment(.center)
            Text("v1 opens one shell per session. Close the existing shell on your desktop, then reopen here.")
                .typography(Typography.caption)
                .foregroundStyle(Theme.textDim)
                .multilineTextAlignment(.center)
            Button("Reopen") { model.reopen() }
                .buttonStyle(.plain)
                .foregroundStyle(Theme.accent)
                .disabled(paused)
                .accessibilityIdentifier("shell-rejected-reopen")
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(Theme.Spacing.xl)
        .accessibilityIdentifier("shell-rejected")
    }

    private var pausedNote: some View {
        Text("Reconnecting — commands are paused. Nothing is sent blind.")
            .typography(Typography.caption)
            .foregroundStyle(Theme.bgDeep)
            .frame(maxWidth: .infinity)
            .padding(.vertical, Theme.Spacing.sm)
            .background(Theme.statusOrange)
            .accessibilityIdentifier("shell-paused-note")
    }

    private func exitText(_ code: Int32?) -> String {
        if let code { return "Process exited (code \(code))" }
        return "Process exited"
    }

    // MARK: - Configuration

    private func configureIfNeeded() {
        guard !didConfigure else { return }
        didConfigure = true
        model.pasteProvider = { UIPasteboard.general.string }

        #if DEBUG
        if ShellDebugSeam.isFixtureShell {
            // No relay: scripted sender records sends (for the seam label) and
            // the forced link-state (via CommandsPausedGate) drives gating.
            // Always drive a live fixture shell so the terminal + key bar
            // render; when the forced link-state is down, the key bar renders
            // disabled and the paused note shows ("input disabled" test).
            model.configure(sender: scriptedSender,
                            gate: CommandsPausedGate(source: ShellFixtureConnectionSource()))
            model.debugDriveFixture(chunks: ShellDebugSeam.fixtureChunks)
            return
        }
        #endif

        if let store { model.configure(store: store) }
    }

    #if DEBUG
    /// Element exposing the scripted sender's last send so UI tests can assert
    /// key-bar / interrupt / paste plumbing without a relay. Rendered in the
    /// background tinted to the deep background (visually invisible) but kept
    /// in the accessibility tree so `XCUITest` can read its label.
    private var debugSentLabel: some View {
        Text(model.debugLastSentDescription)
            .typography(Typography.caption)
            .foregroundStyle(Theme.bgDeep)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .accessibilityIdentifier("shell-debug-last-sent")
    }
    #endif
}

#if DEBUG
/// DEBUG launch-argument seam for the shell surface, mirroring
/// `ConnectionDebugSeam`. `-uitest-fixture-shell` drives a scripted live shell
/// (with an ANSI-coloured line) and a scripted sender, so the Shell UI tests
/// run without a live desktop. Combine with `-uitest-linkstate disconnected`
/// to exercise the paused/disabled state.
enum ShellDebugSeam {
    static var isFixtureShell: Bool {
        ProcessInfo.processInfo.arguments.contains("-uitest-fixture-shell")
    }

    /// Scripted output: a plain line and an ANSI green line, so the terminal
    /// renders coloured text deterministically.
    static let fixtureChunks: [String] = [
        "$ echo hello\r\nhello\r\n",
        "\u{1b}[32mtests passed\u{1b}[0m\r\n"
    ]
}
#endif
