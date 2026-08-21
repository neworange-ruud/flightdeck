//
//  ChatMarkdownTests.swift
//  FlightDeckRemoteTests
//
//  Unit tests for the pure Markdown block parser (`ChatMarkdown`) that turns
//  raw agent prose into `MarkdownBlock` structure, plus the `plainText`
//  syntax-stripper used by the focus-mode condensation. The renderer
//  (`MarkdownProseView`) is exercised via UI tests; these cover the pure logic.
//

import Testing
import Foundation
@testable import FlightDeckRemote

@Suite struct ChatMarkdownTests {

    // MARK: - Paragraphs

    @Test func plainTextIsASingleParagraph() {
        let blocks = ChatMarkdown.blocks("Just a plain sentence.")
        #expect(blocks == [.paragraph("Just a plain sentence.")])
    }

    @Test func blankLineSeparatesParagraphs() {
        let blocks = ChatMarkdown.blocks("First para.\n\nSecond para.")
        #expect(blocks == [.paragraph("First para."), .paragraph("Second para.")])
    }

    @Test func softLineBreaksStayWithinAParagraph() {
        let blocks = ChatMarkdown.blocks("Line one\nline two")
        #expect(blocks == [.paragraph("Line one\nline two")])
    }

    @Test func inlineEmphasisIsPreservedRawForTheRenderer() {
        // Block parsing does not touch inline syntax — the renderer resolves it.
        let blocks = ChatMarkdown.blocks("Some **bold** and `code`.")
        #expect(blocks == [.paragraph("Some **bold** and `code`.")])
    }

    // MARK: - Headings

    @Test func atxHeadingsCarryTheirLevel() {
        #expect(ChatMarkdown.blocks("# Title") == [.heading(level: 1, text: "Title")])
        #expect(ChatMarkdown.blocks("### Deep") == [.heading(level: 3, text: "Deep")])
    }

    @Test func headingLevelIsClampedAndRequiresASpace() {
        // Seven hashes is not a heading; `#foo` (no space) is not a heading.
        #expect(ChatMarkdown.blocks("####### Nope") == [.paragraph("####### Nope")])
        #expect(ChatMarkdown.blocks("#foo") == [.paragraph("#foo")])
    }

    @Test func closingHashesAreTrimmed() {
        #expect(ChatMarkdown.blocks("## Title ##") == [.heading(level: 2, text: "Title")])
    }

    // MARK: - Lists

    @Test func bulletListGroupsConsecutiveItems() {
        let blocks = ChatMarkdown.blocks("- one\n- two\n- three")
        #expect(blocks == [.unorderedList([
            MarkdownListItem(level: 0, text: "one"),
            MarkdownListItem(level: 0, text: "two"),
            MarkdownListItem(level: 0, text: "three"),
        ])])
    }

    @Test func orderedListKeepsStartNumber() {
        let blocks = ChatMarkdown.blocks("3. third\n4. fourth")
        #expect(blocks == [.orderedList(start: 3, items: [
            MarkdownListItem(level: 0, text: "third"),
            MarkdownListItem(level: 0, text: "fourth"),
        ])])
    }

    @Test func nestedBulletsCarryIndentLevel() {
        let blocks = ChatMarkdown.blocks("- top\n  - nested")
        #expect(blocks == [.unorderedList([
            MarkdownListItem(level: 0, text: "top"),
            MarkdownListItem(level: 1, text: "nested"),
        ])])
    }

    @Test func bulletAndOrderedListsAreSeparateBlocks() {
        let blocks = ChatMarkdown.blocks("- bullet\n1. number")
        #expect(blocks == [
            .unorderedList([MarkdownListItem(level: 0, text: "bullet")]),
            .orderedList(start: 1, items: [MarkdownListItem(level: 0, text: "number")]),
        ])
    }

    @Test func continuationLineFoldsIntoPrecedingItem() {
        let blocks = ChatMarkdown.blocks("- first line\n  continued")
        #expect(blocks == [.unorderedList([
            MarkdownListItem(level: 0, text: "first line\ncontinued"),
        ])])
    }

    @Test func numberFollowedByNonListTextIsNotAList() {
        // A bare "1." without content-space, or "1.5", is prose, not a list.
        #expect(ChatMarkdown.blocks("1.5 is a number") == [.paragraph("1.5 is a number")])
    }

    // MARK: - Code blocks

    @Test func fencedCodeBlockCapturesLiteralBody() {
        let blocks = ChatMarkdown.blocks("```\nlet x = 1\nlet y = 2\n```")
        #expect(blocks == [.codeBlock(language: nil, code: "let x = 1\nlet y = 2")])
    }

    @Test func fencedCodeBlockKeepsTheLanguageInfoString() {
        let blocks = ChatMarkdown.blocks("```swift\nprint(1)\n```")
        #expect(blocks == [.codeBlock(language: "swift", code: "print(1)")])
    }

    @Test func markdownInsideAFenceIsNotInterpreted() {
        let blocks = ChatMarkdown.blocks("```\n# not a heading\n- not a list\n```")
        #expect(blocks == [.codeBlock(language: nil, code: "# not a heading\n- not a list")])
    }

    @Test func tildeFenceIsSupported() {
        let blocks = ChatMarkdown.blocks("~~~\nraw\n~~~")
        #expect(blocks == [.codeBlock(language: nil, code: "raw")])
    }

    @Test func unterminatedFenceStillCapturesToEnd() {
        let blocks = ChatMarkdown.blocks("```\nno closing fence")
        #expect(blocks == [.codeBlock(language: nil, code: "no closing fence")])
    }

    // MARK: - Blockquotes & thematic breaks

    @Test func blockquoteStripsMarkersAndJoins() {
        let blocks = ChatMarkdown.blocks("> quoted line one\n> quoted line two")
        #expect(blocks == [.blockquote("quoted line one\nquoted line two")])
    }

    @Test func thematicBreakIsItsOwnBlock() {
        let blocks = ChatMarkdown.blocks("before\n\n---\n\nafter")
        #expect(blocks == [.paragraph("before"), .thematicBreak, .paragraph("after")])
    }

    // MARK: - Mixed document

    @Test func mixedDocumentParsesInOrder() {
        let text = """
        ## Summary

        Fixed the redirect. Steps:

        - thread `returnTo`
        - add a test

        ```bash
        npm test
        ```
        """
        let blocks = ChatMarkdown.blocks(text)
        #expect(blocks == [
            .heading(level: 2, text: "Summary"),
            .paragraph("Fixed the redirect. Steps:"),
            .unorderedList([
                MarkdownListItem(level: 0, text: "thread `returnTo`"),
                MarkdownListItem(level: 0, text: "add a test"),
            ]),
            .codeBlock(language: "bash", code: "npm test"),
        ])
    }

    // MARK: - plainText (focus-mode condensation)

    @Test func plainTextStripsInlineEmphasis() {
        #expect(ChatMarkdown.plainText("Some **bold** and *italic* and `code`.")
            == "Some bold and italic and code.")
    }

    @Test func plainTextStripsLinkSyntaxKeepingLabel() {
        #expect(ChatMarkdown.plainText("See [the docs](https://example.com) now.")
            == "See the docs now.")
    }

    @Test func plainTextDropsBlockMarkers() {
        #expect(ChatMarkdown.plainText("# Heading") == "Heading")
        #expect(ChatMarkdown.plainText("- a bullet") == "a bullet")
        #expect(ChatMarkdown.plainText("> a quote") == "a quote")
    }

    @Test func plainTextStripsStrikethrough() {
        #expect(ChatMarkdown.plainText("~~gone~~ kept") == "gone kept")
    }
}
