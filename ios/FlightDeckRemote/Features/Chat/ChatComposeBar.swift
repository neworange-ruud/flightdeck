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
//  Voice (PRD §7): the mic is push-to-talk. HOLD it to record ("Listening…"),
//  RELEASE to stop; the transcription drops into this field as EDITABLE text
//  (edit-before-send, always — never auto-sent). A quick tap (a hold released
//  before the dictation minimum) falls back to focusing the field so the system
//  keyboard's dictation key is reachable — the v1 affordance, still available
//  (and the only behaviour when no hold-to-talk handler is wired).
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
    /// Whether push-to-talk is actively recording — drives the "Listening…"
    /// indicator and the mic's hot appearance.
    var isListening: Bool = false
    /// Push-to-talk hold began (mic pressed). When set, the mic drives voice
    /// dictation; when `nil`, the mic just focuses the field (v1 behaviour).
    var onHoldBegin: (() -> Void)?
    /// Push-to-talk hold ended (mic released) — the controller stops recording
    /// and either drops the transcript in or (on a mis-tap) triggers the field.
    var onHoldEnd: (() -> Void)?

    @FocusState private var isFocused: Bool
    /// When the mic is pressed, the wall-clock press start — a release before
    /// `quickTapThreshold` is treated as a tap (focus the field), not a hold.
    @State private var pressStart: Date?

    /// Below this hold duration a mic press is a tap (focus the field), not a
    /// dictation. Matches `DictationStateMachine.minimumHoldDuration`.
    private let quickTapThreshold: TimeInterval = 0.35

    private var voiceEnabled: Bool { onHoldBegin != nil }

    private var trimmed: String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var canSend: Bool { !trimmed.isEmpty && !commandsPaused }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            if isListening {
                listeningIndicator
            } else if commandsPaused {
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

    private var listeningIndicator: some View {
        HStack(spacing: Theme.Spacing.xs) {
            Circle()
                .fill(Theme.accent)
                .frame(width: 8, height: 8)
            Text("Listening…")
                .typography(Typography.caption)
                .foregroundStyle(Theme.accent)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Listening")
        .accessibilityIdentifier("compose-listening-indicator")
    }

    // The mic is a plain shape driven by a press/release gesture when voice is
    // wired (hold to talk, quick tap to focus); a simple Button otherwise.
    private var micButton: some View {
        micLabel
            .contentShape(Circle())
            .accessibilityIdentifier("compose-hold-to-talk")
            .accessibilityLabel(isListening ? "Recording — release to stop"
                                             : "Hold to talk")
            .accessibilityAddTraits(.isButton)
            .gesture(voiceEnabled ? micGesture : nil)
            .onTapGesture {
                // The no-voice fallback (and a belt-and-braces path for the
                // gesture): a plain tap focuses the field for keyboard dictation.
                if !voiceEnabled { isFocused = true }
            }
    }

    private var micLabel: some View {
        Image(systemName: isListening ? "waveform" : "mic.fill")
            .font(.system(size: 18, weight: .semibold))
            .foregroundStyle(isListening ? Theme.bgDeep : Theme.textMuted)
            .frame(width: 44, height: 44)
            .background(Circle().fill(isListening ? Theme.accent : Theme.bgField))
    }

    /// Press-and-hold to record; release to stop. A release before
    /// `quickTapThreshold` focuses the field instead (v1 keyboard dictation).
    private var micGesture: some Gesture {
        DragGesture(minimumDistance: 0)
            .onChanged { _ in
                guard pressStart == nil else { return }
                pressStart = Date()
                onHoldBegin?()
            }
            .onEnded { _ in
                let held = pressStart.map { Date().timeIntervalSince($0) } ?? 0
                pressStart = nil
                onHoldEnd?()
                if held < quickTapThreshold { isFocused = true }
            }
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
