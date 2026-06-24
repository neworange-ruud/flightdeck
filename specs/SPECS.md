# FlightDeck MVP Implementation Specification v0.1

## 1. Product Summary

**FlightDeck** is a macOS-first terminal UI application for orchestrating multiple local AI coding agents working in parallel on the same Git project.

The user starts FlightDeck from inside a Git repository:

```bash
flightdeck
```

FlightDeck creates isolated Git worktrees under the project’s `.flightdeck/` directory, launches a selected AI coding agent inside each worktree, lets the user switch between parallel agent sessions, opens additional child terminals inside each worktree, tracks Git and agent status, and helps push branches for GitHub pull request workflows.

MVP target:

- macOS first
- Linux likely compatible if the PTY implementation supports it
- Windows out of scope for MVP, but architecture should not make future Windows support impossible

---

## 2. MVP Product Name and Naming Conventions

Product name:

```text
FlightDeck
```

Binary name:

```text
flightdeck
```

Project metadata folder:

```text
.flightdeck/
```

Generated branch prefix:

```text
flightdeck/
```

Example generated branches:

```text
flightdeck/add-auth-tests
flightdeck/refactor-payment-client
flightdeck/fix-dashboard-loading
```

The old placeholder name “Agent Orchestrator” must not appear in code, docs, config, UI, folders, or branches.

---

## 3. Core MVP Model

FlightDeck runs against exactly one Git project per process.

Users who want to use FlightDeck in multiple repositories should open multiple terminal windows and run separate FlightDeck instances.

Core invariant:

```text
1 Agent Tab = 1 Worktree = 1 Branch = 1 Primary Agent Process
```

Each Agent Tab contains:

- One Git branch
- One Git worktree
- One primary terminal running the selected coding agent
- Zero or more child terminals running plain shells in the same worktree

Child terminals are not modeled as agents, even if the user manually launches another agent inside them.

---

## 4. Primary MVP Workflow

1. User runs `flightdeck` inside a Git repository.
2. FlightDeck detects the Git root.
3. FlightDeck auto-initializes `.flightdeck/` if needed.
4. User creates a new Agent Tab.
5. FlightDeck prompts for:
   - Tab/task name
   - Agent selection, defaulting to OpenCode
6. FlightDeck generates a branch name using `flightdeck/<task-slug>`.
7. FlightDeck creates or attaches to the branch.
8. FlightDeck creates or attaches to the worktree.
9. FlightDeck immediately starts the selected agent.
10. User interacts with the agent in the primary terminal.
11. User may create child terminals inside the same worktree.
12. User or agent creates commits manually.
13. FlightDeck may push the branch after explicit confirmation.
14. FlightDeck shows a GitHub PR compare URL.
15. User creates the PR manually.

FlightDeck does **not** create commits or pull requests in MVP.

---

## 5. Git Ownership Boundary

FlightDeck may:

- Detect repository root
- Detect base branch
- Detect dirty state
- Create `.flightdeck/`
- Update `.gitignore`
- Create branches
- Attach to existing branches
- Create worktrees
- Recover worktrees
- Push branches after confirmation
- Remove managed worktrees when safe
- Delete managed local branches when safe and explicitly confirmed
- Perform local merge-back only under strict conditions

FlightDeck must not:

- Stage files
- Create commits
- Amend commits
- Squash commits
- Rebase automatically
- Rewrite history
- Create GitHub PRs
- Resolve merge conflicts automatically

This boundary matters. The fastest way to make this tool untrustworthy is to let it mutate commit history.

---

## 6. Project Layout

FlightDeck stores project-specific files here:

```text
project-root/
  .flightdeck/
    config.toml
    state.json
    worktrees/
      add-auth-tests/
      refactor-payment-client/
```

Committed:

```text
.flightdeck/config.toml
```

Ignored:

```text
.flightdeck/state.json
.flightdeck/worktrees/
```

Required `.gitignore` entries:

```gitignore
.flightdeck/state.json
.flightdeck/worktrees/
```

On first run, FlightDeck may update `.gitignore` automatically.

Rules:

- Append missing lines only.
- Preserve existing `.gitignore` contents.
- Do not rewrite, sort, or remove existing rules.
- Show a short notice after updating `.gitignore`.

---

## 7. First-Run Initialization

FlightDeck auto-initializes on first run.

No explicit `flightdeck init` is required for MVP.

Startup flow:

1. Locate Git repository root.
2. Validate Git is available.
3. Detect current/base branch.
4. Create `.flightdeck/` if missing.
5. Create `.flightdeck/config.toml` if missing.
6. Create `.flightdeck/state.json` if missing.
7. Create `.flightdeck/worktrees/` if missing.
8. Append required `.gitignore` entries if missing.
9. Load project state.
10. Recover known worktrees.

Future CLI support for `flightdeck init` is acceptable, but not required for MVP.

---

## 8. Config File

Config file:

```text
.flightdeck/config.toml
```

The config is committed and human-readable.

No in-TUI settings editor for MVP.

Example MVP config:

```toml
[project]
name = "my-project"
default_base_branch = "main"

[worktrees]
root = ".flightdeck/worktrees"

[git]
default_remote = "origin"
primary_host = "github"
branch_prefix = "flightdeck/"

[ui]
agent_tab_position = "left"
default_agent = "opencode"

[agents.opencode]
display_name = "OpenCode"
command = "opencode"
args = []

[agents.claude]
display_name = "Claude Code"
command = "claude"
args = []

[agents.codex]
display_name = "Codex CLI"
command = "codex"
args = []

[agents.opencode.status_patterns]
waiting = ["Proceed?", "Confirm", "Approve", "Do you want to"]
completed = ["Done", "Complete", "Task complete"]
error = ["Error", "Failed"]
```

Supported initial agents:

- OpenCode
- Claude Code
- Codex CLI

Agent definitions must be config-driven.

---

## 9. Runtime State

Runtime state file:

```text
.flightdeck/state.json
```

This file is ignored and not committed.

State should store relative paths for portability. Absolute paths may be computed at runtime.

Top-level state:

```json
{
  "version": 1,
  "project_root_relative": ".",
  "base_branch": "main",
  "tabs": []
}
```

Each Agent Tab state:

```json
{
  "id": "stable-id",
  "name": "Add auth tests",
  "slug": "add-auth-tests",
  "agent": "opencode",
  "branch": "flightdeck/add-auth-tests",
  "worktree_path_relative": ".flightdeck/worktrees/add-auth-tests",
  "base_branch": "main",
  "base_commit_sha": "abc123",
  "created_at": "ISO-8601",
  "attached_existing_branch": false,
  "recovered": false,
  "last_known_status": "unknown",
  "manual_status": null
}
```

Live PTY sessions are not persisted.

After restart, FlightDeck restores metadata only.

---

## 10. Recovery

FlightDeck must recover Agent Tabs.

On startup:

1. Load `state.json`.
2. Validate stored tabs.
3. Scan `.flightdeck/worktrees/`.
4. Detect valid Git worktrees.
5. Reconstruct missing tabs where possible.
6. Mark reconstructed tabs as recovered.
7. Do not relaunch agents automatically after restart.

Recovered tabs should offer:

- Restart agent
- Open shell
- Push branch
- Local merge if safe
- Close tab
- Remove stale state entry

Important distinction:

- Creating or attaching to a branch during normal new-tab flow starts the selected agent immediately.
- Recovering after restart does not start agents automatically.

---

## 11. Branch Creation and Attachment

Generated branches must use:

```text
flightdeck/<task-slug>
```

If the generated branch does not exist:

1. Create branch from configured base branch.
2. Store current base commit SHA.
3. Create worktree under `.flightdeck/worktrees/<task-slug>`.
4. Launch selected agent.

If the generated branch already exists:

1. Inform the user clearly.
2. Attach to the existing branch.
3. Create or reuse a managed worktree.
4. Mark tab as attached to existing branch.
5. Launch selected agent immediately.

FlightDeck must not silently attach to existing branches.

If the branch is already checked out:

- If checked out in `.flightdeck/worktrees/`, reuse that worktree.
- If checked out elsewhere, refuse and show the existing worktree path.
- Do not force checkout in MVP.

Generated branches must use `flightdeck/`.

Manual attach to non-prefixed branches is not part of the core MVP. If later added, such branches should be marked as external.

---

## 12. Base Branch and Drift Tracking

MVP supports one base branch per project.

Each Agent Tab stores:

- Base branch name
- Base commit SHA at creation time

FlightDeck should show drift information:

```text
Base moved: 12 commits ahead since tab creation
```

This should be computed by comparing the stored base commit SHA to the current base branch.

This matters because long-running agent branches can become stale quickly.

---

## 13. Dirty Base Repository Behavior

If the base repository has uncommitted changes at startup:

- Warn the user.
- Continue startup.
- Allow Agent Tab creation.
- Allow push workflow.
- Disable local merge-back.
- Show persistent warning.

Example warning:

```text
Base repo dirty: local merge disabled
```

Do not block parallel work just because the base repo is dirty.

---

## 14. Push Workflow

FlightDeck may push branches, but only after explicit user confirmation.

Push flow:

1. Check worktree status.
2. If uncommitted changes exist, warn:
   ```text
   This worktree has uncommitted changes. Push will only include committed changes.
   ```
3. Offer:
   - Push committed changes
   - Open terminal to commit manually
   - Cancel
4. If confirmed, run push.
5. Show success/failure.
6. If GitHub remote is detected, show PR compare URL.

GitHub remote formats:

```text
git@github.com:owner/repo.git
https://github.com/owner/repo.git
```

PR URL format:

```text
https://github.com/<owner>/<repo>/compare/<base>...<branch>
```

---

## 15. Local Merge-Back Workflow

Local merge-back is secondary and guarded.

Before local merge-back, FlightDeck must require:

- Base worktree is clean
- Agent worktree is clean
- Base branch exists
- Agent branch exists
- No running primary agent unless explicitly stopped
- User explicitly confirms merge
- FlightDeck knows the tab’s base branch and base commit SHA

If base repo is dirty:

```text
Base worktree has uncommitted changes. Local merge is disabled.
Recommended action: push this branch and create a PR instead.
```

MVP should not attempt conflict resolution. If merge conflicts occur, FlightDeck should stop and explain that manual Git intervention is required.

---

## 16. Agent Command Validation

When creating a new Agent Tab, FlightDeck must verify that the selected agent command exists in `PATH`.

If missing:

- Fail tab creation.
- Do not create the branch.
- Do not create the worktree.
- Show a clear message.

Example:

```text
OpenCode command not found: opencode

Fix options:
- Install OpenCode
- Add it to PATH
- Edit .flightdeck/config.toml and set the correct command
```

This validation should happen before mutating Git state.

---

## 17. Agent Startup

New Agent Tab creation starts the selected agent immediately.

Flow:

1. Validate agent command exists.
2. Prompt for task/tab name.
3. Generate slug.
4. Generate branch name.
5. Create or attach branch.
6. Create or attach worktree.
7. Create primary terminal.
8. Launch agent command.
9. Focus primary terminal.

No initial prompt is passed to the agent in MVP.

The task description is a label only.

---

## 18. Tab Naming

Tab names are independent from branch names after creation.

The user may rename a tab without renaming:

- Branch
- Worktree folder
- Slug
- Stored base metadata

Example:

```text
Original branch: flightdeck/add-auth-tests
Original tab: Add auth tests
Renamed tab: Add auth tests - blocked
```

This avoids risky Git renames and keeps UI labeling flexible.

---

## 19. Terminal Model

Each Agent Tab contains:

- Primary agent terminal
- Optional child shell terminals

Child terminal UI:

```text
agent | shell 1 | shell 2 | shell 3
```

Use a horizontal child terminal tab bar inside the main pane.

Reason:

- Lower complexity than split panes.
- Less layout pressure.
- Easier keyboard navigation.
- Split panes can be a post-MVP feature.

Child terminals:

- Run plain shells.
- Start inside the worktree directory.
- May continue after the primary agent exits.
- May be closed independently.
- Are not persisted after app restart.

---

## 20. Main Layout

Default layout:

```text
┌──────────────────────┬──────────────────────────────────────────┐
│ Agent Tabs Sidebar   │ Child Terminal Tabs                      │
│                      ├──────────────────────────────────────────┤
│ ▸ Auth tests         │                                          │
│   OpenCode waiting   │                                          │
│   dirty, +2          │          Active Terminal View             │
│                      │                                          │
│   Payment refactor   │                                          │
│   Claude running     │                                          │
│                      ├──────────────────────────────────────────┤
│                      │ Git/status/action bar                    │
└──────────────────────┴──────────────────────────────────────────┘
```

Left sidebar shows Agent Tabs.

Each Agent Tab row should show:

- Tab name
- Agent name
- Interpreted status
- Process state
- Dirty indicator
- Ahead/behind indicator
- Base drift indicator
- Existing/recovered marker where relevant

Main pane shows:

- Child terminal tab bar
- Active terminal viewport
- Lightweight status/action bar

---

## 21. Git Status Panel

MVP includes a lightweight Git status panel.

It should not be a full diff viewer.

It should show, for the active Agent Tab:

- Branch name
- Base branch
- Base drift
- Dirty/clean state
- Ahead/behind relative to upstream, if known
- Whether upstream exists
- Last push status, if known
- Worktree path
- PR compare URL after push, if available

Access:

- Command palette action: `Show Git Status`
- Optional visible status bar summary

No file diff view in MVP.

---

## 22. Interaction Model

FlightDeck is keyboard-first.

Primary interaction model:

```text
Command palette first
Visible status/actions second
Keyboard shortcuts always available
```

The command palette is the reliable fallback because terminal shortcut collisions are unavoidable.

Required command palette actions:

```text
New Agent Tab
Rename Agent Tab
Close Agent Tab
Push Branch
Finish / Local Merge
Abandon Worktree
New Child Terminal
Close Child Terminal
Switch Agent Tab
Switch Child Terminal
Set Manual Status
Restart Agent
Open Shell
Show Git Status
Show Help
Quit
```

---

## 23. Keyboard Modes

FlightDeck needs two input modes.

### Terminal Focus Mode

Most keystrokes go to the active terminal.

Status bar:

```text
MODE: TERMINAL | Esc: app commands | Ctrl-g: command palette
```

### App Command Mode

Keystrokes control FlightDeck.

Status bar:

```text
MODE: APP | Enter: focus terminal | Ctrl-g: command palette | ?: help
```

Required shortcuts:

```text
Global
  Ctrl-g          Command palette
  Ctrl-q          Quit / close app
  Ctrl-n          New Agent Tab
  Ctrl-p          Push current branch
  Ctrl-f          Finish current Agent Tab
  Ctrl-k          Close current Agent Tab
  ?               Help / keybindings

Agent Tab Navigation
  Alt-Left        Previous Agent Tab
  Alt-Right       Next Agent Tab
  Alt-1..Alt-9    Jump to Agent Tab by index

Child Terminal Navigation
  Ctrl-t          New child terminal
  Ctrl-w          Close active child terminal
  Ctrl-Tab        Next child terminal
  Ctrl-Shift-Tab  Previous child terminal

Focus
  Esc             Leave terminal input focus / focus app chrome
  Enter           Focus active terminal

Status
  Ctrl-s          Set manual status
  Ctrl-r          Restart primary agent in recovered/stopped tab
```

Shortcut conflicts are expected. The command palette must be the dependable path.

---

## 24. Agent Status Detection

MVP combines:

1. Process state
2. Output pattern matching
3. Manual status override
4. Future plugin hook architecture, not implemented yet

Statuses:

```text
Starting
Running
Waiting for input
Needs attention
Completed / idle
Failed / exited
Stopped
Session lost
Recovered
Unknown
```

Manual overrides:

```text
In progress
Waiting
Blocked
Done
Clear override
```

UI should display both:

```text
OpenCode | process: running | status: waiting
```

Manual status override takes visual priority but should not hide process state.

---

## 25. Close Behavior

When closing an Agent Tab with running processes, offer:

```text
Send Ctrl-C to primary agent
Send Ctrl-C to all terminals in this tab
Force terminate process tree
Close only if all processes have stopped
Cancel
```

Default suggested action:

```text
Send Ctrl-C to primary agent
```

Do not escalate to force-kill automatically.

If the process remains alive after Ctrl-C, ask again.

FlightDeck should not intentionally leave orphaned child processes in MVP.

---

## 26. Testing Requirements

The MVP must be designed for extensive automated testing from day one.

This is not optional. Git/worktree tooling can destroy trust quickly if regression testing is weak.

### Testing Principles

- Business logic must be separated from TUI rendering.
- Git command execution must be abstracted behind interfaces.
- Filesystem operations must be abstracted where useful.
- PTY/session behavior must be wrapped behind testable boundaries.
- App state transitions must be unit-testable without launching a real terminal UI.
- Dangerous operations must have tests for refusal paths, not only success paths.

### Required Unit Test Areas

Config:

- Creates default config
- Loads config
- Rejects invalid config
- Preserves human-editable structure where practical

Initialization:

- Creates `.flightdeck/`
- Creates `config.toml`
- Creates `state.json`
- Creates `worktrees/`
- Appends `.gitignore` entries
- Does not duplicate `.gitignore` entries
- Does not rewrite unrelated `.gitignore` contents

Branch naming:

- Slug generation
- `flightdeck/` prefix enforcement
- Existing branch detection
- Tab rename not affecting branch name

Git workflow:

- Dirty base detection
- Dirty worktree detection
- Worktree creation planning
- Existing branch attach behavior
- Existing checked-out branch refusal
- Push confirmation flow
- Push warning with uncommitted changes
- Local merge precondition checks
- Base drift calculation

Recovery:

- Loads valid state
- Handles missing state
- Handles stale state
- Scans `.flightdeck/worktrees/`
- Reconstructs tabs
- Marks tabs as recovered
- Does not auto-restart agents after recovery

Agent handling:

- Detects missing command before Git mutation
- Builds agent command from config
- Starts selected agent
- Does not pass initial prompts in MVP
- Classifies output patterns
- Applies manual status override correctly

Terminal/session abstraction:

- Creates primary terminal
- Creates child terminal
- Switches child terminal
- Closes child terminal
- Sends Ctrl-C
- Handles process exit
- Handles failed process start

App state:

- Creates tab
- Renames tab
- Switches tab
- Closes tab
- Maintains selected tab and selected child terminal
- Preserves state after save/load

TUI rendering:

- Render functions should be snapshot-testable where practical.
- Layout calculations should be unit-tested independently from terminal I/O.

### Integration Tests

Use temporary Git repositories.

Required integration tests:

- Initialize FlightDeck in fresh Git repo
- Create branch and worktree
- Attach to existing branch
- Recover worktree from disk
- Detect dirty base repo
- Detect dirty agent worktree
- Simulate push command through mocked remote
- Block local merge when base is dirty
- Allow local merge only when preconditions pass

Avoid depending on real GitHub in tests.

---

## 27. Recommended Architecture

Recommended stack:

```text
Rust + Ratatui + portable PTY abstraction
```

Suggested module structure:

```text
src/
  main.rs

  app/
    state.rs
    events.rs
    commands.rs
    modes.rs

  tui/
    layout.rs
    render.rs
    input.rs
    palette.rs

  git/
    repo.rs
    worktree.rs
    branch.rs
    status.rs
    remote.rs

  terminal/
    pty.rs
    session.rs
    shell.rs

  agents/
    registry.rs
    adapter.rs
    status.rs

  config/
    load.rs
    schema.rs
    init.rs

  persistence/
    project_state.rs
    recovery.rs

  fs/
    paths.rs
    ignore.rs

tests/
  integration/
    init.rs
    worktree.rs
    recovery.rs
    push.rs
    merge_preconditions.rs
```

Important architectural rule:

The TUI must not directly execute Git commands, mutate files, or manage PTYs. It should dispatch commands into testable application services.

---

## 28. MVP Non-Goals

MVP does not include:

- Windows support
- Multiple repositories in one FlightDeck process
- Live terminal resurrection after restart
- Automatic commits
- PR creation
- GitHub API integration
- Agent plugin system
- Initial prompt injection
- Built-in diff viewer
- Built-in editor
- Split panes
- Multi-agent modeling inside one worktree
- Multiple base branches
- TUI settings editor
- Automatic conflict resolution
- Background daemon