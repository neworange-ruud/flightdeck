//
//  ManualStatusSheet.swift
//  FlightDeckRemote
//
//  The "Set manual status" sub-sheet (PRD §5.8): the phone sets the cyan
//  manual override with a short label (matches desktop); it clears on the
//  next real state change, or explicitly via "Clear override" here. A label
//  field + preset chips + Set/Clear — the parent `SessionActionsSheet` runs
//  the actual command so its outcome row shows the honest delivery state.
//

import SwiftUI

struct ManualStatusSheet: View {
    let sessionName: String
    var currentStatus: Wire.AgentStatus?
    let commandsPaused: Bool
    /// Set the override with this (trimmed, non-empty) label.
    let onSet: (String) -> Void
    /// Clear an existing override.
    let onClear: () -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var label = ""

    static let presets = ["reviewing", "blocked", "on hold", "waiting on CI"]

    private var trimmed: String {
        label.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var canSet: Bool { !trimmed.isEmpty && !commandsPaused }

    private var currentOverrideLabel: String? {
        if case let .manual(existing)? = currentStatus { return existing }
        return nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.lg) {
            header

            if commandsPaused {
                Text("Commands are paused until the link is back.")
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textDim)
            }

            labelField
            presetChips

            setButton

            if let existing = currentOverrideLabel {
                clearButton(existing: existing)
            }

            Text("The cyan override clears automatically on the agent's next real state change.")
                .typography(Typography.caption)
                .foregroundStyle(Theme.textDim)

            Spacer(minLength: 0)
        }
        .padding(Theme.Spacing.lg)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bgDeep)
        .presentationDetents([.medium])
        .presentationDragIndicator(.visible)
        .presentationBackground(Theme.bgDeep)
        .onAppear {
            if let existing = currentOverrideLabel { label = existing }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("ManualStatusSheet")
    }

    private var header: some View {
        HStack(spacing: Theme.Spacing.md) {
            HStack(spacing: Theme.Spacing.sm) {
                Circle()
                    .fill(Theme.statusManual)
                    .frame(width: 10, height: 10)
                Text("Set manual status")
                    .typography(Typography.headline)
                    .foregroundStyle(Theme.textPrimary)
            }
            Spacer(minLength: Theme.Spacing.sm)
            Button("Cancel") { dismiss() }
                .typography(Typography.callout)
                .foregroundStyle(Theme.accent)
                .accessibilityIdentifier("manual-status-cancel")
        }
        .padding(.top, Theme.Spacing.md)
    }

    private var labelField: some View {
        TextField("", text: $label)
            .typography(Typography.body)
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
            .overlay(alignment: .leading) {
                if label.isEmpty {
                    Text("Label, e.g. reviewing")
                        .typography(Typography.body)
                        .foregroundStyle(Theme.textDim)
                        .padding(.horizontal, Theme.Spacing.md)
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                }
            }
            .accessibilityIdentifier("manual-status-field")
    }

    private var presetChips: some View {
        // Four short presets — a fixed two-row flow keeps this dependency-free.
        let rows = [Array(Self.presets.prefix(2)), Array(Self.presets.dropFirst(2))]
        return VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            ForEach(rows, id: \.self) { row in
                HStack(spacing: Theme.Spacing.sm) {
                    ForEach(row, id: \.self) { preset in
                        chip(preset)
                    }
                }
            }
        }
    }

    private func chip(_ preset: String) -> some View {
        Button {
            label = preset
        } label: {
            Text(preset)
                .typography(Typography.callout)
                .foregroundStyle(label == preset ? Theme.bgDeep : Theme.statusManual)
                .padding(.horizontal, Theme.Spacing.md)
                .padding(.vertical, Theme.Spacing.xs)
                .background(
                    Capsule().fill(label == preset ? Theme.statusManual : Theme.statusManual.opacity(0.12))
                )
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("manual-status-chip-\(preset.replacingOccurrences(of: " ", with: "-"))")
    }

    private var setButton: some View {
        Button {
            onSet(trimmed)
            dismiss()
        } label: {
            Text("Set status")
                .typography(Typography.bodyMedium)
                .foregroundStyle(canSet ? Theme.bgDeep : Theme.textDim)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.md)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                        .fill(canSet ? Theme.statusManual : Theme.bgField)
                )
        }
        .buttonStyle(.plain)
        .disabled(!canSet)
        .accessibilityIdentifier("manual-status-set")
    }

    private func clearButton(existing: String) -> some View {
        Button {
            onClear()
            dismiss()
        } label: {
            Text("Clear override (\(existing))")
                .typography(Typography.bodyMedium)
                .foregroundStyle(commandsPaused ? Theme.textDim : Theme.textPrimary)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.md)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                        .fill(Theme.bgCard)
                )
        }
        .buttonStyle(.plain)
        .disabled(commandsPaused)
        .accessibilityIdentifier("manual-status-clear")
    }
}

#if DEBUG
#Preview {
    Color.black.sheet(isPresented: .constant(true)) {
        ManualStatusSheet(sessionName: "update-docs",
                          currentStatus: .manual(label: "reviewing"),
                          commandsPaused: false,
                          onSet: { _ in }, onClear: {})
    }
    .preferredColorScheme(.dark)
}
#endif
