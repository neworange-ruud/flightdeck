//
//  ShellKeyBar.swift
//  FlightDeckRemote
//
//  The accessory key bar above the keyboard (PRD §5.4 — make-or-break, IS v1):
//  `Esc Tab Ctrl ←↑↓→ | / - ~ \` ⌃C paste`. `Ctrl` is a sticky modifier that
//  stays lit until the next key folds into a control byte (composition + byte
//  mapping live in the pure `ShellKeyBarLogic` / `ShellByteEncoding`; this view
//  only renders + forwards taps). Disabled + dimmed while commands are paused
//  (PRD §8: nothing sent blind). Horizontally scrollable so the full run fits
//  in portrait without truncation.
//

import SwiftUI

struct ShellKeyBar: View {
    let ctrlArmed: Bool
    let disabled: Bool
    let onKey: (ShellKey) -> Void

    /// Layout order (PRD §5.4).
    private let keys: [ShellKey] = [
        .escape, .tab, .ctrl, .left, .up, .down, .right,
        .pipe, .slash, .dash, .tilde, .backtick, .interrupt, .paste
    ]

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Theme.Spacing.xs) {
                ForEach(keys, id: \.self) { key in
                    keyButton(key)
                }
            }
            .padding(.horizontal, Theme.Spacing.md)
            .padding(.vertical, Theme.Spacing.sm)
        }
        .background(Theme.bgRaised)
        .opacity(disabled ? 0.5 : 1)
        .allowsHitTesting(!disabled)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("shell-key-bar")
    }

    @ViewBuilder
    private func keyButton(_ key: ShellKey) -> some View {
        let isCtrl = key == .ctrl
        let armed = isCtrl && ctrlArmed
        let isInterrupt = key == .interrupt
        Button {
            onKey(key)
        } label: {
            Text(key.label)
                .typography(Typography.monoMedium)
                .foregroundStyle(foreground(armed: armed, isInterrupt: isInterrupt))
                .padding(.horizontal, Theme.Spacing.md)
                .padding(.vertical, Theme.Spacing.sm)
                .frame(minWidth: 40)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card * 0.6, style: .continuous)
                        .fill(background(armed: armed, isInterrupt: isInterrupt))
                )
        }
        .buttonStyle(.plain)
        .disabled(disabled)
        .accessibilityIdentifier(key.accessibilityId)
        .accessibilityAddTraits(armed ? [.isSelected] : [])
        .accessibilityValue(armed ? "on" : "")
    }

    private func foreground(armed: Bool, isInterrupt: Bool) -> Color {
        if armed { return Theme.bgDeep }
        if isInterrupt { return Theme.statusRed }
        return Theme.textPrimary
    }

    private func background(armed: Bool, isInterrupt: Bool) -> Color {
        if armed { return Theme.accent }          // lit sticky Ctrl
        return Theme.bgField
    }
}

#if DEBUG
#Preview("Key bar") {
    VStack(spacing: 16) {
        ShellKeyBar(ctrlArmed: false, disabled: false, onKey: { _ in })
        ShellKeyBar(ctrlArmed: true, disabled: false, onKey: { _ in })
        ShellKeyBar(ctrlArmed: false, disabled: true, onKey: { _ in })
    }
    .padding()
    .background(Theme.bgDeep)
}
#endif
