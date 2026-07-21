//
//  ChatFixtures.swift
//  FlightDeckRemote
//
//  A realistic cleaned-transcript fixture (PRD §5.3 3a): user + agent prose,
//  collapsed activity pills with expandable detail, and a pending inline
//  permission prompt. Seeded into `ChatViewModel` under the DEBUG
//  `-uitest-fixture-transcript` launch arg so the chat screen can be built and
//  UI-tested without a live desktop.
//
//  This is the Chat feature's own additive fixture seam. It intentionally does
//  *not* build a `TransportStore` (that requires the full identity/keychain/
//  socket stack) — the view-model consumes items directly, so the fixture is a
//  plain value provider.
//

import Foundation

enum ChatFixtures {

    /// `from_index` of the seeded window's first item. Non-zero so the "load
    /// earlier" affordance is exercised.
    static let fromIndex: UInt64 = 3

    private static let base: Int64 = 1_720_000_000_000

    /// A Markdown-rich agent reply (bold, inline code, a bullet list, and a
    /// fenced code block) so the rich-text rendering is exercised end-to-end.
    /// Hoisted out of the array literal to keep the fixture's type-check fast.
    private static let agent2Markdown = """
    Tests pass and the redirect now keeps the return URL. Here's **what changed**:

    - Thread `returnTo` through the `refresh()` boundary
    - Fall back to `/` when no return URL is saved
    - Add a regression test in `redirect.test.ts`

    Verify with:

    ```bash
    npm test -- redirect
    ```

    Before I rebuild I want to clear the stale build output.
    """

    /// The main seeded transcript window (positions 0…5).
    static func items() -> [Wire.TranscriptItem] {
        [
            .userMessage(
                itemId: Wire.ItemId("fx-user-1"),
                text: "Can you fix the login redirect? It loops back to /login after a token refresh.",
                atMs: base),
            .agentMessage(
                itemId: Wire.ItemId("fx-agent-1"),
                text: "Looking at the auth flow. The refresh path drops the saved return URL, so the guard bounces you back to /login. I'll thread the return URL through the refresh.",
                atMs: base + 30_000),
            .activity(
                itemId: Wire.ItemId("fx-edit-1"),
                summary: "Edited auth.ts +18 −4",
                detail: """
                @@ -42,6 +42,20 @@ async function refresh() {
                -  const token = await rotate();
                -  return token;
                +  const token = await rotate();
                +  // preserve the return URL across the refresh boundary
                +  const returnTo = session.returnTo ?? "/";
                +  session.returnTo = returnTo;
                +  return { token, returnTo };
                """,
                body: nil,
                kind: .edit,
                atMs: base + 45_000),
            .activity(
                itemId: Wire.ItemId("fx-test-1"),
                summary: "Ran npm test · 42 passed",
                detail: """
                PASS  src/auth.test.ts
                PASS  src/redirect.test.ts
                Tests: 42 passed, 42 total
                Time:  3.11 s
                """,
                body: nil,
                kind: .test,
                atMs: base + 20 * 60 * 1000),
            .agentMessage(
                itemId: Wire.ItemId("fx-agent-2"),
                text: agent2Markdown,
                atMs: base + 20 * 60 * 1000 + 5_000),
            .permissionPrompt(
                itemId: Wire.ItemId("fx-perm-1"),
                promptId: Wire.PromptId("fx-prompt-1"),
                kind: .permission,
                command: "rm -rf dist/",
                options: [
                    Wire.PermissionOption(index: 0, choice: .allowOnce, label: "Allow once"),
                    Wire.PermissionOption(index: 1, choice: .deny, label: "Deny"),
                ],
                allowFreeText: false,
                multiSelect: false,
                atMs: base + 20 * 60 * 1000 + 8_000),
        ]
    }

    /// The slice revealed when "load earlier" is tapped (positions before the
    /// main window). Prepended by `ChatViewModel.loadEarlier` in fixture mode.
    static func earlierItems() -> [Wire.TranscriptItem] {
        [
            .agentMessage(
                itemId: Wire.ItemId("fx-earlier-1"),
                text: "Cloned the worktree and installed dependencies.",
                atMs: base - 120_000),
            .activity(
                itemId: Wire.ItemId("fx-earlier-cmd"),
                summary: "Ran npm install · 312 packages",
                detail: "added 312 packages in 6s",
                body: nil,
                kind: .command,
                atMs: base - 90_000),
        ]
    }
}
