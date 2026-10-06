# FlightDeck Desktop changelog

All notable changes to the FlightDeck desktop app (`flightdeck-desktop`,
`FlightDeck.app`) are documented in this file. The desktop app is released on
its own `desktop-v<x.y.z>` tags, separately from the `flightdeck` CLI/TUI, whose
changes are in the root `CHANGELOG.md`. A change to the shared core that both
apps get is listed in both.

Group notes under `New features`, `Improvements` and `Bug fixes`.
`scripts/release-desktop` rolls `Unreleased` into a version, and the release job
uses that version's section as the GitHub Release notes.

## [Unreleased]

### New features

- **Open Worktree in VS Code**, from the command palette or an agent's right-click menu. A remote window opens the host's folder through VS Code's Remote - SSH extension, as the host's FlightDeck user at the address you connected to, or through an `ssh_target` you set for that remote in `~/.flightdeck/remotes.json`. A local window opens the folder directly.

### Improvements

- None yet.

### Bug fixes

- In a remote window, the New Agent dialog now follows the host. Switching target updates the form, and choosing an agent with ↑/↓ moves the `(•)` mark. Before, the form stayed as it first opened and the mark stayed on the host's agent.

## [0.3.0] - 2026-10-03

### New features

- **Paste images into an agent.** Cmd-V (Ctrl-V) with an image on the
  clipboard and no text, such as a screenshot, saves the image and types its
  path, which Claude Code and other agents attach. In a remote window the image
  goes to the host, where the agent runs. Text on the clipboard still wins.


### Bug fixes

- **A remote window smaller than the host's terminal can reach all of it.**
  The host's grid used to be cut off at the bottom and right, hiding the
  agent's newest output and its prompt. The window now shows the bottom of the
  grid first, the wheel scrolls through the hidden rows before the history, a
  sideways swipe reaches the hidden columns, typing brings the cursor back into
  view, and scrollbars show when part of the grid is out of view.

## [0.2.0] - 2026-10-02

### New features

- **Control a FlightDeck on another machine.** *Connect to remote…* (the
  launcher, and File in the menu bar) pairs with a FlightDeck running elsewhere
  on your network or VPN, using the address and 4-digit code its web access
  overlay shows in network mode. Each remote opens in its own window with the
  usual sidebar, git strip and terminals. You browse it independently: looking
  at another agent never moves the selection on the other screen, and a small
  `host` marker shows which agent its user is looking at. Commands, keystrokes
  and dialog answers act on what this window shows. Pick Control (the default)
  or Observe, and take over the input lock from the status bar when someone
  else is typing.
- **Saved remotes.** Paired machines are remembered in
  `~/.flightdeck/remotes.json` (owner-only), so reconnecting needs no new code.
  Forget one from the connect window. Nothing connects at launch until you
  choose a remote.
- **Plain-WebSocket warning.** The first connect to a remote outside loopback or
  Tailscale warns, once, that the link is not encrypted.
- **Run more than one FlightDeck.** *New window* (File in the menu bar, Cmd-N,
  the Dock icon's menu, the Linux launcher's right-click menu, or the command
  palette) starts another instance on the launcher, so one can work on local
  projects while another controls a remote. Each has its own window and quits
  on its own. `flightdeck-desktop --launcher` does the same from a shell.
- **Connect to Remote and New Window in the command palette,** in the local
  window and in a remote window's palette alike.

### Improvements

- **Remote commands can name the session they act on.** When the app is
  controlled from another machine, FlightDeck Web (protocol v6) accepts a
  `session_id` or `terminal_id` on session commands and acts on that session
  without moving this window's selection. Browsers are unchanged; an open
  browser tab from an older build is asked to reload.

### Bug fixes

- **Only one FlightDeck on a computer talks to the phone relay.** Two of them
  at once (the desktop app next to the TUI, or two desktop windows) kept
  knocking each other's relay connection off and confused the phone's
  message order. The first one started owns the relay; the others leave it
  alone, and Pair Phone in them says which FlightDeck to pair from.
- **Agents show colour and bold when the app is launched from Finder or the
  Dock.** Their terminals now always start with `TERM=xterm-256color` and
  `COLORTERM=truecolor`, describing the app's own emulator. Before, they
  inherited the launch environment, which outside a terminal has no `TERM`, so
  agents fell back to plain text.

## [0.1.1] - 2026-10-02

### Bug fixes

- **The app works when launched from Finder, the Dock or a desktop launcher.**
  Such a launch only gets the system `PATH`, so `git` and the agents did not
  resolve as they do in a terminal, and opening a project could fail. The app
  now adopts your login shell's environment at start-up when it was not started
  from a terminal. This is bounded to 5 seconds, and if it fails the launch
  environment is kept.
- **"Not a git repository" errors now name the folder and give git's own
  reason.** Before, any git failure became "could not determine repository
  root", and the launcher told you to "run FlightDeck from a git project".

## [0.1.0] - 2026-10-01

### New features

- **Native desktop app (preview).** A GPUI window around real agent terminals.
  Project tabs sit in the titlebar, with an agent sidebar showing status, diff
  stats and unread dots, a git strip (Push, Pull base, Finish) and a
  mode/status bar. It has the command palette, every TUI dialog and
  confirmation, the configuration manager, help and about, web access and phone
  pairing (QR drawn natively), split view and child terminals, native macOS
  menus, a dock badge, `--isolated`, and a project picker when launched outside
  a repository. Every TUI chord works the same; Cmd on macOS is left to the
  platform. See `specs/DESKTOP_UI.md`.
- **Mission control view (Alt-m).** Live tiles for every working or waiting
  session across all projects, ordered needs-you first, plus "earlier today"
  cards with a one-click Push, Pull base or Finish. A scope menu selects the
  last 24h, 7 days or all sessions, with a project filter. Enter opens a tile
  full size and Alt-Esc goes back. Waiting tiles offer inline Approve/Deny for
  permission and single-select prompts, using the same answer path as the
  phone.
- **Self-update** from the `desktop-v*` GitHub Releases.
- **New settings:** `[ui] desktop_terminal_font_size` and
  `[ui] macos_option_as_meta`.
