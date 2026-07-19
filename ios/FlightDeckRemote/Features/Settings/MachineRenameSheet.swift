//
//  MachineRenameSheet.swift
//  FlightDeckRemote
//
//  Per-machine rename affordance (remote-control-b8d.9): sets
//  `PairedInstance.userOverrideName`, which always wins over the desktop-
//  reported `machineNameFromDesktop` in `displayName`'s precedence — see that
//  property's doc comment. Renaming is entirely local to the phone; nothing
//  here talks to the relay or the desktop.
//
//  Reachable from `SettingsView`'s "Machines" section (b8d.7 added the
//  section + Add-machine entry point; this sheet is the per-row rename this
//  issue adds on top of it).
//
//  Mirrors `ManualStatusSheet`'s shape (a lightweight custom sheet rather than
//  a `Form`, to match the app's card-based design system): a header with
//  Cancel, a styled text field seeded from the current override (or the
//  desktop name, or blank), a Save button, and — only when an override is
//  currently set — a "Reset to Mac's name" button that clears it back to the
//  desktop-reported default.
//

import SwiftUI

struct MachineRenameSheet: View {
    var pairingStore: PairingStore
    let pairingId: String

    @Environment(\.dismiss) private var dismiss
    @State private var name: String = ""

    private var instance: PairedInstance? {
        pairingStore.list.first { $0.pairingId == pairingId }
    }

    /// Trimmed + length-bounded (REMOTE_PROTOCOL §5.7's 64-char rule applies
    /// equally to a phone-set override — the same `sanitizeMachineName` the
    /// wire path uses), or `nil` when the field is empty/whitespace-only.
    private var sanitizedName: String? {
        Wire.sanitizeMachineName(name)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.lg) {
            header

            nameField

            if let desktopName = instance?.machineNameFromDesktop, !desktopName.isEmpty {
                Text("Reported by the Mac as \u{201C}\(desktopName)\u{201D}.")
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textDim)
                    .accessibilityIdentifier("machine-rename-desktop-name-note")
            } else {
                Text("The Mac hasn't reported a name yet.")
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textDim)
                    .accessibilityIdentifier("machine-rename-no-desktop-name-note")
            }

            saveButton

            if instance?.userOverrideName != nil {
                resetButton
            }

            Spacer(minLength: 0)
        }
        .padding(Theme.Spacing.lg)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bgDeep)
        .presentationDetents([.medium])
        .presentationDragIndicator(.visible)
        .presentationBackground(Theme.bgDeep)
        .onAppear {
            name = instance?.userOverrideName ?? instance?.machineNameFromDesktop ?? ""
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("MachineRenameSheet")
    }

    private var header: some View {
        HStack(spacing: Theme.Spacing.md) {
            Text("Rename machine")
                .typography(Typography.headline)
                .foregroundStyle(Theme.textPrimary)
            Spacer(minLength: Theme.Spacing.sm)
            Button("Cancel") { dismiss() }
                .typography(Typography.callout)
                .foregroundStyle(Theme.accent)
                .accessibilityIdentifier("machine-rename-cancel")
        }
        .padding(.top, Theme.Spacing.md)
    }

    private var nameField: some View {
        TextField("", text: $name)
            .typography(Typography.body)
            .foregroundStyle(Theme.textPrimary)
            .tint(Theme.accent)
            .autocorrectionDisabled()
            .submitLabel(.done)
            .padding(.horizontal, Theme.Spacing.md)
            .padding(.vertical, Theme.Spacing.sm)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                    .fill(Theme.bgField)
            )
            .overlay(alignment: .leading) {
                if name.isEmpty {
                    Text("Machine name")
                        .typography(Typography.body)
                        .foregroundStyle(Theme.textDim)
                        .padding(.horizontal, Theme.Spacing.md)
                        .allowsHitTesting(false)
                        .accessibilityHidden(true)
                }
            }
            .accessibilityIdentifier("machine-rename-textfield")
    }

    private var saveButton: some View {
        Button {
            guard let sanitized = sanitizedName else { return }
            pairingStore.setOverrideName(pairingId: pairingId, sanitized)
            dismiss()
        } label: {
            Text("Save")
                .typography(Typography.bodyMedium)
                .foregroundStyle(sanitizedName != nil ? Theme.bgDeep : Theme.textDim)
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.md)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                        .fill(sanitizedName != nil ? Theme.accent : Theme.bgField)
                )
        }
        .buttonStyle(.plain)
        .disabled(sanitizedName == nil)
        .accessibilityIdentifier("machine-rename-save")
    }

    /// Clears the override — `PairingStore.setOverrideName(pairingId:nil)` —
    /// so `displayName` falls back to `machineNameFromDesktop` (or the
    /// generic fallback if the Mac never announced a name either).
    private var resetButton: some View {
        Button {
            pairingStore.setOverrideName(pairingId: pairingId, nil)
            dismiss()
        } label: {
            Text("Reset to Mac's name")
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
        .accessibilityIdentifier("machine-rename-reset")
    }
}

#if DEBUG
#Preview {
    let store = PairingStore()
    store.add(PairedInstance(
        pairingId: "preview-pairing",
        machineNameFromDesktop: "Ruud's MacBook Pro",
        relayURL: URL(string: "wss://relay.example/v1")!
    ))
    return Color.black.sheet(isPresented: .constant(true)) {
        MachineRenameSheet(pairingStore: store, pairingId: "preview-pairing")
    }
    .preferredColorScheme(.dark)
}
#endif
