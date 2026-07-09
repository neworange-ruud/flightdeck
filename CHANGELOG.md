# Changelog

All notable changes to FlightDeck will be documented in this file.

Future releases should group notes under `New features`, `Improvements`, and `Bug fixes` so the repo changelog and GitHub Releases stay aligned.

## [Unreleased]

### New features

- None yet.

### Improvements

- Add a code-review topic breakdown that splits the codebase into small,
  independently reviewable scopes.
- Refresh the code-review topic breakdown for the current codebase, including
  container runtime, update, guarded rebase, pull-base, PTY, and TUI changes.
- Complete a full code review across all topics; the fixes below are its result.
- Harden the container security guardrails to also reject the `--flag=value`
  form of `--privileged` and `--env-host` (previously only the bare flag was
  caught).
- The Git Status overlay now shows the GitHub PR compare URL once the branch
  has been pushed (SPECS §21).
- Clearer error messages: distinguish "podman not installed" from "podman not
  ready" (and drop the macOS/Windows-only `podman machine start` hint on Linux),
  surface the underlying cause when a repository can't be discovered, and
  include the agent name in the "build the image first" guidance.

### Bug fixes

- Use `Shift+Esc` to leave terminal focus on Linux, where the window manager
  (e.g. GNOME) reserves `Alt+Esc` for cycling windows and FlightDeck never
  receives it. Matches the existing Windows behaviour; macOS keeps `Alt+Esc`.
- Container child terminals now launch a Linux shell inside the container via
  `podman exec` instead of the host shell, so child shells work on Windows hosts.
- Local merge and worktree rebase now verify the target worktree actually has
  the expected branch checked out before acting, preventing a merge from landing
  on — or a rebase from rewriting — the wrong branch.
- Force-terminate and quit now signal every terminal (primary and all children)
  even when one has already exited, so tabs close reliably and no child
  processes are left running.
- Restarting the primary agent stops the previous process first, preventing two
  agent instances from running against the same worktree.
- Container teardown no longer leaks a running container when spawn/attach fails
  partway, and container-removal failures on close/finish/abandon are now
  reported instead of silently succeeding.
- The base repository is no longer falsely reported as dirty on first run (the
  check now runs before FlightDeck writes its own config and `.gitignore`).
- Appending to a `.gitignore` whose last line lacks a trailing newline no longer
  glues the new entry onto that line.
- Stale recovered-tab entries are now surfaced as warnings instead of being
  silently dropped.
- Windows clipboard copy no longer corrupts non-ASCII text and correctly falls
  back to OSC 52 on failure.
- Windows clipboard handling is now clean under platform-specific Clippy checks.
- `Shift+Tab` is now forwarded to the terminal; the cursor is no longer drawn
  over scrollback when scrolled into history; and pasting while an overlay is
  open now dismisses it instead of swallowing the paste.
- Podman image-existence checks distinguish "not found" from runtime errors,
  agent keys are sanitized into valid image tags, and `flightdeck image build`
  validates the `[containers]` config even when containers are disabled.
- The once-a-day update-check cache now has a Windows fallback path
  (`USERPROFILE`).
- `scripts/release` accepts SemVer versions with dotted pre-release/build
  metadata, and the `keylog` example restores the terminal on error.

## [1.3.0] - 2026-07-01

### New features

- Add **Pull base**: run `git pull --rebase` on the base folder to bring the
  local base branch current after a PR is merged, without leaving FlightDeck.
  Available from the command palette (*Pull Base*) and `Ctrl-u`; refuses on a
  dirty base folder and aborts on conflict, leaving the base folder untouched.
- First-class Linux support: ship an `x86_64-unknown-linux-gnu` release binary,
  run clippy and tests on `ubuntu-latest` in CI, and post desktop notifications
  via `notify-send` (libnotify).

### Improvements

- Automate release-time changelog rollover so `./scripts/release <version>`
  moves `Unreleased` notes into the new version entry and resets the template.
- Clicking anywhere in the agent sidebar — the heading or empty space, not just
  an agent row — now switches to APP mode, so it works with zero or one agents.
- Lay out the command palette across two columns so more entries are visible at
  once without scrolling. Left/right arrow keys move the selection between the
  two columns.

### Bug fixes

- Restore mouse text selection in Split View and make wheel scrolling target
  the column under the pointer.

## [1.2.0] - 2026-06-29

Initial release.

### Supported features

#### Parallel agent workflows

- Run multiple local AI coding agents in parallel against the same Git repository.
- Create an isolated Git worktree and branch for each agent tab.
- Choose the agent per tab from configured agents, with OpenCode, Claude Code, and Codex CLI supported out of the box.
- Open additional shell tabs inside the same worktree.
- Recover saved tabs and managed worktrees when FlightDeck restarts.

#### Git-safe workflow

- Auto-initialize `.flightdeck/` inside a Git repository on first run.
- Append FlightDeck runtime entries to `.gitignore` without overwriting existing content.
- Show per-tab Git status including branch, file-change counts, ahead/behind, base drift, and upstream state.
- Push branches with confirmation and show a GitHub compare URL for pull request creation.
- Support a guarded local merge-back flow when strict preconditions are met.
- Abandon managed worktrees safely, with confirmation before discarding uncommitted changes.
- Enforce a no-history-rewrite boundary: FlightDeck does not stage files, create commits, amend commits, rebase, squash, force-push, or create pull requests.

#### Terminal UI and controls

- Provide a keyboard-first terminal UI with app mode, terminal mode, a command palette, and inline help.
- Support fast tab and terminal navigation with keyboard shortcuts.
- Support mouse selection for agent tabs and child terminals.
- Show a per-tab sidebar with agent process and status indicators.

#### Agent status and notifications

- Track live agent activity with default `working` and `idle` states.
- Allow manual status overrides.
- Offer optional precise agent status integrations via `flightdeck setup-status`.
- Offer optional macOS notifications when an agent finishes, waits for input, or fails.

#### Container support

- Run agents inside isolated rootless Podman containers.
- Bind-mount the host worktree into the container at `/workspace`.
- Reuse the same container for child shells.
- Reattach to still-running containers after restarting FlightDeck.
- Build agent images with `flightdeck image build` and validate readiness with `flightdeck doctor`.
- Support resource limits, localhost-only port forwarding, and controlled credential mounts or environment allowlists.
- Enforce container guardrails such as no `--privileged`, no container socket mounts, no home-directory mounts, `--cap-drop all`, and `no-new-privileges`.

#### Installation, updates, and platform support

- Install via Homebrew, the shell installer, or the Windows PowerShell installer.
- Self-update installer-based macOS and Linux installs with `flightdeck update`.
- Offer an opt-in once-daily update notice with `flightdeck setup-update`.
- Ship macOS and Windows builds from GitHub Releases.
