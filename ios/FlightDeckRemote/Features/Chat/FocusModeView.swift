//
//  FocusModeView.swift
//  FlightDeckRemote
//
//  The eyes-free focus mode (PRD §5.3 3b): the pending permission ask pinned
//  large, big Approve / Deny buttons, and a condensed history timeline. Voice
//  reply reuses the compose bar's push-to-talk dictation (PRD §7); "type
//  instead" is the same field. It degrades gracefully to full visual reading —
//  the timeline scrolls and stays legible when there is no pending ask.
//
//  Approve / Deny route through the SAME inline permission-resolution path as
//  the transcript card (`ChatViewModel.decidePermission`), so honest sending /
//  resolved / stale / paused states are shared, not reimplemented.
//
//  "Read aloud" (TTS via `AVSpeechSynthesizer`) is a FAST-FOLLOW per PRD §7 — it
//  is present as a clearly-disabled "coming soon" affordance, not wired.
//

import SwiftUI

struct FocusModeView: View {

    @Bindable var model: ChatViewModel
    let dictation: DictationController
    @Environment(\.dismiss) private var dismiss
    /// Options toggled on in a multi-select checklist, before Submit.
    @State private var focusSelected: Set<Int> = []

    var body: some View {
        // Read observable state in the tracked top scope (same pattern as
        // AgentChatView) so decisions / streamed appends re-render.
        let presentation = FocusMode.presentation(items: model.displayItems,
                                                   currentPending: model.currentPendingPromptId)
        let paused = model.commandsPaused
        let actionState = presentation.pendingPromptId.map { model.permissionActionState($0) } ?? .idle
        let actionable = presentation.pendingPromptId.map { model.isPermissionActionable($0) } ?? false

        return VStack(spacing: 0) {
            header
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.Spacing.xl) {
                    if let command = presentation.pendingCommand,
                       let promptId = presentation.pendingPromptId {
                        pinnedAsk(command: command)
                        decisionButtons(promptId: promptId, options: presentation.options,
                                        multiSelect: presentation.multiSelect,
                                        actionState: actionState, actionable: actionable)
                    } else {
                        noPendingNote
                    }
                    timeline(presentation.timeline)
                }
                .padding(Theme.Spacing.lg)
            }
            replyBar(paused: paused)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.bgDeep)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("focus-mode")
        .task { await dictation.prepare() }
        .onDisappear { dictation.cancel() }
    }

    // MARK: - Header

    private var header: some View {
        HStack {
            Text("Focus")
                .typography(Typography.headline)
                .foregroundStyle(Theme.textPrimary)
            Spacer()
            Button {
                dismiss()
            } label: {
                Label("Exit", systemImage: "xmark")
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.textMuted)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("focus-exit")
        }
        .padding(.horizontal, Theme.Spacing.lg)
        .padding(.vertical, Theme.Spacing.md)
        .background(Theme.bgRaised)
    }

    // MARK: - Pinned ask

    private func pinnedAsk(command: String) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.md) {
            HStack {
                Text("Permission needed")
                    .typography(Typography.captionBold)
                    .foregroundStyle(Theme.accent)
                    .textCase(.uppercase)
                Spacer()
                readAloudButton
            }
            Text(command)
                .typography(Typography.monoMedium)
                .foregroundStyle(Theme.textPrimary)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(Theme.Spacing.lg)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                        .fill(Theme.bgField)
                )
                .accessibilityIdentifier("focus-pending-command")
        }
    }

    /// FAST-FOLLOW (PRD §7): TTS readback is not wired in v1 — a disabled,
    /// clearly-labelled "coming soon" affordance.
    private var readAloudButton: some View {
        Label("Read aloud", systemImage: "speaker.wave.2")
            .typography(Typography.caption)
            .foregroundStyle(Theme.textDim)
            .padding(.horizontal, Theme.Spacing.sm)
            .padding(.vertical, Theme.Spacing.xs)
            .background(Capsule().fill(Theme.bgField))
            .overlay(alignment: .topTrailing) {
                Text("soon")
                    .typography(Typography.captionBold)
                    .foregroundStyle(Theme.textDim)
                    .offset(y: -10)
            }
            .accessibilityIdentifier("focus-read-aloud")
            .accessibilityLabel("Read aloud, coming soon")
            .accessibilityAddTraits(.isButton)
            .accessibilityRemoveTraits(.isButton) // announced but not actionable
    }

    // MARK: - Approve / Deny

    private func decisionButtons(promptId: Wire.PromptId, options: [Wire.PermissionOption],
                                 multiSelect: Bool,
                                 actionState: PermissionActionState, actionable: Bool) -> some View {
        VStack(spacing: Theme.Spacing.md) {
            switch actionState {
            case let .resolved(answer):
                resolvedBanner(answer)
            case .stale:
                Text("Already answered on the desktop")
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.textMuted)
                    .accessibilityIdentifier("focus-stale")
            default:
                if multiSelect {
                    focusChecklist(promptId: promptId, options: options,
                                   actionState: actionState, actionable: actionable)
                } else {
                    HStack(spacing: Theme.Spacing.md) {
                        ForEach(Array(options.enumerated()), id: \.offset) { _, option in
                            bigDecisionButton(option: option, actionState: actionState,
                                              actionable: actionable, promptId: promptId)
                        }
                    }
                }
            }
        }
    }

    /// A multi-select checklist for focus mode: toggle several options, then a
    /// full-width Submit sends them together (mirrors the chat card's checklist).
    private func focusChecklist(promptId: Wire.PromptId, options: [Wire.PermissionOption],
                                actionState: PermissionActionState, actionable: Bool) -> some View {
        let submitting: Bool = {
            if case let .sending(answer) = actionState, case .options = answer { return true }
            return false
        }()
        return VStack(spacing: Theme.Spacing.sm) {
            ForEach(options, id: \.index) { option in
                let isOn = focusSelected.contains(option.index)
                Button {
                    if isOn { focusSelected.remove(option.index) }
                    else { focusSelected.insert(option.index) }
                } label: {
                    HStack(spacing: Theme.Spacing.sm) {
                        Image(systemName: isOn ? "checkmark.square.fill" : "square")
                            .font(.system(size: 22))
                            .foregroundStyle(isOn ? Theme.accent : Theme.textMuted)
                        Text(option.label)
                            .typography(Typography.headline)
                            .foregroundStyle(Theme.textPrimary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .padding(Theme.Spacing.md)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                        .fill(Theme.bgRaised)
                )
                .disabled(!actionable)
                .accessibilityIdentifier("focus-check-\(option.index)")
                .accessibilityAddTraits(isOn ? .isSelected : [])
            }
            Button {
                let indices = options.map(\.index).filter { focusSelected.contains($0) }
                guard !indices.isEmpty else { return }
                let labels = options.filter { focusSelected.contains($0.index) }.map(\.label)
                model.decidePermission(promptId: promptId, optionIndices: indices, labels: labels)
            } label: {
                ZStack {
                    Text("Submit \(focusSelected.count) selected")
                        .typography(Typography.headline)
                        .opacity(submitting ? 0 : 1)
                    if submitting {
                        WorkingSpinner(size: 22, lineWidth: 2.5, color: Theme.bgDeep)
                    }
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.lg)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(Theme.bgDeep)
            .background(
                RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                    .fill(Theme.accent)
            )
            .disabled(!actionable || focusSelected.isEmpty)
            .opacity(actionable ? 1 : 0.5)
            .accessibilityIdentifier("focus-submit")
        }
    }

    private func bigDecisionButton(option: Wire.PermissionOption,
                                   actionState: PermissionActionState,
                                   actionable: Bool, promptId: Wire.PromptId) -> some View {
        // Binary permission options map to the choice fast-path; a Question's
        // options (no `choice`) answer by index instead.
        let answer: PermissionAnswer = option.choice.map { .choice($0) }
            ?? .option(index: option.index, label: option.label)
        let isAllow = option.choice == .allowOnce
        let inFlight: Bool = {
            if case let .sending(sent) = actionState { return sent == answer }
            return false
        }()
        return Button {
            switch answer {
            case let .choice(choice):
                model.decidePermission(promptId: promptId, choice: choice)
            case let .option(index, label):
                model.decidePermission(promptId: promptId, optionIndex: index, label: label)
            case .options, .answers, .freeText:
                break // single-tap buttons never carry a multi-select, multi-question, or free-text answer
            }
        } label: {
            ZStack {
                Text(option.label)
                    .typography(Typography.headline)
                    .opacity(inFlight ? 0 : 1)
                if inFlight {
                    WorkingSpinner(size: 22, lineWidth: 2.5,
                                   color: isAllow ? Theme.bgDeep : Theme.textPrimary)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, Theme.Spacing.lg)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(isAllow ? Theme.bgDeep : Theme.textPrimary)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                .fill(isAllow ? Theme.accent : Theme.bgRaised)
        )
        .disabled(!actionable)
        .opacity(actionable ? 1 : 0.5)
        .accessibilityIdentifier(isAllow ? "focus-approve" : "focus-deny")
    }

    private func resolvedBanner(_ answer: PermissionAnswer) -> some View {
        Text(answer.resolvedText)
            .typography(Typography.headline)
            .foregroundStyle(Theme.textMuted)
            .frame(maxWidth: .infinity)
            .padding(.vertical, Theme.Spacing.md)
            .accessibilityIdentifier("focus-resolved")
    }

    private var noPendingNote: some View {
        VStack(spacing: Theme.Spacing.sm) {
            Image(systemName: "checkmark.circle")
                .font(.system(size: 36))
                .foregroundStyle(Theme.textMuted)
            Text("Nothing waiting on you")
                .typography(Typography.headline)
                .foregroundStyle(Theme.textPrimary)
            Text("The agent isn't asking for a decision right now.")
                .typography(Typography.caption)
                .foregroundStyle(Theme.textDim)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Theme.Spacing.lg)
        .accessibilityIdentifier("focus-no-pending")
    }

    // MARK: - Timeline

    private func timeline(_ entries: [FocusTimelineEntry]) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            Text("Recently")
                .typography(Typography.captionBold)
                .foregroundStyle(Theme.textDim)
                .textCase(.uppercase)
            ForEach(entries) { entry in
                HStack(alignment: .top, spacing: Theme.Spacing.md) {
                    Text(entry.text)
                        .typography(entry.isPending ? Typography.bodyMedium : Typography.callout)
                        .foregroundStyle(entry.isPending ? Theme.textPrimary : Theme.textMuted)
                        .frame(maxWidth: .infinity, alignment: .leading)
                    Text(entry.timeLabel)
                        .typography(Typography.caption)
                        .foregroundStyle(entry.isPending ? Theme.accent : Theme.textDim)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("focus-timeline")
    }

    // MARK: - Reply (voice / type), reusing the compose bar's dictation + send

    private func replyBar(paused: Bool) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
            Text("Hold the mic to reply by voice · or type")
                .typography(Typography.caption)
                .foregroundStyle(Theme.textDim)
                .padding(.horizontal, Theme.Spacing.lg)
                .accessibilityIdentifier("focus-reply-hint")
            ChatComposeBar(sessionName: model.sessionName,
                           text: $model.draft,
                           commandsPaused: paused,
                           onSend: { model.send() },
                           isListening: dictation.isListening,
                           onHoldBegin: { dictation.beginHold() },
                           onHoldEnd: { dictation.endHold() })
        }
    }
}

#if DEBUG
#Preview {
    let model = ChatViewModel(projectId: Wire.ProjectId("p1"),
                              sessionId: Wire.SessionId("s1"))
    model.loadFixture()
    return FocusModeView(model: model, dictation: DictationController())
        .preferredColorScheme(.dark)
}
#endif
