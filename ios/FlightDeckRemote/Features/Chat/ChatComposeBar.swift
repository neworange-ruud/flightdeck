//
//  ChatComposeBar.swift
//  FlightDeckRemote
//
//  The real compose bar (PRD §5.3): a growing text field ("Reply to
//  fix-login…"), an orange send button, and a mic affordance. State-changing
//  by nature, so it honours the commands-paused gate (PRD §8): while the link
//  is down the send button is disabled and a subtle "paused — reconnecting"
//  label states it honestly, rather than letting the user send blind.
//
//  Voice: v1 mic is the *system keyboard dictation* affordance only (PRD §7
//  MVP: native dictation + strict edit-before-send). The mic button focuses the
//  field and surfaces the keyboard (whose dictation key does the STT); a
//  separate `onHoldToTalk` seam closure is left for the custom hold-to-talk
//  voice task (`remote-control-chat-voice-dictation`) and is not wired here.
//

import SwiftUI

struct ChatComposeBar: View {
    /// Session name for the placeholder prompt ("Reply to fix-login…").
    let sessionName: String
    /// The compose text (two-way bound to the view-model's `draft`).
    @Binding var text: String
    /// Whether commands are paused (link down) — disables send + shows the note.
    let commandsPaused: Bool
    /// Send the current text.
    let onSend: () -> Void
    /// Seam for the future custom hold-to-talk voice task. When set, the mic
    /// button routes to it; otherwise the mic focuses the field so the system
    /// keyboard's dictation key is reachable (the v1 behaviour).
    var onHoldToTalk: (() -> Void)?

    @FocusState private var isFocused: Bool

    private var trimmed: String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var canSend: Bool { !trimmed.isEmpty && !commandsPaused }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            if commandsPaused {
                Text("paused — reconnecting")
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textDim)
                    .accessibilityIdentifier("compose-paused-label")
            }

            HStack(alignment: .bottom, spacing: Theme.Spacing.sm) {
                micButton

                TextField("", text: $text, axis: .vertical)
                    .lineLimit(1...5)
                    .typography(Typography.body)
                    .foregroundStyle(Theme.textPrimary)
                    .tint(Theme.accent)
                    .focused($isFocused)
                    .submitLabel(.send)
                    .padding(.horizontal, Theme.Spacing.md)
                    .padding(.vertical, Theme.Spacing.sm)
                    .background(
                        RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                            .fill(Theme.bgField)
                    )
                    .overlay(alignment: .leading) {
                        if text.isEmpty {
                            Text("Reply to \(sessionName)…")
                                .typography(Typography.body)
                                .foregroundStyle(Theme.textDim)
                                .padding(.horizontal, Theme.Spacing.md)
                                .allowsHitTesting(false)
                        }
                    }
                    .accessibilityIdentifier("compose-field")

                sendButton
            }
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.vertical, Theme.Spacing.sm)
        .background(Theme.bgDeep)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("chat-compose-bar")
    }

    private var micButton: some View {
        Button {
            if let onHoldToTalk {
                onHoldToTalk()
            } else {
                // v1: focus the field so the keyboard (with its dictation key)
                // comes up — free iOS dictation, edit-before-send preserved.
                isFocused = true
            }
        } label: {
            Image(systemName: "mic.fill")
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(Theme.textMuted)
                .frame(width: 44, height: 44)
                .background(Circle().fill(Theme.bgField))
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("compose-mic")
    }

    private var sendButton: some View {
        Button(action: onSend) {
            Image(systemName: "arrow.up")
                .font(.system(size: 18, weight: .bold))
                .foregroundStyle(canSend ? Theme.bgDeep : Theme.textDim)
                .frame(width: 44, height: 44)
                .background(
                    Circle().fill(canSend ? Theme.accent : Theme.bgField)
                )
                .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .disabled(!canSend)
        .accessibilityIdentifier("compose-send")
    }
}

#if DEBUG
#Preview {
    struct Harness: View {
        @State var text = ""
        var body: some View {
            VStack {
                Spacer()
                ChatComposeBar(sessionName: "fix-login", text: $text,
                               commandsPaused: false, onSend: {})
                ChatComposeBar(sessionName: "fix-login", text: .constant("hi"),
                               commandsPaused: true, onSend: {})
            }
            .background(Theme.bgDeep)
        }
    }
    return Harness().preferredColorScheme(.dark)
}
#endif
