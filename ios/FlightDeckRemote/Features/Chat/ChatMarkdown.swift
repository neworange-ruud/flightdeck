//
//  ChatMarkdown.swift
//  FlightDeckRemote
//
//  Pure, view-agnostic Markdown parsing for agent prose. The desktop now sends
//  unparsed Markdown in agent responses, so raw `**bold**`, `` `code` ``, list
//  markers and fenced code blocks would otherwise leak into the chat verbatim.
//
//  This file is deliberately SwiftUI-free (Foundation only) so the block
//  segmentation is unit-testable without a view. It splits a message into
//  block-level structure (`MarkdownBlock`); inline emphasis inside each block
//  is left as raw Markdown and resolved at render time by `MarkdownProseView`
//  via `AttributedString(markdown:)` (Apple's well-tested inline parser).
//
//  Two entry points:
//   - `blocks(_:)` — the block model rendered as rich text in chat bubbles and
//     the activity pill's expandable prose.
//   - `plainText(_:)` — a syntax-stripped flattening used by the focus-mode
//     "Recently" condensation, whose one-line peek must not show raw markers.
//
//  Scope is the subset agents actually emit: paragraphs, ATX headings, bullet
//  and ordered lists (with indent-based nesting), fenced code blocks,
//  blockquotes, and thematic breaks. GFM tables are intentionally out of scope.
//

import Foundation

/// One block-level element of a parsed agent message. `text` fields hold the
/// block's *raw inline Markdown* (emphasis/code/links), resolved at render.
enum MarkdownBlock: Equatable {
    /// A run of prose. May contain soft line breaks (`\n`) within the block.
    case paragraph(String)
    /// An ATX heading, `level` clamped to 1…6.
    case heading(level: Int, text: String)
    /// A `-` / `*` / `+` bullet list. Items carry an indent `level` (0-based).
    case unorderedList([MarkdownListItem])
    /// An ordered list. `start` is the first item's number; items carry indent.
    case orderedList(start: Int, items: [MarkdownListItem])
    /// A fenced code block, rendered literally in monospace.
    case codeBlock(language: String?, code: String)
    /// A blockquote. Nested block structure is flattened to joined prose.
    case blockquote(String)
    /// A horizontal rule (`---`, `***`, `___`).
    case thematicBreak
}

/// One list item: its indent depth and its raw inline Markdown text.
struct MarkdownListItem: Equatable {
    /// 0-based indent depth (two spaces per level), for nested lists.
    let level: Int
    /// The item's raw inline Markdown (marker already stripped).
    let text: String
}

/// Namespace for the pure Markdown transforms.
enum ChatMarkdown {

    // MARK: - Block parsing

    /// Parse an agent message into block-level structure.
    static func blocks(_ text: String) -> [MarkdownBlock] {
        // Normalize CRLF so line handling is uniform.
        let lines = text.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n")
        var blocks: [MarkdownBlock] = []
        var paragraph: [String] = []

        func flushParagraph() {
            guard !paragraph.isEmpty else { return }
            let joined = paragraph.joined(separator: "\n")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if !joined.isEmpty { blocks.append(.paragraph(joined)) }
            paragraph = []
        }

        var i = 0
        while i < lines.count {
            let line = lines[i]

            // Fenced code block — everything until the closing fence is literal.
            if let fence = fenceMarker(line) {
                flushParagraph()
                var code: [String] = []
                i += 1
                while i < lines.count, !isClosingFence(lines[i], marker: fence.marker) {
                    code.append(lines[i])
                    i += 1
                }
                if i < lines.count { i += 1 } // consume the closing fence
                blocks.append(.codeBlock(language: fence.language,
                                         code: code.joined(separator: "\n")))
                continue
            }

            // Blank line — paragraph boundary.
            if line.trimmingCharacters(in: .whitespaces).isEmpty {
                flushParagraph()
                i += 1
                continue
            }

            // Thematic break.
            if isThematicBreak(line) {
                flushParagraph()
                blocks.append(.thematicBreak)
                i += 1
                continue
            }

            // ATX heading.
            if let heading = atxHeading(line) {
                flushParagraph()
                blocks.append(.heading(level: heading.level, text: heading.text))
                i += 1
                continue
            }

            // Blockquote — gather consecutive `>` lines.
            if isBlockquote(line) {
                flushParagraph()
                var quote: [String] = []
                while i < lines.count, isBlockquote(lines[i]) {
                    quote.append(stripBlockquoteMarker(lines[i]))
                    i += 1
                }
                let joined = quote.joined(separator: "\n")
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                blocks.append(.blockquote(joined))
                continue
            }

            // List — gather consecutive items of the same ordered-ness, plus
            // indented continuation lines folded into the preceding item.
            if listMarker(line) != nil {
                flushParagraph()
                let (block, next) = parseList(lines, from: i)
                blocks.append(block)
                i = next
                continue
            }

            // Otherwise a paragraph line.
            paragraph.append(line)
            i += 1
        }
        flushParagraph()
        return blocks
    }

    // MARK: - Plain-text flattening

    /// Strip Markdown syntax to readable plain text — used by the focus-mode
    /// condensation, whose one-line peek must not surface raw markers. Block
    /// markers (`#`, `>`, list bullets) are removed per line; inline emphasis
    /// (`**`, `_`, `` ` ``, `~~`) and link syntax `[t](url)` collapse to text.
    static func plainText(_ text: String) -> String {
        let lines = text.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n")
        var out: [String] = []
        for raw in lines {
            var line = raw
            // Drop leading block markers.
            if let heading = atxHeading(line) {
                line = heading.text
            } else if isBlockquote(line) {
                line = stripBlockquoteMarker(line).trimmingCharacters(in: .whitespaces)
            } else if let marker = listMarker(line) {
                line = String(line[marker.contentStart...])
            }
            out.append(stripInline(line))
        }
        return out.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
    }

    // MARK: - List parsing

    /// Fold consecutive list lines (from `start`) into one list block, returning
    /// the block and the index of the first line after it.
    private static func parseList(_ lines: [String], from start: Int) -> (MarkdownBlock, Int) {
        guard let first = listMarker(lines[start]) else {
            return (.paragraph(lines[start]), start + 1)
        }
        let ordered = first.number != nil
        var items: [MarkdownListItem] = []
        var i = start

        while i < lines.count {
            let line = lines[i]
            if let marker = listMarker(line), (marker.number != nil) == ordered {
                let content = String(line[marker.contentStart...])
                items.append(MarkdownListItem(level: marker.level, text: content))
                i += 1
            } else if !line.trimmingCharacters(in: .whitespaces).isEmpty,
                      leadingSpaces(line) >= 2, !items.isEmpty, listMarker(line) == nil {
                // A continuation line for the current item (indented, no marker).
                let trimmed = line.trimmingCharacters(in: .whitespaces)
                let last = items.removeLast()
                items.append(MarkdownListItem(level: last.level, text: last.text + "\n" + trimmed))
                i += 1
            } else {
                break
            }
        }

        if ordered {
            return (.orderedList(start: first.number ?? 1, items: items), i)
        }
        return (.unorderedList(items), i)
    }

    // MARK: - Line classifiers

    /// A fenced-code opening marker: the fence run (```` ``` ```` or `~~~`) plus
    /// an optional info string (language).
    private struct Fence { let marker: Character; let language: String? }

    private static func fenceMarker(_ line: String) -> Fence? {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        for marker: Character in ["`", "~"] {
            if trimmed.hasPrefix(String(repeating: marker, count: 3)) {
                let info = trimmed.drop(while: { $0 == marker })
                    .trimmingCharacters(in: .whitespaces)
                return Fence(marker: marker, language: info.isEmpty ? nil : info)
            }
        }
        return nil
    }

    private static func isClosingFence(_ line: String, marker: Character) -> Bool {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        return !trimmed.isEmpty && trimmed.allSatisfy { $0 == marker } && trimmed.count >= 3
    }

    private static func isThematicBreak(_ line: String) -> Bool {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        guard trimmed.count >= 3 else { return false }
        for marker: Character in ["-", "*", "_"] {
            if trimmed.allSatisfy({ $0 == marker }) { return true }
        }
        return false
    }

    private static func atxHeading(_ line: String) -> (level: Int, text: String)? {
        let trimmed = line.drop(while: { $0 == " " })
        let hashes = trimmed.prefix(while: { $0 == "#" }).count
        guard hashes >= 1, hashes <= 6 else { return nil }
        let after = trimmed.dropFirst(hashes)
        // Require a space (or end of line) after the hashes — `#foo` is not a heading.
        guard after.isEmpty || after.first == " " else { return nil }
        let text = after.trimmingCharacters(in: .whitespaces)
            // Drop an optional closing `###` run (`## Title ##`).
            .replacingOccurrences(of: "^#+$|\\s+#+$", with: "", options: .regularExpression)
        return (min(hashes, 6), text.trimmingCharacters(in: .whitespaces))
    }

    private static func isBlockquote(_ line: String) -> Bool {
        line.drop(while: { $0 == " " }).first == ">"
    }

    private static func stripBlockquoteMarker(_ line: String) -> String {
        var s = Substring(line.drop(while: { $0 == " " }))
        if s.first == ">" { s = s.dropFirst() }
        if s.first == " " { s = s.dropFirst() }
        return String(s)
    }

    /// A list marker: its indent `level`, the ordered `number` (nil = bullet),
    /// and the string index where the item content begins.
    private struct ListMarker { let level: Int; let number: Int?; let contentStart: String.Index }

    private static func listMarker(_ line: String) -> ListMarker? {
        let indent = leadingSpaces(line)
        let idx = line.index(line.startIndex, offsetBy: indent)
        let rest = line[idx...]
        guard let firstChar = rest.first else { return nil }

        // Bullet: `- `, `* `, `+ `.
        if firstChar == "-" || firstChar == "*" || firstChar == "+" {
            let afterMarker = rest.dropFirst()
            guard afterMarker.first == " " else { return nil }
            let contentStart = line.index(idx, offsetBy: 2)
            return ListMarker(level: indent / 2, number: nil, contentStart: contentStart)
        }

        // Ordered: `1. ` or `1) `.
        let digits = rest.prefix(while: { $0.isNumber })
        if !digits.isEmpty, digits.count <= 9 {
            let afterDigits = rest.dropFirst(digits.count)
            guard let delimiter = afterDigits.first, delimiter == "." || delimiter == ")" else { return nil }
            let afterDelimiter = afterDigits.dropFirst()
            guard afterDelimiter.first == " " else { return nil }
            let contentStart = line.index(idx, offsetBy: digits.count + 2)
            return ListMarker(level: indent / 2, number: Int(digits), contentStart: contentStart)
        }
        return nil
    }

    private static func leadingSpaces(_ line: String) -> Int {
        // Treat a tab as two spaces of indent.
        var count = 0
        for ch in line {
            if ch == " " { count += 1 }
            else if ch == "\t" { count += 2 }
            else { break }
        }
        return count
    }

    // MARK: - Inline stripping (plain-text path)

    /// Remove inline Markdown emphasis/code/link syntax, leaving readable text.
    private static func stripInline(_ text: String) -> String {
        var s = text
        // Links / images: `[label](url)` → `label`, `![alt](url)` → `alt`.
        s = s.replacingOccurrences(of: "!?\\[([^\\]]*)\\]\\([^)]*\\)",
                                   with: "$1", options: .regularExpression)
        // Emphasis / code / strikethrough markers.
        for token in ["**", "__", "~~", "*", "_", "`"] {
            s = s.replacingOccurrences(of: token, with: "")
        }
        return s
    }
}
