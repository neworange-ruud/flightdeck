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
config-driven — edit the `command`, `args`, and `status_patterns` there.

## The Git ownership boundary (why FlightDeck is safe)

FlightDeck deliberately **never mutates commit history**. This boundary is
enforced *by construction*: the `GitExecutor` trait does not even expose a
history-rewriting operation, and a guard test (`tests/guards.rs`) fails the build
if a forbidden git subcommand ever appears in the source.

FlightDeck **may**: detect the repo root / base branch / dirty state, create
`.flightdeck/`, update `.gitignore` (append-only), create & attach branches,
create & recover worktrees, push branches *after explicit confirmation*, remove
managed worktrees when safe, and perform a guarded local merge-back only when
strict preconditions hold.

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
tab · `↑/↓` (or `Alt-↑/↓`) previous/next **agent tab** · `Alt-1..9` jump to agent
tab · `Ctrl-t` new child terminal · `Ctrl-w` close child · `←/→` (or `Alt-←/→`)
cycle the **terminal tabs** (agent + shells) · `Ctrl-s` set manual status ·
`Ctrl-r` restart agent. In App mode the bare arrow keys work too (some terminals
intercept `Alt`+arrows). The full table is in the in-app help (`?`).

**Mouse**: click an Agent Tab in the sidebar to select it, or a child-terminal
tab (`agent | shell 1 | …`) to switch terminals.

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
   and `.gitignore` gains the two entries (notice shown).
2. **New tab** (`Ctrl-n`) → enter a name → the `flightdeck/<slug>` branch +
   worktree are created and the default agent launches in the primary terminal.
3. **Missing agent**: edit `config.toml` to a bogus `command`, create a tab →
   creation fails with a clear message and **no** branch/worktree is created.
4. **Child terminal** (`Ctrl-t`) → a shell opens in the same worktree; switch
   with `Alt-←/→` (or click its tab); close with `Ctrl-w`. The agent's and each
   shell's live output renders in the main pane.
5. **Git status** (palette → *Show Git Status*) → branch, base, drift, dirty,
   ahead/behind, worktree path.
6. **Push** (`Ctrl-p`) → with uncommitted changes you are warned; after a commit,
   confirm push → a GitHub PR compare URL is shown (if origin is GitHub).
7. **Manual status** (`Ctrl-s`) → set/clear; the process state stays visible.
8. **Close tab** (`Ctrl-k`) → the option menu defaults to *Send Ctrl-C to
   primary*; it never auto-escalates to force-kill.
8b. **Quit**: `Ctrl-q`, or open the palette (`Ctrl-g`) and choose *Quit* — both
    exit cleanly.
9. **Recovery / resume**: quit (`Ctrl-q`), relaunch → tabs are reconstructed
   from disk and their agents are restarted automatically when their worktree
   still exists (you can also restart any tab manually with `Ctrl-r`).
10. **No orphans**: after quitting, confirm no agent/shell child processes are
    left running (e.g. `pgrep -fl opencode`).

## Status

MVP. Out of scope for now: Windows, multiple repos per process, live terminal
resurrection after restart, automatic commits/PRs, GitHub API integration, an
agent plugin system, initial-prompt injection, a diff viewer, split panes, and
multiple base branches (see SPECS §28).
