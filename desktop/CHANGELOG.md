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

- **Allow always for OpenCode permissions on the phone.** OpenCode permission prompts now offer Allow once, Allow always and Reject, the same choices as OpenCode's own dialog. Allow always approves the request for the rest of the session, and OpenCode also settles any other pending requests it covers, so a burst of identical requests takes one tap.

### Improvements

- None yet.

### Bug fixes

- **OpenCode no longer hangs after you answer one of several permission requests.** When OpenCode asked for several permissions at once (one per parallel tool call), the phone and the inline prompt only offered the last one. Answering it left the others pending, so the agent stayed blocked with nothing left to answer. FlightDeck now offers them one after another, in the order OpenCode's own dialog shows them, and the session stays waiting until the last one is answered.

## [0.9.0] - 2026-10-25

### New features

- **Attach images from the iOS app.** The chat composer has a photo button. Pick up to four images, and they are sent with your reply: the host saves each one and types its path ahead of your text, the same way a pasted image is attached on the desktop (container paths are mapped for containerized agents). Photos are scaled down to 2048 px JPEG on the phone and sent in 256 KB pieces, so they also work over the direct link. The button only appears when the host is new enough to accept images.


### Bug fixes

- **OpenCode permission prompts work from the phone and the inline prompt.** The prompt used to show the agent's last message instead of the request. Allow was acknowledged but never reached OpenCode, so the agent kept waiting. The prompt now says what OpenCode is asking for (for example "Access external directory …" with the command and paths it covers), and Allow once / Reject is answered through OpenCode's own API by FlightDeck's OpenCode plugin instead of an injected keystroke.

## [0.8.0] - 2026-10-23

### New features

- **Workers.** The launcher has a WORKERS section listing your saved FlightDeck Workers with live status (online, offline, not paired, access revoked, update needed), health, how many agents need you and a "version differs" badge. Desktop keeps status-only background links to them and raises OS notifications when an agent needs input, fails or finishes. Opening a worker gives a remote window with a Worker shell tab (and a Restart shell button), an "Open a project…" picker (scan the projects folder, browse the worker's home directory, or clone a git URL), host messages ("N messages while you were away"), a version-skew warning, and a "Taken over by …" panel with Reconnect when another Desktop takes the worker. Set up the worker with `flightdeck worker`; see the "Run a FlightDeck Worker" docs.
- **Configuration manager in remote windows.** Open Configuration in the command palette of any remote window (a worker or a Desktop or terminal host) to edit the host's configuration.

### Improvements

- Web protocol v7, with automatic fallback to v6 hosts. Workers require v7, so update the Desktop and the CLI together.
- A local Desktop window refuses to open while a worker runs on the machine; use the worker's remote window or run `flightdeck worker stop`.

## [0.7.0] - 2026-10-17

### New features

- **Reliable, private phone notifications.** Your Mac now decides when your phone gets a push and sends it as a visible alert whose text only the iPhone can read: the payload is end-to-end encrypted and the iOS app decrypts it. Notifications arrive even when the app was force-quit. There are three kinds: an agent needs input or permission, hit an error, or is done. A done alert waits until the agent has stayed idle for `[remote] push_done_debounce_secs` (default 5), so a pause mid-turn does not notify. The silent wake-up pushes and the push per queued message are gone. When the relay connection might be stale, or a push could not go out on it promptly, the Mac sends that push over HTTPS instead of letting it arrive late.
- **Direct link over Tailscale (opt-in).** Set `[remote] direct_enabled = true` and the iPhone can talk to your Mac directly over Tailscale instead of through the relay. `direct_port` (default 47800) and `direct_host` control where it listens and what address the phone is given. The Mac keeps an outbox, so nothing is lost while the phone is away. A pairing made with the direct link on uses it automatically; it needs the updated iPhone app, so re-pair phones you paired before. The remote-access status shows each phone as "paired · direct" or "paired · relay" and the address the Mac listens on. The Mac listens only on the addresses Tailscale itself reports, so a carrier-NAT `100.x` address on a tethered connection is never used. If the relay ever loses its record of your Mac, the direct link keeps working and FlightDeck tells you to re-pair the phone to get notifications back, instead of reconnecting in a loop.
- **Push-only relay mode.** Relay operators can run with `RELAY_MODE=push_only` for a relay that only delivers notifications, for use with the direct link. The relay also accepts pushes from the Mac on `POST /v1/push` and logs the outcome of each one. A push-only relay will not pair a phone whose app is too old for the direct link (or a Mac without it); both sides are told why. On the Mac, Pair Phone says whether the direct link is off or just not listening yet (check Tailscale).
- **Upgrade order: relay first, then desktops, then the iPhone app.** The new relay stops sending the old silent wake-up pushes, so a desktop that has not been updated gets no notifications through it; an old relay cannot accept the new pushes; and the new iPhone app no longer reacts to wake-up pushes, so update it last.


### Bug fixes

- A phone or desktop that resumes a relay session from 0 after the relay's queue overflowed no longer loops on `seq_violation`. It now gets the retained tail replayed and carries on.
- Copying text from Claude Code works again when Claude draws its own
  selection (its fullscreen renderer, which takes the mouse). Claude copies
  with an OSC 52 escape, which the app's terminals ignored, so nothing reached
  the clipboard. Now a copy made in the terminal on screen lands on this
  machine's clipboard, in a local window, a remote window (where Claude runs
  on the other machine) and a remote window's SSH shell. Copies from terminals
  not on screen are dropped, and a program can never read the clipboard.

## [0.6.0] - 2026-10-12

### New features

- A remote window has an **SSH shell** tab, always the first tab: a plain
  shell on the host, outside any project. It runs `ssh` from this machine to
  the same target "Open in VS Code" uses (the one saved for the remote, else
  the host's account at its address, so a tailnet address goes over
  Tailscale), and takes the whole window while it is on screen. The host must
  accept SSH. When the session ends, Reconnect starts a new one. Host commands
  are refused on that tab, so they cannot hit an agent you can't see.


### Bug fixes

- Tab and Shift-Tab reach the terminal again: a shell's Tab completion works
  in every terminal (an agent's shell tabs, a remote window's terminals and its
  SSH shell). The widget layer used to take them for moving focus between
  controls, so the shell never saw them.

## [0.5.1] - 2026-10-09

### Bug fixes

- Remote window: switching projects no longer drops you on the project's first
  agent. Each project remembers the agent you last looked at, and switching
  back lands on it (and on the terminal it was showing), as a local project
  already does.

## [0.5.0] - 2026-10-07

### New features

- **Clickable links in terminals.** Hold `Cmd` (macOS) or `Ctrl` (Windows, Linux) over a URL in an agent's or shell's output to underline it, and click to open it in the browser. A URL wrapped onto the next rows opens whole. Only `http` and `https` links open, and in a remote window they open on the machine you are at.


### Bug fixes

- The sidebar and git strip now show the branch an agent's worktree has checked out, in local and remote windows. Before, switching branches in the worktree's shell left the name it was created with.
- A remote window now sees git status update for every project, not only the one active on the host.

## [0.4.1] - 2026-10-07

### Bug fixes

- The Linux (`.deb`, `.rpm`, AppImage) and Windows (MSI, portable zip) builds are published with the release again. Their packaging steps failed, so 0.3.0 and 0.4.0 shipped macOS builds only.

## [0.4.0] - 2026-10-06

### New features

- **Open Worktree in VS Code**, from the command palette or an agent's right-click menu. A remote window opens the host's folder through VS Code's Remote - SSH extension, as the host's FlightDeck user at the address you connected to, or through an `ssh_target` you set for that remote in `~/.flightdeck/remotes.json`. A local window opens the folder directly.


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
