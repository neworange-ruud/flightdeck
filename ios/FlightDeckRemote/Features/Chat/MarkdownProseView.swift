//
//  MarkdownProseView.swift
//  FlightDeckRemote
//
//  Renders parsed agent Markdown (`ChatMarkdown.blocks`) as rich text using the
//  design-system tokens (Geist / Geist Mono, Theme colors, Theme.Spacing). The
//  desktop sends unparsed Markdown in agent responses; this view is where that
//  becomes headings, lists, code blocks, blockquotes, and inline emphasis.
//
//  Block structure is parsed here (Foundation, in `ChatMarkdown`); inline
//  emphasis inside each block is resolved with `AttributedString(markdown:)` —
//  Apple's inline parser — and then re-styled run-by-run so `` `code` `` picks
//  up Geist Mono and links take the accent. Bold/italic/strikethrough are left
//  as presentation intents for SwiftUI's `Text` to synthesize over the base
//  Geist font, so custom-font emphasis renders without bundling weight-specific
//  italic faces.
//

import SwiftUI

/// Rich-text renderer for a Markdown agent message.
struct MarkdownProseView: View {
    /// The parsed blocks. Parsing happens once in `init` (messages are short).
    private let blocks: [MarkdownBlock]
    /// Base text color — primary in a bubble, muted in an activity detail.
    private let textColor: Color

    /// Accessibility identifier for the whole prose container (preserves the
    /// `prose-agent` UI-test contract when used in the agent bubble).
    private let identifier: String?

    init(text: String, textColor: Color = Theme.textPrimary, identifier: String? = nil) {
        self.blocks = ChatMarkdown.blocks(text)
        self.textColor = textColor
        self.identifier = identifier
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.sm) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                blockView(block)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .textSelection(.enabled)
        .modifier(OptionalProseIdentifier(identifier: identifier))
    }

    // MARK: - Block dispatch

    @ViewBuilder
    private func blockView(_ block: MarkdownBlock) -> some View {
        switch block {
        case let .paragraph(text):
            inlineText(text, style: Typography.body)
                .frame(maxWidth: .infinity, alignment: .leading)

        case let .heading(level, text):
            inlineText(text, style: headingStyle(level))
                .foregroundStyle(textColor)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.top, Theme.Spacing.xxs)

        case let .unorderedList(items):
            listView(items) { _ in "•" }

        case let .orderedList(start, items):
            listView(items) { index in "\(start + index)." }

        case let .codeBlock(language, code):
            codeBlockView(language: language, code: code)

        case let .blockquote(text):
            blockquoteView(text)

        case .thematicBreak:
            Rectangle()
                .fill(Theme.textDim.opacity(0.4))
                .frame(height: 1)
                .padding(.vertical, Theme.Spacing.xs)
        }
    }

    // MARK: - Lists

    @ViewBuilder
    private func listView(_ items: [MarkdownListItem],
                          marker: @escaping (Int) -> String) -> some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.xs) {
            ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.sm) {
                    Text(marker(index))
                        .typography(Typography.body)
                        .foregroundStyle(Theme.textMuted)
                        .frame(minWidth: 16, alignment: .trailing)
                    inlineText(item.text, style: Typography.body)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .padding(.leading, CGFloat(item.level) * Theme.Spacing.lg)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Code block

    private func codeBlockView(language: String?, code: String) -> some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Text(code)
                .typography(Typography.mono)
                .foregroundStyle(textColor)
                .padding(Theme.Spacing.md)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: Theme.Radius.card - 6, style: .continuous)
                .fill(Theme.bgField)
        )
        .accessibilityIdentifier("markdown-code-block")
    }

    // MARK: - Blockquote

    private func blockquoteView(_ text: String) -> some View {
        HStack(alignment: .top, spacing: Theme.Spacing.sm) {
            RoundedRectangle(cornerRadius: 1, style: .continuous)
                .fill(Theme.accent.opacity(0.6))
                .frame(width: 3)
            inlineText(text, style: Typography.body)
                .foregroundStyle(Theme.textMuted)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .fixedSize(horizontal: false, vertical: true)
    }

    // MARK: - Inline

    /// A `Text` rendering the run's inline Markdown as a styled `AttributedString`.
    private func inlineText(_ raw: String, style: Typography.Style) -> some View {
        Text(styledInline(raw, style: style))
            .tracking(style.tracking)
            .foregroundStyle(textColor)
            .multilineTextAlignment(.leading)
    }

    /// Resolve inline Markdown to an `AttributedString`, then re-style runs:
    /// code spans → Geist Mono + accent tint, links → accent + underline.
    /// Bold/italic/strikethrough stay as presentation intents for `Text`.
    private func styledInline(_ raw: String, style: Typography.Style) -> AttributedString {
        var attributed: AttributedString
        if let parsed = try? AttributedString(
            markdown: raw,
            options: .init(allowsExtendedAttributes: true,
                           interpretedSyntax: .inlineOnlyPreservingWhitespace,
                           failurePolicy: .returnPartiallyParsedIfPossible)) {
            attributed = parsed
        } else {
            attributed = AttributedString(raw)
        }

        attributed.font = style.font

        for run in attributed.runs {
            if let intent = run.inlinePresentationIntent, intent.contains(.code) {
                attributed[run.range].font = Typography.monoSmall.font
                attributed[run.range].foregroundColor = Theme.accent
            }
            if run.link != nil {
                attributed[run.range].foregroundColor = Theme.accent
                attributed[run.range].underlineStyle = .single
            }
        }
        return attributed
    }

    private func headingStyle(_ level: Int) -> Typography.Style {
        switch level {
        case 1: Typography.title
        case 2: Typography.headline
        default: Typography.bodyMedium
        }
    }
}

/// Applies the container accessibility identifier only when one is provided, so
/// the agent bubble keeps its `prose-agent` marker while other uses stay clean.
private struct OptionalProseIdentifier: ViewModifier {
    let identifier: String?

    func body(content: Content) -> some View {
        if let identifier {
            content
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(identifier)
        } else {
            content
        }
    }
}
