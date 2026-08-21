//
//  AbandonConfirmView.swift
//  FlightDeckRemote
//
//  The one truly destructive path's type-to-confirm screen (PRD §5.6/§8):
//  "Abandon this worktree? Deletes the <name> worktree and its uncommitted
//  changes. The branch stays. This cannot be undone. Type <name> to confirm."
//  The destructive button stays disabled until the typed text matches the
//  session name exactly (`AbandonConfirmLogic` — exact match only). The
//  typed text itself is sent as `confirm_name`, so the desktop re-validates.
//

import SwiftUI

struct AbandonConfirmView: View {
    let sessionName: String
    let commandsPaused: Bool
    /// Called with the typed name on confirm; the parent runs the command so
    /// its outcome row shows the honest delivery state.
    let onConfirm: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var input = ""

    private var isConfirmed: Bool {
        AbandonConfirmLogic.isConfirmed(input: input, sessionName: sessionName)
    }

    private var canAbandon: Bool { isConfirmed && !commandsPaused }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.lg) {
            HStack(spacing: Theme.Spacing.sm) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(Theme.statusWorking)
                Text("Abandon this worktree?")
                    .typography(Typography.title)
                    .foregroundStyle(Theme.textPrimary)
            }
            .padding(.top, Theme.Spacing.xl)

            Text("Deletes the \(sessionName) worktree and its uncommitted changes. The branch stays. This cannot be undone.")
                .typography(Typography.body)
                .foregroundStyle(Theme.textMuted)

            if commandsPaused {
                Text("Commands are paused until the link is back.")
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textDim)
            }

            VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
                Text("Type \(sessionName) to confirm.")
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.textPrimary)
                nameField
            }

            abandonButton
            keepButton

            Spacer(minLength: 0)
        }
        .padding(Theme.Spacing.lg)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bgDeep)
        .presentationDragIndicator(.visible)
        .presentationBackground(Theme.bgDeep)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("AbandonConfirmView")
    }

    private var nameField: some View {
        TextField("", text: $input)
            .typography(Typography.mono)
            .foregroundStyle(Theme.textPrimary)
            .tint(Theme.accent)
            .autocorrectionDisabled()
            .textInputAutocapitalization(.never)
            .padding(.horizontal, Theme.Spacing.md)
            .padding(.vertical, Theme.Spacing.sm)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                    .fill(Theme.bgField)
            )
            .overlay(
                RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                    .stroke(isConfirmed ? Theme.statusWorking : Theme.bgRaised, lineWidth: 1)
            )
            .overlay(alignment: .leading) {
                if input.isEmpty {
                    Text(sessionName)
                        .typography(Typography.mono)
                        .foregroundStyle(Theme.textDim)
                        .padding(.horizontal, Theme.Spacing.md)
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                }
            }
            .accessibilityIdentifier("abandon-confirm-field")
    }

    private var abandonButton: some View {
        Button {
            onConfirm(input)
            dismiss()
        } label: {
            Text("Abandon worktree")
                .typography(Typography.bodyMedium)
                .foregroundStyle(canAbandon ? Theme.textPrimary : Theme.textDim)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.md)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                        .fill(canAbandon ? Theme.statusWorking : Theme.bgField)
                )
        }
        .buttonStyle(.plain)
        .disabled(!canAbandon)
        .accessibilityIdentifier("abandon-confirm-button")
    }

    private var keepButton: some View {
        Button {
            dismiss()
        } label: {
            Text("Keep it")
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
        .accessibilityIdentifier("abandon-keep-button")
    }
}

#if DEBUG
#Preview {
    Color.black.sheet(isPresented: .constant(true)) {
        AbandonConfirmView(sessionName: "fix-login", commandsPaused: false,
                           onConfirm: { _ in })
    }
    .preferredColorScheme(.dark)
}
#endif
