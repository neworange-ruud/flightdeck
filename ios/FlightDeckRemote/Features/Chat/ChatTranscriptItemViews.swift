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
//     Mono, Allow-once / Deny buttons rendered but *disabled* this task, plus
//     the "or say 'approve' · hold mic below" voice hint);
//   - sparse timestamp dividers.
//
//  The Allow/Deny buttons are visually complete but inert: the real decision
//  wiring is the chat-permission task and flows through
//  `AgentChatView.onPermissionDecision`.
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
    let onPermissionDecision: ((Wire.PromptId, Bool) -> Void)?

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
        case let .userMessage(_, text, _):
            ProseBubble(text: text, sender: .user)
        case let .agentMessage(_, text, _):
            ProseBubble(text: text, sender: .agent)
        case let .activity(_, summary, detail, body, kind, _):
            ActivityPillView(index: row.index, summary: summary, detail: detail,
                             prose: body, kind: kind, isExpanded: isExpanded,
                             onToggle: onToggle)
        case let .permissionPrompt(_, promptId, command, options, _):
            PermissionPromptCard(promptId: promptId, command: command,
                                 options: options, isPending: isPending,
                                 onPermissionDecision: onPermissionDecision)
        }
    }
}

// MARK: - Prose

/// A prose message. User messages are right-aligned in an accented bubble;
/// agent messages are left-aligned readable body.
struct ProseBubble: View {
    enum Sender { case user, agent }

    let text: String
    let sender: Sender

    var body: some View {
        HStack {
            if sender == .user { Spacer(minLength: Theme.Spacing.xxl) }
            Text(text)
                .typography(Typography.body)
                .foregroundStyle(sender == .user ? Theme.bgDeep : Theme.textPrimary)
                .multilineTextAlignment(.leading)
                .padding(.horizontal, Theme.Spacing.md)
                .padding(.vertical, Theme.Spacing.sm)
                .background(bubbleBackground)
                .frame(maxWidth: .infinity, alignment: sender == .user ? .trailing : .leading)
            if sender == .agent { Spacer(minLength: Theme.Spacing.xxl) }
        }
        .accessibilityIdentifier(sender == .user ? "prose-user" : "prose-agent")
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
                        Text(prose)
                            .typography(Typography.callout)
                            .foregroundStyle(Theme.textMuted)
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
/// Mono, Allow-once / Deny buttons (rendered but disabled this task) and the
/// voice hint.
struct PermissionPromptCard: View {
    let promptId: Wire.PromptId
    let command: String
    let options: [Wire.PermissionOption]
    let isPending: Bool
    let onPermissionDecision: ((Wire.PromptId, Bool) -> Void)?

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.md) {
            Text("Permission needed")
                .typography(Typography.captionBold)
                .foregroundStyle(Theme.accent)
                .textCase(.uppercase)

            Text(command)
                .typography(Typography.monoMedium)
                .foregroundStyle(Theme.textPrimary)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(Theme.Spacing.sm)
                .background(
                    RoundedRectangle(cornerRadius: Theme.Radius.card - 6, style: .continuous)
                        .fill(Theme.bgField)
                )

            HStack(spacing: Theme.Spacing.sm) {
                ForEach(Array(options.enumerated()), id: \.offset) { _, option in
                    let isAllow = option.choice == .allowOnce
                    Button {
                        // Seam: wired by the chat-permission task.
                        onPermissionDecision?(promptId, isAllow)
                    } label: {
                        Text(option.label)
                            .typography(Typography.bodyMedium)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, Theme.Spacing.sm)
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(isAllow ? Theme.bgDeep : Theme.textPrimary)
                    .background(
                        RoundedRectangle(cornerRadius: Theme.Radius.card - 4, style: .continuous)
                            .fill(isAllow ? Theme.accent : Theme.bgRaised)
                    )
                    .disabled(true) // actions land in the chat-permission task
                    .accessibilityIdentifier(isAllow ? "permission-allow" : "permission-deny")
                }
            }

            Text("or say “approve” · hold mic below")
                .typography(Typography.caption)
                .foregroundStyle(Theme.textDim)
                .accessibilityIdentifier("permission-voice-hint")
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
