# Changelog

All notable changes to FlightDeck will be documented in this file.

Future releases should group notes under `New features`, `Improvements`, and `Bug fixes` so the repo changelog and GitHub Releases stay aligned.

## [Unreleased]

### New features

- FlightDeck Remote desktop pairing (Settings → Remote): a "Pair Phone" command
  palette action shows a scannable QR code and a 4-digit code, then completes
  pairing over the relay and brings the end-to-end-encrypted channel live. A
  "Unpair Phone" action (with confirmation) forgets the pairing. The relay honors
  a 4-digit `claim_token_hint` (short TTL, single use, per-connection rate limit),
  and the E2E salt is pinned to the claim-token bytes on both pairing paths.
- FlightDeck Remote agent chat transcript (iOS): the cleaned agent-conversation
  surface (PRD §5.3) — readable prose, noisy tool calls collapsed into tappable
  activity pills that expand inline to detail, inline permission-prompt cards,
  sparse timestamps, "load earlier" pagination, and standard chat auto-scroll
  with a "jump to latest" affordance. Includes the per-session `Agent · Shell`
  surface switcher (Shell disabled for now) and an inert compose-bar placeholder.
- FlightDeck Remote monitor surfaces (iOS): the Projects list with rolled-up
  status dots + plain-language summaries, per-project Agent-sessions list with
  git indicators / running time / needs-input previews, and the status roll-up
  precedence model (needs-input › working › idle) shared across the hierarchy
  (PRD §4, §5.2).
- FlightDeck Remote connection honesty & offline (iOS): live connection/latency
  indicator, a "Reconnecting…" banner that pauses commands (nothing sent blind),
  delivery-failure marking ("not delivered — retry"), and a cached last-known
  transcript/status shown read-only and clearly marked stale while disconnected
  (PRD §8, §9.2).
- FlightDeck Remote agent chat compose + voice (iOS): reply/follow-up send with
  inline permission resolution (Allow once / Deny), hold-to-talk voice dictation
  that drops an editable transcript into the field (never auto-sent), and an
  eyes-free Focus mode pinning the pending question with large Approve/Deny and a
  condensed timeline (PRD §5.3, §7; TTS read-aloud is a flagged fast-follow).
- FlightDeck Remote light control (iOS): a session actions sheet (safe actions
  grouped, destructive apart in red), New-agent screen (type + name + base +
  first task), restart agent (fresh process, same worktree), close session
  (confirmed), set manual cyan status override, and an Activity feed of status
  events that deep-link to the agent (PRD §5.5–§5.7).
- FlightDeck Remote minimal shell (iOS): a live terminal surface (streamed
  stdout/stderr, ANSI colours, scrollback) with the accessory key bar
  (`Esc Tab Ctrl ←↑↓→ | / - ~ \` ⌃C paste`, sticky Ctrl), Ctrl-C interrupt,
  copy/paste, a font-size control, and a landscape layout — one shell per
  session (PRD §5.4).
- FlightDeck Remote git actions (iOS): a read-only git status view (branch,
  base, ahead/behind, drift, changed files), plus confirmation-gated pull-base,
  guarded merge-back, and type-to-confirm abandon-worktree — no push/PR (that
  stays the agent's job) (PRD §5.5, §8).
- FlightDeck Remote push notifications (iOS + relay): APNs registration on the
  phone (token registered with the relay over the wire) and a relay-side APNs
  sender (ES256 JWT, HTTP/2, behind the `apns-live` feature) that fires typed
  notifications — *needs input* (urgent) and *finished* (green) — deep-linking to
  the agent, deduplicated by `event_id`, with independent notification toggles
  and per-project mute (PRD §5.2, §9.1–§9.2). Requires an Apple APNs auth key +
  signing team to deliver on device (see below).
- FlightDeck Remote settings (iOS): connected-device + connection card,
  Require-Face-ID-to-open gate, unpair-device (confirmed), and the notification
  preference toggles / per-project mute described above.
- FlightDeck Remote end-to-end test harness (`tests/remote_e2e.rs`,
  `scripts/e2e/`): a two-tier harness that stands up the *real* stack and
  exercises it, self-setup and self-verifying. Tier A (the CI gate) runs a real
  relay + a real desktop under a PTY + a Rust "phone" driver and asserts every
  remote capability against real side effects — pairing, snapshot, new-agent
  (worktree on disk), reply, permission decision, manual status, restart/close,
  git pull/merge/abandon, shell, and transcript. Tier B
  (`scripts/e2e/run-fullstack.sh`) drives the real iOS app in the simulator,
  paired live to the local relay + real desktop, and confirms it reaches the main
  tab. A single desktop test seam (`FLIGHTDECK_REMOTE_AUTOPAIR`) makes the pairing
  offer deterministic; the iOS side needs no production change. See
  `scripts/e2e/README.md`.

### Improvements

- FlightDeck Remote iOS transport hardening (follow-ups to the chat "paused —
  reconnecting" fix): `TransportStore` now bridges the transport's events onto
  the main actor through a single FIFO stream drained by one serial loop, so
  link-state and data events apply strictly in emit order (the previous
  per-event `Task { @MainActor … }` bridge gave no cross-event ordering
  guarantee — a stale `linkState` could land after a newer one). Chat's
  commands-paused gate now distinguishes a genuinely down link from a never-wired
  gate: an unbound gate still fails safe (paused, nothing sent blind) but trips a
  DEBUG assertion at the view's setup site, so a missing store binding fails
  visibly instead of masquerading as a permanent "reconnecting" (remote-control-qbj,
  remote-control-7wu).

### Bug fixes

- FlightDeck Remote iOS chat could not control the agent — the composer stayed
  disabled showing "paused — reconnecting" even though the link was up (shell and
  notifications worked). The chat screen was mounted without the live transport
  store, so its commands-paused gate never bound and defaulted to paused; the
  transcript never loaded either. `MainTabView` now threads the live store into
  `AgentChatView`, so chat binds to the real connection and phone→agent control
  (replies, permission decisions) works. Surfaced by the full-stack E2E harness.
- FlightDeck Remote desktop first-pairing bootstrap: a fresh desktop with no
  saved pairings sent `auth_response` before ever offering, so the relay rejected
  it as an unknown device and a first pairing could never complete. The client
  now, when a pairing is pending and it has no existing pairings, sends its
  `pairing_offer` pre-auth (registering its device key) and then authenticates —
  matching the relay's offer-first bootstrap. Returning desktops with saved
  pairings authenticate first, unchanged. Surfaced by the new end-to-end harness.

## [1.7.1] - 2026-07-13

### New features

- Waiting-for-input alerts now post an OS notification and play a distinctive
  three-pulse sound, separate from the completion chime, including OpenCode
  question prompts.

### Improvements

- None yet.

### Bug fixes

- Fix image paste for Codex CLI: local sessions now receive Codex's native
  clipboard paste shortcut (`Ctrl-V` and reported `Cmd-V` on macOS), while
  containerized sessions receive a path to a safely shared temporary image file.

## [1.7.0] - 2026-07-13

### New features

- Add a **configuration manager**, opened from the command palette
  ("Open Configuration"). It edits the common settings as toggles/choices —
  OS notifications and per-category alerts, the finish chime, update checks,
  agent tab position, and the default agent. `Tab` switches between the
  **Global** and **Project** scope (the header always names the file being
  edited and, for a project, which project), `Space` toggles, `c` clears a
  project override so it re-inherits, `s` saves, and `e` opens the raw
  `config.toml` in `$EDITOR` for the full surface. Saving reloads every open
  project's effective config immediately.
- Introduce a per-user **global config** at `~/.flightdeck/config.toml`,
  created on first run with every setting present and documented so it is clear
  what can be overridden. Each project's `.flightdeck/config.toml` now only
  needs to store the values it overrides; everything else is inherited from the
  global base. The project layer wins field-by-field, except `[agents]`, which
  a project replaces wholesale when it defines any of its own. Existing
  fully-populated project configs keep working unchanged.

### Improvements

- OS notifications are now **on by default** (previously opt-in), including the
  finish chime (`sound`). Turn them off with `enabled = false` under
  `[notifications]` in the global or a project config, or from the
  configuration manager.
- OS notifications now include the project name, e.g. `myproject: my-agent`,
  so alerts are unambiguous when several projects are open.

### Bug fixes

- None yet.

## [1.6.0] - 2026-07-13

### New features

- Play a distinctive two-note "ding" chime when an agent finishes its turn
  (transitions from working to idle/completed). The sound is embedded in the
  binary, plays on macOS, Linux, and Windows, and can be turned off with
  `sound = false` under `[notifications]`.

### Improvements

- Show a compact red animated Braille spinner on working Agent and Project
  tabs, with green dots for idle projects and a high-contrast white active
  Project tab with dark navy text.

### Bug fixes

- Detect working and waiting states from explicit Claude Code, Codex, and
  OpenCode lifecycle events instead of terminal output/silence, preventing typed
  prompts from arming false completion notifications and making project-tab
  progress indicators dependable.
- Fix the `create_tab_happy_path` test failing on Windows by normalizing path
  separators when asserting the OpenCode config directory environment variable.

## [1.5.0] - 2026-07-12

### New features

- **Multiple projects in one window.** FlightDeck can now run several project
  folders side by side. A new **project tab row** at the top of the screen
  switches between them; the folder you launch from is the first (active)
  project. Each project keeps its own Agent Session Tabs, worktrees, git status,
  and base branch — and every open project stays **live in the background**, so
  agents in a project you're not looking at keep running and still fire OS
  notifications when they finish or need input.
  - **Open another project** with the **`+ project`** button on the tab row or
    the **Open Project** palette command. A folder picker lets you **type a
    path** or **browse** directories (↑↓ select · → open folder · ← parent ·
    Enter to open).
  - **Switch projects** with **`Shift`+`Left` / `Shift`+`Right`** (works while a
    terminal is focused too), by clicking a project tab, or via the **Next/
    Previous Project** palette commands.
  - **Close a project** with the tab's `✕` (confirmed first — it stops that
    project's agents) or the **Close Project** palette command.
  - **Open projects are remembered across restarts** (per-user
    `~/.flightdeck/workspace.json`); each project's own tabs are still recovered
    from its `state.json`, and agents are never auto-relaunched.

### Improvements

- Enable the once-a-day update notice by default and tell Homebrew users to run
  `brew update && brew upgrade flightdeck` so stale tap metadata is refreshed.

### Bug fixes

- None yet.

## [1.4.0] - 2026-07-09

### New features

- **Mouse-driven tab management on the child tab bar.** The horizontal tab bar
  now carries **`+ agent`** and **`+ shell`** buttons, right-aligned and styled
  distinctly from the tabs. **`+ agent`** first asks which **backend** to use
  (Claude, OpenCode, …) then spawns an *additional agent* in the **same
  worktree** as another `agent` tab on the row (agents number `agent`, `agent 2`,
  `agent 3`, …); **`+ shell`** opens a child shell. Each tab shows a `✕` close
  control you can click to close it. (With no session yet, `+ agent` creates a
  fresh Agent Session Tab/worktree.) New palette commands **New Agent** and
  **Close Agent** cover the same in-session agents from the keyboard.
- **Sidebar close control.** Each Agent Session Tab in the sidebar shows a
  right-aligned `✕` on its name row. Clicking it asks whether to **Abandon** the
  worktree, just **Close** the agent, or **Cancel**.
- **Clearer terminology.** The worktree-level tabs (and their palette commands)
  are now called **"Agent Session Tab"** — *New/Rename/Close/Switch Agent Session
  Tab* — to distinguish them from the individual agent tabs on the horizontal
  row within a session.

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
- **Confirmations and notifications now appear as a centered modal dialog** that
  overlays the UI, instead of a single line at the bottom of the screen. Every
  dialog shows a clickable button for each available action (Abandon, Close,
  Cancel, …) while keeping the existing keyboard shortcuts, and long messages
  wrap across lines inside the box instead of being truncated.
- **Closing always confirms first.** Clicking a shell/agent tab's `✕` (or
  pressing `Ctrl-w`) asks for confirmation before closing the terminal, matching
  the existing confirmation flow for closing an Agent Tab. Routine actions no
  longer pop a follow-up notification — opening a shell/agent or closing a tab is
  its own confirmation, so those toasts are gone.
- New agent sessions now **symlink** the base folder's `.env` and `.env.local`
  into the worktree automatically, instead of requiring a manual copy. The link
  keeps secrets in sync with the base and is best-effort — sessions where the
  base has no `.env`/`.env.local` are created silently, with nothing to do. The
  now-redundant *Copy .env(.local)* command is hidden from the palette.

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
