# FlightDeck

**FlightDeck** is a macOS-first terminal UI for orchestrating multiple local AI
coding agents working in parallel on the same Git project. You run it from inside
a Git repository; it creates isolated Git **worktrees** under `.flightdeck/`,
launches a selected AI coding agent inside each one, lets you switch between
parallel agent sessions, open extra child shells in each worktree, tracks Git and
agent status, and helps push branches for GitHub pull-request workflows.

```text
1 Agent Tab = 1 Worktree = 1 Branch = 1 Primary Agent Process
```

## Quick start

```bash
cd /path/to/your/git/repo
flightdeck
```

On first run FlightDeck auto-initializes (no `flightdeck init` needed):

```text
your-repo/
  .flightdeck/
    config.toml        # committed, human-editable
    state.json         # ignored (runtime state)
    worktrees/         # ignored (managed worktrees)
```

It also appends two entries to your `.gitignore` (append-only — existing content
is preserved):

```gitignore
.flightdeck/state.json
.flightdeck/worktrees/
```

Configured agents live in `.flightdeck/config.toml` (OpenCode is the default;
Claude Code and Codex CLI are pre-configured). Agent definitions are
config-driven — edit the `command`, `args`, and `status_patterns` there. When
you create a tab you pick which agent it runs from a quick menu, so you can mix
agents (e.g. Claude Code in one tab, OpenCode in another); the menu is skipped
when only one agent is configured.

## The Git ownership boundary (why FlightDeck is safe)

FlightDeck deliberately **never mutates commit history**. This boundary is
enforced *by construction*: the `GitExecutor` trait does not even expose a
history-rewriting operation, and a guard test (`tests/guards.rs`) fails the build
if a forbidden git subcommand ever appears in the source.

FlightDeck **may**: detect the repo root / base branch / dirty state, create
`.flightdeck/`, update `.gitignore` (append-only), create & attach branches,
create & recover worktrees, push branches *after explicit confirmation*, remove
managed worktrees (a clean worktree is removed immediately; a worktree with
uncommitted changes is removed only after you confirm discarding them), and
perform a guarded local merge-back only when strict preconditions hold.

FlightDeck **must not** (and cannot): stage files, create/amend/squash commits,
rebase, rewrite history, force-push, create GitHub PRs, or auto-resolve merge
conflicts. You (or your agent) make the commits; FlightDeck shows you a GitHub PR
**compare URL** after a push so you create the PR yourself.

## Keyboard model

FlightDeck is keyboard-first with two modes. The **command palette** (`Ctrl-g`)
is the dependable fallback because terminal shortcut collisions are unavoidable.

- **Terminal mode** — keystrokes go to the active terminal. `Esc` leaves to app
  mode; `Ctrl-g` opens the palette.
- **App mode** — keystrokes control FlightDeck. `Enter` focuses the terminal;
  `?` shows help.

Common shortcuts: `Ctrl-g` palette · `Ctrl-q` quit (or palette → *Quit*) ·
`Ctrl-n` new tab · `Ctrl-p` push · `Ctrl-f` finish/local-merge · `Ctrl-k` close
tab · `Alt-↑/↓` previous/next **agent tab** · `Alt-1..9` jump to agent tab ·
`Ctrl-t` new child terminal · `Ctrl-w` close child · `Alt-←/→` cycle the
**terminal tabs** (agent + shells) · `Ctrl-s` set manual status · `Ctrl-r`
restart agent. The `Alt`-modified navigation works in **both** modes, so you can
switch tabs without leaving terminal focus; in App mode the bare arrow keys also
work (handy because some terminals intercept `Alt`+arrows). The full table is in
the in-app help (`?`).

**Mouse**: click an Agent Tab in the sidebar to select it, or a child-terminal
tab (`agent | shell 1 | …`) to switch terminals.

## Screen layout

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ ░░░▒▒▒▓▓▓██████   F · L · I · G · H · T · D · E · C · K   ██████▓▓▓▒▒▒░░░ │  logo header
├──────────────────────────────────────────────────────────────────────────┤  divider
│ Agents          │ agent | shell 1 | shell 2                                │  terminal tabs
│  ▸ fix-login    │                                                          │
│    add-tests    │            active terminal (agent or shell)              │
│                 │                                                          │
│                 ├──────────────────────────────────────────────────────────┤
│                 │ ⎇ flightdeck/fix-login │ +3 ~2 -1 (6 files) │ ↑0 ↓0 │ …  │  git info bar
│                 ├──────────────────────────────────────────────────────────┤
│                 │ MODE: TERMINAL | Esc: app commands | Ctrl-g: palette     │  status bar
└─────────────────┴──────────────────────────────────────────────────────────┘
```

- **Logo header + divider** — a full-width branded title row. The logo centers
  itself and shrinks to a tighter variant on narrow terminals.
- **Agents sidebar** — the list of Agent Tabs (each shows agent, process/status,
  and git indicators), under a centered **Agents** heading.
- **Git info bar** — a one-line summary for the selected tab's worktree: branch,
  changed-file counts (`+added ~modified -deleted (N files)`, or `clean`),
  ahead/behind vs upstream (or `no upstream` until the branch is pushed), base
  drift, and the base branch. It reflects the tab's worktree regardless of
  whether the agent or a shell is focused.

## Agent status indicators

Every Agent Tab shows its agent's live status — a colour-coded **dot** next to
the tab name plus a `proc: <process> | <status>` line in the sidebar. The
minimum signal is **idle vs in progress**, and it works for **all** agents
(OpenCode, Claude Code, Codex CLI) with **zero setup**:

- 🟢 **working** — the agent is actively producing output (in progress).
- ⚪ **idle** — the process is up but quiet (finished its turn / waiting on you).
- 🔵 manual override (`Ctrl-s`) — shown in cyan, never hides the process state.

This baseline is purely **output-activity based**: FlightDeck watches each
agent's terminal and flips a tab to `idle` once output has been silent past a
short threshold, back to `working` the moment it resumes. Nothing is installed
into the agents.

### Optional: precise status (waiting / needs-attention / completed)

For exact `waiting` / `completed` signals (e.g. light up the moment an agent
asks for confirmation, rather than after the silence timeout), run:

```bash
flightdeck setup-status
```

This writes ready-to-use, self-contained hook/plugin artifacts to
`.flightdeck/integrations/` and adds `.flightdeck/agent-status` to `.gitignore`.
Each agent's hook writes a keyword (`working`/`idle`/`waiting`) to
`<worktree>/.flightdeck/agent-status`, which FlightDeck polls; a fresh hook
signal is shown immediately yet is still superseded by later output activity, so
agents that only signal turn-completion (Codex) still behave correctly. The
hooks are gated on `.flightdeck/` existing, so they're a no-op outside FlightDeck
worktrees. Wire them per the generated `README.md`:

- **Claude Code** — merge `claude-code.settings.json` into `~/.claude/settings.json`
  (`UserPromptSubmit`→working, `Stop`/`StopFailure`→idle, `Notification`→waiting).
- **Codex CLI** — append `codex-config.toml` to `~/.codex/config.toml`
  (`UserPromptSubmit`→working, `Stop`→idle; `notify` fallback for older builds).
- **OpenCode** — copy `opencode-flightdeck.js` to `~/.config/opencode/plugin/`
  (`session.idle`→idle, message activity→working, permission prompt→waiting).

## Architecture

Business logic is separated from the TUI and fully testable. Git, the
filesystem, and PTYs sit behind traits (`src/contracts/traits.rs`) so every logic
module is unit-tested against fakes (`src/testing/`). The TUI dispatches
`Command`s into the headless app core, which calls services — the TUI never runs
git/fs/pty itself.

```text
src/
  contracts/   shared types, traits, errors, trivial real impls
  testing/     FakeGit / FakeFs / FakePty / FakeClock
  config/      load/serialize config.toml, defaults, first-run init
  fs/          relative/absolute paths, append-only .gitignore updater
  git/         real GitExecutor + branch/worktree/status/remote workflow logic
  agents/      registry, PATH validation, output→status classification
  persistence/ state.json load/save + worktree recovery
  terminal/    portable-pty backend + session model (primary + child shells)
  app/         headless state, commands, dispatch, input modes
  tui/         ratatui layout, render, key mapping, command palette
  lib.rs       run(): startup → recovery → event loop → clean teardown
tests/
  integration/ real temp-git-repo workflow tests
  guards.rs    SPECS §2 (naming) and §5 (no history rewriting) guards
```

## Development

Requires a Rust toolchain (stable) and `git`.

```bash
cargo build                              # debug build
cargo build --release                    # release build
cargo test                               # unit + integration + guard tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo run                                # run inside a git repo
```

## Manual smoke test (human, requires a real terminal)

Automated tests cannot drive a real attached terminal/PTY end-to-end. After
changes, run this checklist by hand from inside a scratch Git repo:

1. `cargo run` inside a git repo → FlightDeck starts; `.flightdeck/` is created
   and `.gitignore` gains the two entries (notice shown). A branded logo header
   and divider span the top of the screen.
2. **New tab** (`Ctrl-n`) → pick an agent from the menu (e.g. Claude Code) →
   enter a name → the `flightdeck/<slug>` branch + worktree are created and the
   chosen agent launches in the primary terminal.
3. **Missing agent**: edit `config.toml` to a bogus `command`, create a tab →
   creation fails with a clear message and **no** branch/worktree is created.
4. **Child terminal** (`Ctrl-t`) → a shell opens in the same worktree; switch
   with `Alt-←/→` (or click its tab); close with `Ctrl-w`. The agent's and each
   shell's live output renders in the main pane.
5. **Git info bar**: the line above the status bar shows the selected tab's
   branch, change counts, ahead/behind, and base — and stays correct whether the
   agent or a shell tab is focused.
6. **Git status** (palette → *Show Git Status*) → branch, base, drift, dirty,
   ahead/behind, worktree path.
7. **Push** (`Ctrl-p`) → with uncommitted changes you are warned; after a commit,
   confirm push → a GitHub PR compare URL is shown (if origin is GitHub).
8. **Manual status** (`Ctrl-s`) → set/clear; the process state stays visible.
9. **Abandon worktree** (palette → *Abandon Worktree*) → a clean worktree is
   removed at once; with uncommitted changes you are asked to confirm discarding
   them before it is force-removed.
10. **Close tab** (`Ctrl-k`) → the option menu defaults to *Send Ctrl-C to
    primary*; it never auto-escalates to force-kill.
11. **Quit**: `Ctrl-q`, or open the palette (`Ctrl-g`) and choose *Quit* — both
    exit cleanly.
12. **Recovery / resume**: quit (`Ctrl-q`), relaunch → tabs are reconstructed
    from disk and their agents are restarted automatically when their worktree
    still exists (you can also restart any tab manually with `Ctrl-r`).
13. **No orphans**: after quitting, confirm no agent/shell child processes are
    left running (e.g. `pgrep -fl opencode`).

## Status

MVP. Out of scope for now: Windows, multiple repos per process, live terminal
resurrection after restart, automatic commits/PRs, GitHub API integration, an
agent plugin system, initial-prompt injection, a diff viewer, split panes, and
multiple base branches (see SPECS §28).
