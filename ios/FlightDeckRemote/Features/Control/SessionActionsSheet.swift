//
//  SessionActionsSheet.swift
//  FlightDeckRemote
//
//  The session actions sheet (PRD §5.6): safe actions grouped on top
//  (Restart agent, Open shell [disabled "soon"], Set manual status, Pull
//  base, Merge back), destructive actions apart in red (Close session,
//  Abandon worktree) — never mixed. Every state change is deliberate and
//  confirmed (PRD §8): restart/pull/merge/close get standard confirmation
//  dialogs; abandon gets the type-to-confirm screen (`AbandonConfirmView`);
//  manual status gets its label sub-sheet (`ManualStatusSheet`).
//
//  All sends go through one `CommandRunner` (one action in flight at a time),
//  gated by `CommandsPausedGate` (PRD §8: lost link pauses commands loudly),
//  with the outcome rendered honestly at the top of the sheet: spinner →
//  applied ✓ (verbatim ack note, e.g. close-session's "Stopping agent…") /
//  rejected (desktop's exact reason, verbatim) / "not delivered — retry".
//
//  `SessionActionsButton` is the tiny re-appliable entry point the session
//  row and the chat header mount as a one-liner.
//
//  Git status (PRD §5.5): a "Git status" row above the safe group opens the
//  read-only `GitStatusView` (Features/Git) — reads are frictionless, so it
//  stays reachable even while commands are paused. Merge-back's confirmation
//  is "guarded": when the session's latest known git status has uncommitted
//  changes or drift, `GitMergeGuardText` (Features/Git) adds an extra warning
//  line to the standard confirmation copy.
//

import SwiftUI

// MARK: - Entry point (session row / chat header one-liner)

/// An ellipsis button that presents `SessionActionsSheet` for one session.
struct SessionActionsButton: View {
    let sessionId: Wire.SessionId
    let sessionName: String
    var status: Wire.AgentStatus?
    var store: TransportStore?

    @State private var isPresenting = false

    init(sessionId: Wire.SessionId, sessionName: String,
         status: Wire.AgentStatus? = nil, store: TransportStore? = nil) {
        self.sessionId = sessionId
        self.sessionName = sessionName
        self.status = status
        self.store = store
    }

    /// Convenience for the sessions list row.
    init(session: Wire.SessionState, store: TransportStore?) {
        self.init(sessionId: session.sessionId, sessionName: session.name,
                  status: session.status, store: store)
    }

    var body: some View {
        Button {
            isPresenting = true
        } label: {
            Image(systemName: "ellipsis")
                .font(.system(size: 16, weight: .semibold))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 32, height: 32)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("Session actions"))
        .accessibilityIdentifier("session-actions-\(sessionId.rawValue)")
        .sheet(isPresented: $isPresenting) {
            SessionActionsSheet(sessionId: sessionId, sessionName: sessionName,
                                status: status, store: store)
        }
    }
}

// MARK: - Sheet

struct SessionActionsSheet: View {
    let sessionId: Wire.SessionId
    let sessionName: String
    var status: Wire.AgentStatus?

    @Environment(\.dismiss) private var dismiss

    private let store: TransportStore?
    @State private var gate: CommandsPausedGate
    @State private var runner: CommandRunner
    @State private var activeSheet: SubSheet?

    /// Every deliberate flow the sheet can raise, driven through one
    /// `.sheet(item:)`. The standard confirmations are a themed sheet rather
    /// than a system `.confirmationDialog`: an action sheet raised from within
    /// a themed bottom sheet does not present reliably on iOS 26, and a custom
    /// sheet matches the app's dark design system and is deterministically
    /// testable.
    private enum SubSheet: Identifiable {
        case confirm(SessionControlAction)
        case manualStatus
        case abandon
        case shell
        case gitStatus
        var id: String {
            switch self {
            case let .confirm(action): "confirm-\(action.rawValue)"
            case .manualStatus: "manualStatus"
            case .abandon: "abandon"
            case .shell: "shell"
            case .gitStatus: "gitStatus"
            }
        }
    }

    init(sessionId: Wire.SessionId, sessionName: String,
         status: Wire.AgentStatus? = nil, store: TransportStore? = nil) {
        self.sessionId = sessionId
        self.sessionName = sessionName
        self.status = status
        self.store = store

        // No live store (e.g. the chat header before Chat's own store wiring
        // lands): the gate reads a permanently-down fallback source, so every
        // action is honestly disabled — except under the DEBUG
        // `-uitest-linkstate` seam, where a scripted sender keeps the flow
        // observable for UI tests without a relay.
        let source: any ConnectionStatusSource = store ?? ControlFallbackConnectionSource()
        let gate = CommandsPausedGate(source: source)
        let sender: any ControlCommandSending
        if let store {
            sender = store
        } else {
            #if DEBUG
            sender = ScriptedControlCommandSender()
            #else
            sender = UnavailableControlCommandSender()
            #endif
        }
        _gate = State(initialValue: gate)
        _runner = State(initialValue: CommandRunner(sender: sender,
                                                    isPaused: { gate.commandsPaused }))
    }

    private var commandsPaused: Bool { gate.commandsPaused }

    /// The session's latest known git status (frictionless read, PRD §5.5):
    /// the DEBUG fixture seam wins under `-uitest-fixture-git-status` (no
    /// relay in UI tests), else the live store's last `git_status` push.
    private var gitDetail: Wire.GitStatusDetail? {
        #if DEBUG
        if GitDebugSeam.isFixtureGitStatus {
            return GitDebugSeam.fixtureDetail(sessionId: sessionId)
        }
        #endif
        return store?.gitStatus[sessionId]
    }

    private var isManualOverride: Bool {
        if case .manual = status { return true }
        return false
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.Spacing.lg) {
                    if commandsPaused { pausedNote }
                    outcomeRow
                    gitStatusRow
                    actionGroup(title: "Session", actions: SessionControlAction.safeGroup)
                    actionGroup(title: "Destructive", actions: SessionControlAction.destructiveGroup)
                }
                .padding(Theme.Spacing.lg)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep)
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .presentationBackground(Theme.bgDeep)
        .sheet(item: $activeSheet) { sheet in
            switch sheet {
            case let .confirm(action):
                if let conf = action.confirmation(sessionName: sessionName) {
                    let guardNote = action == .mergeBack ? GitMergeGuardText.build(from: gitDetail) : nil
                    ControlConfirmationSheet(confirmation: conf, guardNote: guardNote) {
                        perform(action)
                    }
                }
            case .manualStatus:
                ManualStatusSheet(
                    sessionName: sessionName,
                    currentStatus: status,
                    commandsPaused: commandsPaused,
                    onSet: { label in
                        runner.run(ControlCommands.setManualStatus(sessionId, label: label))
                    },
                    onClear: {
                        runner.run(ControlCommands.clearManualStatus(sessionId))
                    })
            case .abandon:
                AbandonConfirmView(
                    sessionName: sessionName,
                    commandsPaused: commandsPaused,
                    onConfirm: { typedName in
                        runner.run(ControlCommands.abandonWorktree(sessionId, confirmName: typedName))
                    })
            case .shell:
                shellSheet
            case .gitStatus:
                GitStatusView(sessionId: sessionId, sessionName: sessionName, store: store)
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("SessionActionsSheet")
    }

    // MARK: Header

    private var header: some View {
        HStack(spacing: Theme.Spacing.md) {
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text("Session actions")
                    .typography(Typography.headline)
                    .foregroundStyle(Theme.textPrimary)
                Text(sessionName)
                    .typography(Typography.monoSmall)
                    .foregroundStyle(Theme.textMuted)
                    .lineLimit(1)
            }
            Spacer(minLength: Theme.Spacing.sm)
            Button("Done") { dismiss() }
                .typography(Typography.callout)
                .foregroundStyle(Theme.accent)
                .accessibilityIdentifier("session-actions-done")
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.top, Theme.Spacing.xl)
        .padding(.bottom, Theme.Spacing.sm)
    }

    // MARK: Paused / outcome

    private var pausedNote: some View {
        Text("Commands are paused until the link is back. Nothing is sent blind.")
            .typography(Typography.caption)
            .foregroundStyle(Theme.textDim)
            .accessibilityIdentifier("control-paused-label")
    }

    @ViewBuilder
    private var outcomeRow: some View {
        switch runner.phase {
        case .idle:
            EmptyView()
        case .inFlight:
            HStack(spacing: Theme.Spacing.sm) {
                ProgressView()
                    .tint(Theme.textMuted)
                Text(ControlActionPhrasing.inFlightLabel(for: runner.currentBody))
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.textMuted)
            }
            .accessibilityIdentifier("control-outcome-inflight")
        case let .succeeded(detail):
            HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.sm) {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(Theme.statusIdle)
                Text(detail ?? "Applied")
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.textPrimary)
            }
            .accessibilityIdentifier("control-outcome-applied")
        case let .rejected(reason):
            // The desktop's exact reason, verbatim (PRD §5.6 honesty).
            HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.sm) {
                Image(systemName: "slash.circle")
                    .foregroundStyle(Theme.statusWorking)
                Text(reason)
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.statusWorking)
            }
            .accessibilityIdentifier("control-outcome-rejected")
        case let .failed(reason, _):
            HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.sm) {
                Image(systemName: "exclamationmark.triangle")
                    .foregroundStyle(Theme.statusNeedsInput)
                Text("not delivered — \(reason)")
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.statusNeedsInput)
                Spacer(minLength: Theme.Spacing.sm)
                Button("Retry") { runner.retry() }
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.accent)
                    .disabled(commandsPaused)
                    .accessibilityIdentifier("control-retry")
            }
            .accessibilityIdentifier("control-outcome-failed")
        }
    }

    // MARK: Git status (read, frictionless — PRD §5.5/§8)

    /// Always enabled (a read, not a state change) — opens the read-only
    /// `GitStatusView` (Features/Git).
    private var gitStatusRow: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            Text("GIT")
                .typography(Typography.captionBold)
                .foregroundStyle(Theme.textDim)
            Button {
                activeSheet = .gitStatus
            } label: {
                HStack(spacing: Theme.Spacing.md) {
                    Image(systemName: "arrow.triangle.branch")
                        .font(.system(size: 16, weight: .medium))
                        .foregroundStyle(Theme.textMuted)
                        .frame(width: 22)
                    Text("Git status")
                        .typography(Typography.body)
                        .foregroundStyle(Theme.textPrimary)
                    Spacer(minLength: Theme.Spacing.sm)
                    Image(systemName: "chevron.right")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Theme.textDim)
                }
                .padding(.horizontal, Theme.Spacing.lg)
                .padding(.vertical, Theme.Spacing.md)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .background(Theme.bgCard, in: RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
            .accessibilityIdentifier("control-action-git-status")
        }
    }

    // MARK: Action groups

    private func actionGroup(title: String, actions: [SessionControlAction]) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            Text(title.uppercased())
                .typography(Typography.captionBold)
                .foregroundStyle(actions.first?.isDestructive == true
                                 ? Theme.statusWorking.opacity(0.8) : Theme.textDim)
            VStack(spacing: 0) {
                ForEach(Array(actions.enumerated()), id: \.element) { index, action in
                    if index > 0 {
                        Divider().overlay(Theme.bgDeep)
                    }
                    actionRow(action)
                }
            }
            .background(Theme.bgCard, in: RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
        }
    }

    private func actionRow(_ action: SessionControlAction) -> some View {
        let enabled = !commandsPaused
        let tint: Color = action.isDestructive
            ? Theme.statusWorking
            : (enabled ? Theme.textPrimary : Theme.textDim)
        return Button {
            tapped(action)
        } label: {
            HStack(spacing: Theme.Spacing.md) {
                Image(systemName: action.systemImage)
                    .font(.system(size: 16, weight: .medium))
                    .foregroundStyle(action.isDestructive ? Theme.statusWorking : Theme.textMuted)
                    .frame(width: 22)
                Text(labelText(for: action))
                    .typography(Typography.body)
                    .foregroundStyle(tint)
                Spacer(minLength: Theme.Spacing.sm)
                Image(systemName: "chevron.right")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.textDim)
            }
            .padding(.horizontal, Theme.Spacing.lg)
            .padding(.vertical, Theme.Spacing.md)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.55)
        .accessibilityIdentifier(action.accessibilityIdentifier)
    }

    /// "Set manual status" reads "Clear/edit manual status" context via the
    /// sub-sheet; keep the row title stable but hint at an active override.
    private func labelText(for action: SessionControlAction) -> String {
        if action == .setManualStatus, case let .manual(label)? = status {
            return "Manual status — \(label)"
        }
        return action.title
    }

    private func tapped(_ action: SessionControlAction) {
        switch action {
        case .restartAgent, .pullBase, .mergeBack, .closeSession:
            activeSheet = .confirm(action)
        case .setManualStatus:
            activeSheet = .manualStatus
        case .abandonWorktree:
            activeSheet = .abandon
        case .openShell:
            // The terminal surface now exists (PRD §5.4): mount the session's
            // shell. Opening a shell is read-frictionless — no confirmation.
            activeSheet = .shell
        }
    }

    private func perform(_ action: SessionControlAction) {
        switch action {
        case .restartAgent:
            runner.run(ControlCommands.restartAgent(sessionId))
        case .pullBase:
            runner.run(ControlCommands.pullBase(sessionId))
        case .mergeBack:
            runner.run(ControlCommands.mergeBack(sessionId))
        case .closeSession:
            runner.run(ControlCommands.closeSession(sessionId))
        case .openShell, .setManualStatus, .abandonWorktree:
            break // these use their own flow, not the standard confirmation
        }
    }

    // MARK: Shell

    /// The session's shell surface (PRD §5.4), mounted in a sheet with a small
    /// header. `ShellView` owns all its own phases (open CTA / live / exited /
    /// paused) and drives the same live `TransportStore`.
    private var shellSheet: some View {
        VStack(spacing: 0) {
            HStack(spacing: Theme.Spacing.md) {
                VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                    Text("Shell")
                        .typography(Typography.caption)
                        .foregroundStyle(Theme.textMuted)
                    Text(sessionName)
                        .typography(Typography.headline)
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)
                }
                Spacer(minLength: Theme.Spacing.sm)
                Button("Done") { activeSheet = nil }
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.accent)
                    .accessibilityIdentifier("session-shell-done")
            }
            .padding(.horizontal, Theme.Spacing.lg)
            .padding(.top, Theme.Spacing.xl)
            .padding(.bottom, Theme.Spacing.sm)

            ShellView(sessionId: sessionId, sessionName: sessionName, store: store)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep)
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .presentationBackground(Theme.bgDeep)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("SessionShellSheet")
    }
}

// MARK: - Standard confirmation sheet

/// A themed confirm/cancel sheet for the standard control confirmations (PRD
/// §8: title + consequence sentence + confirm/cancel; destructive confirm in
/// red). Used instead of a system `.confirmationDialog`, which does not present
/// reliably when raised from within a themed bottom sheet on iOS 26.
private struct ControlConfirmationSheet: View {
    let confirmation: ControlConfirmation
    /// Merge-back's "guarded" extra warning line (PRD §5.5), built from the
    /// session's latest known git status (`GitMergeGuardText`, Features/Git).
    /// `nil` for every other action, and for merge-back when there's nothing
    /// to warn about.
    var guardNote: String? = nil
    let onConfirm: () -> Void

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.lg) {
            Text(confirmation.title)
                .typography(Typography.title)
                .foregroundStyle(Theme.textPrimary)
                .padding(.top, Theme.Spacing.xl)

            Text(confirmation.message)
                .typography(Typography.body)
                .foregroundStyle(Theme.textMuted)
                .fixedSize(horizontal: false, vertical: true)

            if let guardNote {
                Text(guardNote)
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.statusNeedsInput)
                    .fixedSize(horizontal: false, vertical: true)
                    .accessibilityIdentifier("control-confirm-guard-note")
            }

            confirmButton
            cancelButton

            Spacer(minLength: 0)
        }
        .padding(Theme.Spacing.lg)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bgDeep)
        .presentationDetents([.medium])
        .presentationDragIndicator(.visible)
        .presentationBackground(Theme.bgDeep)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("ControlConfirmationSheet")
    }

    private var confirmButton: some View {
        Button {
            onConfirm()
            dismiss()
        } label: {
            Text(confirmation.confirmLabel)
                .typography(Typography.bodyMedium)
                .foregroundStyle(confirmation.isDestructive ? Theme.textPrimary : Theme.bgDeep)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.md)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                        .fill(confirmation.isDestructive ? Theme.statusWorking : Theme.accent)
                )
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("control-confirm-button")
    }

    private var cancelButton: some View {
        Button {
            dismiss()
        } label: {
            Text("Cancel")
                .typography(Typography.bodyMedium)
                .foregroundStyle(Theme.textPrimary)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.md)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                        .fill(Theme.bgCard)
                )
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("control-cancel-button")
    }
}

/// Permanently-down `ConnectionStatusSource` used when no live store exists:
/// the gate then honestly reports commands paused. Under `-uitest-linkstate`
/// the gate's DEBUG forced state wins regardless.
@MainActor
private final class ControlFallbackConnectionSource: ConnectionStatusSource {
    var linkState: RemoteLinkState = .disconnected
    var peerConnected: Bool?
}

#if DEBUG
#Preview {
    Color.black.sheet(isPresented: .constant(true)) {
        SessionActionsSheet(
            sessionId: Wire.SessionId("sess_fix_login"),
            sessionName: "fix-login",
            status: .manual(label: "reviewing"))
    }
    .preferredColorScheme(.dark)
}
#endif
