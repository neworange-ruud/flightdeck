//
//  ChatTranscriptItemViews.swift
//  FlightDeckRemote
//
//  The per-item renderers for the cleaned transcript (PRD §5.3 3a):
//   - user prose (right-aligned, accented bubble) and agent prose (readable
//     left-aligned body in Geist);
//   - collapsed activity pills (icon by `ActivityKind` + summary + chevron)
//     that expand inline to show detail in Geist Mono, animating height;
//   - the inline permission-prompt card (orange border/glow, command in Geist
//     Mono, live Allow-once / Deny buttons that resolve inline with honest
//     sending / resolved / stale / failed states, plus the "or say 'approve' ·
//     hold mic below" voice hint);
//   - sparse timestamp dividers.
//
//  Decisions flow through the closures the row threads to `ChatViewModel`
//  (`decidePermission` / `retryPermission` / `retryOutgoing`).
//

import SwiftUI

// MARK: - Row dispatcher

/// Renders a single transcript row: an optional timestamp divider followed by
/// the item's variant view.
struct TranscriptRowView: View {
    let row: ChatRow
    let isExpanded: Bool
    let isPending: Bool
    let onToggle: () -> Void
    /// Send state when this row is an optimistic outgoing user message.
    let sendState: OutgoingState?
    /// Inline permission-decision state for a permission row.
    let permissionState: PermissionActionState
    /// Whether this permission row's Allow/Deny buttons are live.
    let permissionActionable: Bool
    let onDecide: (Wire.PromptId, PermissionAnswer) -> Void
    let onRetryPermission: (Wire.PromptId) -> Void
    let onRetryOutgoing: (Wire.ItemId) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            if row.showsTimestamp {
                TimestampDivider(atMs: row.item.atMs)
            }
            content
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private var content: some View {
        switch row.item {
        case let .userMessage(itemId, text, _):
            ProseBubble(text: text, sender: .user, sendState: sendState,
                        onRetry: { onRetryOutgoing(itemId) })
        case let .agentMessage(_, text, _):
            ProseBubble(text: text, sender: .agent, sendState: nil, onRetry: {})
        case let .activity(_, summary, detail, body, kind, _):
            ActivityPillView(index: row.index, summary: summary, detail: detail,
                             prose: body, kind: kind, isExpanded: isExpanded,
                             onToggle: onToggle)
        case let .permissionPrompt(_, promptId, kind, command, options, allowFreeText, multiSelect, _):
            PermissionPromptCard(promptId: promptId, kind: kind, command: command,
                                 options: options, allowFreeText: allowFreeText,
                                 multiSelect: multiSelect,
                                 isPending: isPending,
                                 actionState: permissionState,
                                 isActionable: permissionActionable,
                                 onDecide: { onDecide(promptId, $0) },
                                 onRetry: { onRetryPermission(promptId) })
        }
    }
}

// MARK: - Prose

/// A prose message. User messages are right-aligned in an accented bubble;
/// agent messages are left-aligned readable body. Agent prose is Markdown
/// (rendered as rich text via `MarkdownProseView`); user prose is verbatim.
struct ProseBubble: View {
    enum Sender { case user, agent }

    let text: String
    let sender: Sender
    /// Optimistic send state for a user message (nil = a settled/agent message).
    var sendState: OutgoingState? = nil
    /// Tap-to-retry for a failed outgoing message.
    var onRetry: () -> Void = {}

    private var isPending: Bool {
        if case .sending = sendState { return true }
        return false
    }

    private var failure: String? {
        if case let .failed(reason, _) = sendState { return reason }
        return nil
    }

    var body: some View {
        // Identifiers live on the leaf elements: an identifier on a plain
        // container propagates onto every accessibility element inside it and
        // would clobber the sending/retry markers' own identifiers (same trap
        // documented on `MainTabView`/`CustomTabBar`).
        VStack(alignment: .trailing, spacing: Theme.Spacing.xxs) {
            HStack {
                if sender == .user { Spacer(minLength: Theme.Spacing.xxl) }
                bubbleContent
                    .padding(.horizontal, Theme.Spacing.md)
                    .padding(.vertical, Theme.Spacing.sm)
                    .background(bubbleBackground)
                    .opacity(isPending ? 0.6 : 1)
                    .frame(maxWidth: .infinity, alignment: sender == .user ? .trailing : .leading)
                if sender == .agent { Spacer(minLength: Theme.Spacing.xxl) }
            }

            if isPending {
                Text("Sending…")
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textDim)
                    .accessibilityIdentifier("prose-user-sending")
            } else if let failure {
                Button(action: onRetry) {
                    HStack(spacing: Theme.Spacing.xs) {
                        Image(systemName: "exclamationmark.circle.fill")
                        Text("\(failure) — tap to retry")
                    }
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.statusWorking)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("prose-user-retry")
            }
        }
    }

    /// The bubble's text. User messages are verbatim (right-aligned, accented);
    /// agent messages are Markdown rendered as rich text (keeps `prose-agent`).
    @ViewBuilder
    private var bubbleContent: some View {
        switch sender {
        case .user:
            Text(text)
                .typography(Typography.body)
                .foregroundStyle(Theme.bgDeep)
                .multilineTextAlignment(.leading)
                .accessibilityIdentifier("prose-user")
        case .agent:
            MarkdownProseView(text: text, textColor: Theme.textPrimary,
                              identifier: "prose-agent")
        }
    }

    @ViewBuilder
    private var bubbleBackground: some View {
        if sender == .user {
            RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                .fill(Theme.accent)
        } else {
            RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                .fill(Theme.bgCard)
        }
    }
}

// MARK: - Activity pill

/// A collapsed activity pill that expands inline to reveal detail (Geist Mono).
struct ActivityPillView: View {
    let index: Int
    let summary: String
    let detail: String?
    /// Optional prose body attached to the activity (distinct from `detail`).
    let prose: String?
    let kind: Wire.ActivityKind
    let isExpanded: Bool
    let onToggle: () -> Void

    private var hasDetail: Bool { (detail?.isEmpty == false) || (prose?.isEmpty == false) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button(action: onToggle) {
                HStack(spacing: Theme.Spacing.sm) {
                    Image(systemName: kind.iconName)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.accent)
                        .frame(width: 18)
                    Text(summary)
                        .typography(Typography.callout)
                        .foregroundStyle(Theme.textPrimary)
                        .lineLimit(1)
                    Spacer(minLength: Theme.Spacing.sm)
                    if hasDetail {
                        Image(systemName: "chevron.down")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(Theme.textMuted)
                            .rotationEffect(.degrees(isExpanded ? 180 : 0))
                    }
                }
                .padding(.horizontal, Theme.Spacing.md)
                .padding(.vertical, Theme.Spacing.sm)
                // Make the whole row (including the Spacer gap) tappable — a
                // plain button's hit area is otherwise just the opaque glyphs.
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(!hasDetail)
            // Identifier on the button itself so a UI-test tap lands on the
            // tappable target (not the surrounding container).
            .accessibilityIdentifier("pill-\(index)")

            if isExpanded, hasDetail {
                VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
                    if let prose, !prose.isEmpty {
                        // Agent explanation prose can carry Markdown; the diff /
                        // command `detail` below stays raw mono.
                        MarkdownProseView(text: prose, textColor: Theme.textMuted)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    if let detail, !detail.isEmpty {
                        Text(detail)
                            .typography(Typography.monoSmall)
                            .foregroundStyle(Theme.textMuted)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .padding(.horizontal, Theme.Spacing.md)
                .padding(.bottom, Theme.Spacing.md)
                .transition(.opacity.combined(with: .move(edge: .top)))
                // Mark as a container so the identifier is queryable (a plain
                // stack with only an identifier is not an accessibility element).
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("pill-detail-\(index)")
            }
        }
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                .fill(Theme.bgField)
        )
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Permission prompt

/// Inline permission ask (PRD §5.3): orange border/glow, command in Geist
/// Mono, live Allow-once / Deny buttons, and the voice hint. Once a decision is
/// in flight the tapped button shows a spinner; on success the card collapses
/// to a muted resolved line; a stale (already-answered-on-desktop) rejection or
/// an undelivered failure surface honest inline notes (PRD §5.8).
struct PermissionPromptCard: View {
    let promptId: Wire.PromptId
    let kind: Wire.PromptKind
    let command: String
    let options: [Wire.PermissionOption]
    let allowFreeText: Bool
    /// Whether the prompt is a checklist (several options selected then submitted
    /// together) rather than a single tap-to-submit choice.
    let multiSelect: Bool
    let isPending: Bool
    let actionState: PermissionActionState
    /// Whether the options are live (current prompt + link up + no decision yet).
    let isActionable: Bool
    let onDecide: (PermissionAnswer) -> Void
    let onRetry: () -> Void

    /// Free-text draft + expand state. Local to the card — a fresh decision
    /// (new prompt row) always starts collapsed.
    @State private var freeTextDraft: String = ""
    @State private var freeTextExpanded = false
    /// The set of option indices toggled on in a multi-select checklist. Local
    /// to the card; a fresh prompt row starts with nothing selected.
    @State private var multiSelected: Set<Int> = []

    /// Keep the original 2-button horizontal Allow/Deny layout only for the
    /// classic binary permission shape (visual continuity); every other shape
    /// (N-option Question, or a permission prompt with a non-standard option
    /// count) renders as a vertical selectable list.
    private var isBinaryPermission: Bool {
        kind == .permission && options.count == 2
    }

    private var inFlight: PermissionAnswer? {
        if case let .sending(answer) = actionState { return answer }
        return nil
    }

    private var title: String {
        kind == .question ? "Question" : "Permission needed"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.md) {
            Text(title)
                .typography(Typography.captionBold)
                .foregroundStyle(Theme.accent)
                .textCase(.uppercase)

            Text(command)
                .typography(kind == .question ? Typography.bodyMedium : Typography.monoMedium)
                .foregroundStyle(Theme.textPrimary)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(Theme.Spacing.sm)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card - 6, style: .continuous)
                        .fill(Theme.bgField)
                )

            resolution
        }
        .padding(Theme.Spacing.lg)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                .fill(Theme.bgCard)
        )
        .overlay(
            RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous)
                .strokeBorder(Theme.accent, lineWidth: isPending ? 2 : 1)
        )
        .shadow(color: Theme.accent.opacity(isPending ? 0.45 : 0.2),
                radius: isPending ? 14 : 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("permission-prompt")
    }

    /// The lower half of the card, which swaps by decision state.
    @ViewBuilder
    private var resolution: some View {
        switch actionState {
        case let .resolved(answer):
            resolvedLine(answer)
        case .stale:
            Text("This prompt was already answered on the desktop")
                .typography(Typography.caption)
                .foregroundStyle(Theme.textMuted)
                .accessibilityIdentifier("permission-stale")
        case let .failed(reason, _, _):
            VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
                optionsSection
                Button(action: onRetry) {
                    HStack(spacing: Theme.Spacing.xs) {
                        Image(systemName: "exclamationmark.circle.fill")
                        Text("\(reason) — tap to retry")
                    }
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.statusWorking)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("permission-retry")
            }
        case .idle, .sending:
            VStack(alignment: .leading, spacing: Theme.Spacing.md) {
                optionsSection
                if kind == .permission {
                    Text("or say “approve” · hold mic below")
                        .typography(Typography.caption)
                        .foregroundStyle(Theme.textDim)
                        .accessibilityIdentifier("permission-voice-hint")
                }
            }
        }
    }

    private func resolvedLine(_ answer: PermissionAnswer) -> some View {
        Text(answer.resolvedText)
            .typography(Typography.callout)
            .foregroundStyle(Theme.textMuted)
            .accessibilityIdentifier("permission-resolved")
    }

    /// The live options: the binary 2-button row, or a vertical selectable
    /// list for N options — plus the free-text affordance when offered.
    @ViewBuilder
    private var optionsSection: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            if multiSelect {
                checklist
            } else if isBinaryPermission {
                buttons
            } else {
                optionList
            }
            if allowFreeText {
                freeTextAffordance
            }
        }
    }

    /// The answer currently being submitted from the checklist, if any (used to
    /// show the spinner on the Submit button while the multi-select is in flight).
    private var multiInFlight: PermissionAnswer? {
        if case let .sending(answer) = actionState, case .options = answer { return answer }
        return nil
    }

    /// A multi-select (checklist) Question: each option toggles on/off, and an
    /// explicit Submit button sends the whole set. Unlike the single-select list
    /// (where one tap selects AND submits), nothing is sent until Submit.
    private var checklist: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            ForEach(options, id: \.index) { option in
                let isOn = multiSelected.contains(option.index)
                Button {
                    if isOn { multiSelected.remove(option.index) }
                    else { multiSelected.insert(option.index) }
                } label: {
                    HStack(alignment: .top, spacing: Theme.Spacing.sm) {
                        Image(systemName: isOn ? "checkmark.square.fill" : "square")
                            .font(.system(size: 20))
                            .foregroundStyle(isOn ? Theme.accent : Theme.textMuted)
                        VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                            Text(option.label)
                                .typography(Typography.bodyMedium)
                                .foregroundStyle(Theme.textPrimary)
                                .multilineTextAlignment(.leading)
                            if let description = option.description, !description.isEmpty {
                                Text(description)
                                    .typography(Typography.caption)
                                    .foregroundStyle(Theme.textMuted)
                                    .multilineTextAlignment(.leading)
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .padding(Theme.Spacing.sm)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card - 4, style: .continuous)
                        .fill(Theme.bgRaised)
                )
                .disabled(!isActionable)
                .accessibilityIdentifier("permission-check-\(option.index)")
                .accessibilityAddTraits(isOn ? .isSelected : [])
            }
            submitButton
        }
    }

    /// The checklist's Submit control: sends the toggled-on options as one
    /// multi-select answer. Disabled until at least one option is selected.
    private var submitButton: some View {
        Button {
            let indices = options.map(\.index).filter { multiSelected.contains($0) }
            guard !indices.isEmpty else { return }
            let labels = options.filter { multiSelected.contains($0.index) }.map(\.label)
            onDecide(.options(indices: indices, labels: labels))
        } label: {
            ZStack {
                Text("Submit \(multiSelected.count) selected")
                    .typography(Typography.bodyMedium)
                    .opacity(multiInFlight == nil ? 1 : 0)
                if multiInFlight != nil {
                    WorkingSpinner(size: 16, lineWidth: 2, color: Theme.bgDeep)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, Theme.Spacing.sm)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(Theme.bgDeep)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.card - 4, style: .continuous)
                .fill(Theme.accent)
        )
        .disabled(!isActionable || multiSelected.isEmpty)
        .accessibilityIdentifier("permission-submit")
    }

    /// The classic 2-button horizontal Allow/Deny row (binary permission only).
    private var buttons: some View {
        HStack(spacing: Theme.Spacing.sm) {
            ForEach(options, id: \.index) { option in
                let choice = option.choice ?? .deny
                let isAllow = choice == .allowOnce
                let answer = PermissionAnswer.choice(choice)
                Button {
                    onDecide(answer)
                } label: {
                    ZStack {
                        Text(option.label)
                            .typography(Typography.bodyMedium)
                            .opacity(inFlight == answer ? 0 : 1)
                        if inFlight == answer {
                            WorkingSpinner(size: 16, lineWidth: 2,
                                           color: isAllow ? Theme.bgDeep : Theme.textPrimary)
                        }
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, Theme.Spacing.sm)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .foregroundStyle(isAllow ? Theme.bgDeep : Theme.textPrimary)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card - 4, style: .continuous)
                        .fill(isAllow ? Theme.accent : Theme.bgRaised)
                )
                .disabled(!isActionable)
                .accessibilityIdentifier(isAllow ? "permission-allow" : "permission-deny")
            }
        }
    }

    /// A vertical selectable list (label + optional description) — the
    /// default rendering for an N-option Question, and for any permission
    /// prompt that isn't the classic 2-option shape.
    private var optionList: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            ForEach(options, id: \.index) { option in
                let answer = PermissionAnswer.option(index: option.index, label: option.label)
                Button {
                    onDecide(answer)
                } label: {
                    HStack(alignment: .top, spacing: Theme.Spacing.sm) {
                        VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                            Text(option.label)
                                .typography(Typography.bodyMedium)
                                .foregroundStyle(Theme.textPrimary)
                                .multilineTextAlignment(.leading)
                            if let description = option.description, !description.isEmpty {
                                Text(description)
                                    .typography(Typography.caption)
                                    .foregroundStyle(Theme.textMuted)
                                    .multilineTextAlignment(.leading)
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        if inFlight == answer {
                            WorkingSpinner(size: 16, lineWidth: 2, color: Theme.textPrimary)
                        }
                    }
                    .padding(Theme.Spacing.sm)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card - 4, style: .continuous)
                        .fill(Theme.bgRaised)
                )
                .disabled(!isActionable)
                .accessibilityIdentifier("permission-option-\(option.index)")
            }
        }
    }

    /// "Type your own answer" — collapsed to a text-button affordance until
    /// tapped, then an expandable inline text field + send button.
    @ViewBuilder
    private var freeTextAffordance: some View {
        if freeTextExpanded {
            HStack(spacing: Theme.Spacing.sm) {
                TextField("Type your own answer", text: $freeTextDraft)
                    .typography(Typography.body)
                    .foregroundStyle(Theme.textPrimary)
                    .padding(Theme.Spacing.sm)
                    .background(
                        RoundedRectangle(cornerRadius: Theme.Radius.card - 6, style: .continuous)
                            .fill(Theme.bgField)
                    )
                    .disabled(!isActionable)
                    .accessibilityIdentifier("permission-free-text-field")
                Button {
                    let text = freeTextDraft.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !text.isEmpty else { return }
                    onDecide(.freeText(text))
                } label: {
                    if case let .sending(.freeText(pending)) = actionState,
                       pending == freeTextDraft.trimmingCharacters(in: .whitespacesAndNewlines) {
                        WorkingSpinner(size: 18, lineWidth: 2, color: Theme.accent)
                    } else {
                        Image(systemName: "arrow.up.circle.fill")
                            .font(.system(size: 22))
                            .foregroundStyle(Theme.accent)
                    }
                }
                .buttonStyle(.plain)
                .disabled(!isActionable
                    || freeTextDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityIdentifier("permission-free-text-send")
            }
        } else {
            Button {
                freeTextExpanded = true
            } label: {
                Text("Type your own answer")
                    .typography(Typography.callout)
                    .foregroundStyle(Theme.accent)
            }
            .buttonStyle(.plain)
            .disabled(!isActionable)
            .accessibilityIdentifier("permission-free-text-toggle")
        }
    }
}

// MARK: - Timestamp divider

/// A sparse, centered timestamp divider shown on long gaps.
struct TimestampDivider: View {
    let atMs: Int64

    private var label: String {
        let date = Date(timeIntervalSince1970: Double(atMs) / 1000)
        let formatter = DateFormatter()
        formatter.dateFormat = "HH:mm"
        return formatter.string(from: date)
    }

    var body: some View {
        Text(label)
            .typography(Typography.caption)
            .foregroundStyle(Theme.textDim)
            .frame(maxWidth: .infinity, alignment: .center)
            .padding(.vertical, Theme.Spacing.xs)
            .accessibilityIdentifier("timestamp-divider")
    }
}

// MARK: - Kind → icon

extension Wire.ActivityKind {
    /// SF Symbol representing the activity kind on a collapsed pill.
    var iconName: String {
        switch self {
        case .edit: "pencil"
        case .command: "terminal"
        case .test: "checkmark.seal"
        case .search: "magnifyingglass"
        case .other: "circle.dashed"
        }
    }
}

// MARK: - Wire → DesignSystem status mapping

extension Wire.AgentStatus {
    /// Maps the wire status onto the DesignSystem status used by `StatusDot`.
    var designSystem: AgentStatus {
        switch self {
        case .working: .working
        case .idle: .idle
        case .needsInput: .needsInput
        case let .manual(label): .manual(label: label)
        }
    }
}

// `Wire.AgentType.displayName` is provided by Features/Sessions/AgentTypeDisplay.swift
// (same module) — consumed here for the header, not redeclared.
