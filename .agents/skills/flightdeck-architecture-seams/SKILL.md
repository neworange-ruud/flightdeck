---
name: flightdeck-architecture-seams
description: Use when adding or changing FlightDeck logic that touches git, the filesystem, PTYs, the clock, containers, or the TUI's side effects.
---

# FlightDeck architecture seams & git safety boundary

FlightDeck's trustworthiness rests on two rules: all side effects go through
traits (so logic is testable against fakes), and git history is never rewritten
outside two guarded carve-outs. Both are enforced by construction — respect them.

## Everything dangerous sits behind a trait

Side effects go through the traits in `src/contracts/traits.rs`:
`GitExecutor`, `FileSystem`, `PtyBackend`/`PtySession`, `Notifier`, `Clock`,
`ContainerRuntime`. Logic depends on the trait, not the concrete impl.

- The **TUI never** executes git, mutates files, or manages PTYs directly. It
  dispatches commands into the app-core services (SPECS §27).
- Test logic against the fakes in `src/testing/` (`FakeGit`, `FakeFs`,
  `FakePty`, `FakeClock`, `FakeContainerRuntime`) — no real terminal/git needed.
- Need a new side effect? Add a method to the right trait **and** its fake;
  don't reach around the seam.
- A new side effect with **OS-specific** behavior (a `Notifier` backend,
  clipboard, PTY) is also a parity concern — consult
  flightdeck-cross-platform-parity alongside this skill.

## The git safety boundary (SPECS §5)

`GitExecutor` exposes **no unguarded history-rewriting op** — no stage, commit,
amend, squash, cherry-pick, or PR creation. Do not add one.

Exactly two sanctioned rebase ops exist, each user-initiated, precondition-
checked, and conflict-aborting (`git rebase --abort`, worktree left untouched):

- `rebase_onto` — Rebase Worktree, confirmation-gated (§5.1).
- `pull_base` — Pull Base on the base folder, clean-tree required (§5.2).

FlightDeck never stashes, discards, resolves conflicts, or force-pushes on the
user's behalf. *The fastest way to make this tool untrustworthy is to let it
mutate commit history.*

When a request asks for a forbidden op (squash, amend, cherry-pick, auto-rebase,
force-push, commit): **refuse and explain**, don't add an escape hatch. Say that
history-rewriting is intentionally outside FlightDeck's remit and the user does
it themselves in their own shell. The two carve-outs above are the only
exceptions, and only through their guarded, confirmed workflows.

## Test the refusal paths

Dangerous operations must have tests for the **refusal** path (dirty tree, wrong
branch checked out, missing preconditions), not only the success path
(SPECS §26). Guarded ops verify the target worktree has the expected branch
before acting.

## Feature-behind-config

New user-visible behavior is gated by config with an explicit default — see
flightdeck-config-conventions.
