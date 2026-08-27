# Design Briefing: FlightDeck Web — the TUI as a browser application

**Audience:** a designer (Claude Design) producing the visual design for a web
application that exposes **the entire FlightDeck terminal interface** in a
browser, so a user can control a running FlightDeck instance from any device with
a browser.

**Relationship to the other briefing in this folder:** `MOBILE_REMOTE_BRIEFING.md`
describes a *curated phone companion* — deliberately reduced, read-mostly, with
cleaned transcripts instead of raw terminals. **This document describes the
opposite**: a full-fidelity, keyboard-first control surface that intends to
reproduce every screen, region, overlay and command the desktop TUI has. Where
the two documents disagree, this one governs the web app.

---

## 1. What FlightDeck is

FlightDeck is a cross-platform (macOS / Linux / Windows) **terminal UI for
orchestrating several local AI coding agents working in parallel on one Git
project**. You launch it inside a Git repository. It creates isolated Git
**worktrees**, starts a selected AI coding agent (Claude Code, OpenCode, Codex
CLI) inside each one, and lets you switch between those parallel agent sessions,
open extra shells in each worktree, watch Git and agent status, and push branches
for pull-request workflows.

The mental model, stated in FlightDeck's own README and load-bearing for the
whole UI:

> **Each Agent Tab = 1 Worktree = 1 Branch = 1 Primary Agent Process + optional Shell Processes**

The user is a developer whose job has shifted from *typing* to **supervision**:
launch agents, see which are working / idle / waiting, jump into whichever needs
attention, then push the good work. The single most important recurring event in
their day is *"an agent I wasn't looking at just finished, or just got stuck."*

There is a strict three-level hierarchy that every screen must make legible:

1. **Projects** — repositories open at once, as a tab row across the top. Every
   open project stays live in the background.
2. **Agent Session Tabs** — the middle level, a left sidebar list within a
   project. One agent, one worktree, one branch. **This is the unit the user
   thinks in.**
3. **Terminals within a session** — one *agent* terminal plus zero or more
   *shell* terminals, as a row of tabs (`agent | shell 1 | shell 2`).

---

## 2. What we are asking for

**Goal:** a web application that *resembles* the TUI and exposes *all* of it, so
a FlightDeck instance can be fully driven from a browser.

"Resembles the TUI" is a deliberate aesthetic and functional choice, not
nostalgia. It means:

- **Monospace, grid-aligned, dense.** The information architecture is a character
  grid. Keep that feeling: aligned columns, fixed-width labels, box-drawing
  rules, compact rows. Do **not** translate it into an airy SaaS dashboard with
  cards and generous whitespace.
- **Dark, terminal-native palette.** The TUI runs on the user's terminal theme,
  effectively the ANSI 16-color palette on a near-black ground (see §7).
- **Keyboard-first, with real mouse support.** Every action must be reachable by
  keyboard; the TUI also supports clicking tabs, buttons and `✕` controls, and
  drag-to-select in terminals.
- **Terminals are real terminals.** The main pane is a live VT100 screen, not a
  chat transcript. Full ANSI color, cursor, scrollback, text selection.

But it is a **web application**, so it should be *better* than a character grid
where the browser is genuinely better: crisp typography instead of half-block
approximations, real scrollbars, smooth animation, resizable panes, hover states,
focus rings, and layouts that reflow on narrow viewports. Think **"the TUI, drawn
properly"** — the same regions, hierarchy, density and semantics, rendered with
pixels instead of cells.

---

## 3. The main screen: exact anatomy

The desktop TUI's layout, top to bottom. A reference screenshot is at
`specs/screenshot.png`. Sizes below are the terminal's own row/column budgets —
they tell you the *relative weight* the design should preserve, not literal
pixel values.

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ ████▓▓▓▒▒▒░░░  · F L I G H T D E C K ·  ░░░▒▒▒▓▓▓████                    │  logo header (1 row)
│ ● flightdeck ✕ | ● api ✕ | ● web ✕                            + project  │  project tabs (1 row)
├──────────────────────────────────────────────────────────────────────────┤  divider (1 row)
│ Agents          │ agent ✕ | shell 1 ✕            + agent | + shell        │  terminal tabs (1 row)
│ ────────────────│                                                        │
│ ▸ ● fix-login ✕ │                                                        │
│   Claude Code   │            active terminal (agent or shell)             │
│   [in progress] │                                                        │
│   ~dirty drift:3│                                                        │
│ ────────────────│                                                        │
│   ● add-tests ✕ │                                                        │
│   OpenCode      │                                                        │
│   [idle]        ├────────────────────────────────────────────────────────┤  divider
│   no-upstream   │ ⎇ flightdeck/fix-login │ +3 ~2 -1 (6 files) │ ↑0 ↓0 │ … │  git info bar (1 row)
│                 ├────────────────────────────────────────────────────────┤  divider
│                 │ MODE: TERMINAL | Shift+Esc: app commands | Ctrl-g: …    │  status bar (1 row)
└─────────────────┴────────────────────────────────────────────────────────┘
```

### 3.1 Logo header (1 row, full width)

A branded wordmark row. The wordmark is `· F L I G H T D E C K ·` (a tighter
`F·L·I·G·H·T·D·E·C·K` variant on narrow terminals), centered, framed by
`▓▓▓▒▒▒░░░` gradient ramps and padded to both edges with solid `█` blocks in
cyan. In the web app this is the app's title bar / brand strip — reinterpret the
block-ramp motif freely (it exists because the terminal has no gradients; a
browser does), but keep it a **thin full-width branded band**, not a tall hero.

### 3.2 Project tab row (1 row, full width)

- One tab per open project, rendered `● <folder-name> ✕`, separated by ` | `.
- **Status dot** per project, precedence-ordered: **red** = an agent needs
  attention, **red animated braille spinner** = an agent is working, **green** =
  all idle.
- The active project is highlighted (inverted: dark-blue text on white in the
  TUI; the accent background is `rgb(16, 38, 68)`).
- Each tab has a **red `✕`** close control.
- A right-aligned **`+ project`** button opens another project folder.

### 3.3 Agents sidebar (28 columns wide, full height of the body)

Header: a centered bold **`Agents`** title. Then, per session, a **4-line block**
preceded by a horizontal divider rule (including above the first):

1. **Name line** — `▸ ` selection marker (2 cols) + a one-cell **status
   indicator** + the session name, truncated with `…`, with a right-aligned red
   **`✕`** close control pinned to the last column. Selected rows are bold
   yellow; unselected are white.
2. **Agent + status line** — the agent's display name in gray (`Claude Code`,
   `OpenCode`, `Codex CLI`) followed by a bracketed status label in its status
   color: `[in progress]` / `[idle]` / `[waiting]` / `[error]`, or a cyan manual
   override label.
3. **Git indicator line** — compact chips, only when they apply:
   `[recovered]` (magenta), `[existing]` (cyan), `~dirty` (yellow),
   `+<ahead> -<behind>` (cyan) or `no-upstream` (dim), `drift:<n>` (magenta), or
   `git: ?` (dim) when status hasn't been collected yet.
4. Blank / spacing row keeping every block the same height.

**Special state:** a session whose worktree is still being created shows an
animated red braille spinner on the name line and `creating worktree…` instead
of the agent/status line.

**Empty state:** `No tabs. Ctrl-n to create.` in dim gray.

The sidebar can be configured to the **right** side instead of the left
(`ui.agent_tab_position = left | right`) — the design should say how it handles
both.

### 3.4 Terminal tab bar (1 row, top of the main pane)

- Tabs for the session's terminals: `agent ✕ | shell 1 ✕ | shell 2 ✕`.
- Active tab is inverted: **black on yellow** for the *agent* terminal, **black
  on cyan** for a *shell*. Inactive tabs are gray. Each carries a red `✕`.
- Right-aligned action buttons: **`+ agent`** (black on green) and **`+ shell`**
  (white on blue). They are styled distinctly from tabs so they read as actions.
- Empty state: `No tab selected`.

### 3.5 Terminal viewport (fills the main pane)

A live VT100 screen rendered cell by cell: full 16-color / 256-color / truecolor
ANSI, bold / italic / underline / reverse, block cursor, scrollback.

- **When the terminal is not the focused pane** (App mode), its text is **dimmed
  to muted gray with bold removed**, so an inactive terminal reads as "asleep".
  Configurable (`ui.dim_terminal_in_app_mode`).
- **Text selection** by drag; copies to clipboard on release. Dragging past the
  edge auto-scrolls to reach offscreen text. `Shift`-drag forces selection over a
  mouse-driven TUI app running inside. Selection highlight is a solid
  `rgb(58, 90, 138)` background with white text.
- **Split view** (`Ctrl-b`) replaces the tab bar + single viewport with **all of
  the session's terminals side by side in equal-width columns**, each topped by
  its own label row, with a one-column gutter between them. This is a real mode
  the design must cover.
- Empty state: `FlightDeck — no Agent Session Tab selected. Press Ctrl-n to
  create one.`

### 3.6 Git info bar (1 row)

A one-line summary for the **selected session's worktree**, regardless of which
terminal is focused. Segments separated by a dim ` │ `:

`⎇ <branch>` (blue glyph, bold white branch) │ `+3 ~2 -1 (6 files)`
(green/yellow/red/dim) or `clean` (green) │ `↑<ahead> ↓<behind>` (cyan) or
`no upstream` (dim) │ `base +<n>` (magenta, only when the base has moved) │
`base: <base-branch>` (dim).

Empty state: `No Agent Session Tab selected`.

### 3.7 Status bar (1 row, bottom)

A **mode chip** followed by the contextual keys for that mode:

- Terminal mode: `MODE: TERMINAL | Shift+Esc: app commands | Ctrl-g: command palette`
- App mode: `MODE: APP | Enter: focus terminal | Ctrl-g: command palette | ?: help`

The chip is black text on the mode's color (configurable per mode, default
distinct colors from green/cyan/blue/magenta/yellow/red/white). Key names are
yellow; prose is default foreground.

An optional trailing **update notice** chip appears when a newer release exists:
`● v1.16.0 available — run \`flightdeck update\`` in black on yellow. It never
becomes a modal.

### 3.8 Live-pane border (optional, on by default in spirit)

`ui.mode_border = off | dim | normal | bright` draws a **1-cell border around
whichever pane is receiving keystrokes** — the sidebar in App mode, the terminal
in Terminal mode — in that mode's color at the chosen brightness. This is the
main "where is my focus" affordance and the web design needs an equivalent (and
should probably make it stronger, not weaker).

---

## 4. The keyboard model — the hardest translation problem

FlightDeck is **modal**, with two modes, because the terminal in the main pane
wants every keystroke:

- **Terminal mode** — keystrokes go to the active terminal. Leave with
  `Alt+Esc` (macOS) / `Shift+Esc` (Windows/Linux), or an optional `F2` binding.
  Bare `Esc` passes through to the hosted agent. `Ctrl-g` still opens the palette.
- **App mode** — keystrokes control FlightDeck. `Enter` focuses the terminal;
  `?` shows help; bare arrow keys navigate.

The **command palette (`Ctrl-g`) is the dependable fallback** — it exists
precisely because terminal shortcut collisions are unavoidable. In a browser the
collision problem is *worse* (the browser itself claims `Ctrl-t`, `Ctrl-w`,
`Ctrl-n`, `Ctrl-p`…). **The design must take a clear position on this**, and it
is one of the most valuable things this briefing can get back:

- How is the current mode communicated at a glance? (chip + border + terminal
  dimming, or something better?)
- How does a user leave terminal focus with a gesture the browser won't eat?
- Which shortcuts are re-mapped for the web, and how is that taught?
- Is the palette promoted from "fallback" to "primary" in the web app?

### Full shortcut inventory (from the in-app help overlay)

| Group | Keys | Action |
|---|---|---|
| Global | `Ctrl-g` | Command palette |
| Global | `Ctrl-q` | Quit / close app |
| Global | `Ctrl-n` | New Agent Session Tab |
| Global | `Ctrl-p` | Push current branch |
| Global | `Ctrl-u` | Pull base (`git pull --rebase`) |
| Global | `Ctrl-f` | Finish / local merge current session |
| Global | `Ctrl-k` | Close current session |
| Global | `Alt-o` | Open worktree in file manager |
| Global | `?` | Help / keybindings |
| Projects | `Shift-←` / `Shift-→` | Previous / next project |
| Projects | click tab / `+ project` | Switch / open project |
| Sessions | `↑` / `↓` (or `Alt-↑/↓`) | Previous / next session |
| Sessions | `Alt-1` … `Alt-9` | Jump to session by index |
| Terminals | `Ctrl-t` | New child terminal |
| Terminals | `Ctrl-w` | Close active child terminal |
| Terminals | `←` / `→` (or `Alt-←/→`) | Cycle terminal tabs (agent + shells) |
| Terminals | `Ctrl-b` | Toggle split view |
| Selection | drag / `Shift`-drag | Select terminal text (copies on release) |
| Focus | `Alt+Esc` / `Shift+Esc` / `F2` | Leave terminal focus |
| Focus | `Enter` | Focus active terminal |
| Status | `Ctrl-s` | Set manual status |
| Status | `Ctrl-r` | Restart primary agent |

Note the `Alt`- and `Shift`-modified navigation works in **both** modes, so the
user can switch projects and tabs without leaving terminal focus.

---

## 5. The command palette

`Ctrl-g`. A centered overlay (≈90×32 cells) with a cyan border, titled
`Command Palette  (Esc to close)`. A `> ` filter input with a cursor on the first
row; below it the filtered commands **split across two columns**, each column
rendering its own group headers (bold yellow) with a blank line between groups.
The selected entry is inverted black-on-cyan. Empty result: `(no matches)`.

The complete command set, by group — the web app must expose all of it:

- **Projects** — Open Project · Close Project · Next Project · Previous Project
- **Agent Session Tabs** — New Agent Session Tab · Rename Agent Session Tab ·
  Close Agent Session Tab · Switch Agent Session Tab · Restart Agent
- **Worktree** — Rebase Worktree · Abandon Worktree · Open Worktree in File Manager
- **Git** — Push Branch · Finish / Local Merge · Pull Base · Show Git Status
- **Terminals** — New Child Terminal · Close Child Terminal · New Agent ·
  Close Agent · Switch Child Terminal · Open Shell
- **Status** — Set Manual Status
- **Configuration** — Open Configuration
- **Remote** — Pair Phone · Unpair Phone
- **View** — Toggle Split View · Show Help · About FlightDeck
- **Global** — Quit

(There is also a context-gated "Copy .env(.local)" command, hidden from the list
when it doesn't apply.)

---

## 6. Overlays, modals and dialogs — the complete inventory

Everything below is a **centered overlay** over the dimmed main view, with a
titled single-line border in an accent color. All of it needs a web treatment.

### 6.1 Named overlays

| Overlay | Trigger | Contents |
|---|---|---|
| **Command palette** | `Ctrl-g` | §5. Cyan border. |
| **Help / keybindings** | `?` | The full shortcut table from §4, grouped under bold yellow headers, keys in cyan and descriptions in gray. Yellow border, ≈64×40. `Esc`/`q` closes. |
| **Git status** | palette → Show Git Status | Label-aligned detail: Branch, Base branch, Base drift (`none` / `N commits ahead since creation`), Dirty (`yes` / `clean`), Upstream (or `none (not pushed)`), Ahead/behind `↑N ↓N`, Worktree path, and optionally a **PR compare URL** in green. Yellow border, ≈70×18. No file diff view. |
| **Configuration** | palette → Open Configuration | §6.3. Cyan border, ≈66 wide, height fits content. |
| **Remote pairing** | palette → Pair Phone | A **QR code** rendered as black-on-white half-block cells, a 4-digit pairing code, an expiry countdown in seconds, and a status line (`Waiting for phone…` / `Paired ✓` / an error), with success/error accents. Degrades honestly to code-only when the terminal is too small for the QR. |
| **About** | palette → About FlightDeck | Center-aligned: `FlightDeck  v<version>`, a one-line description, credits, and `https://flightdeckai.app` in cyan. |
| **Dialog** | many | §6.2. |

### 6.2 The dialog model

One reusable centered modal covers every confirmation, selection, text entry and
notification. Its parts:

- **Title** — the question or message, wrapped across lines.
- Optional **text input field** (a typed value, with a block cursor).
- Optional **scrollable list** with a highlighted selection (used by pickers).
- **Buttons** in display order, each bound to a **keyboard accelerator**
  (`Enter`, `Esc`, `Tab`, a letter, or a digit). Clicking a button synthesizes
  its key, so mouse and keyboard drive the exact same code path — the web design
  should preserve that "every button shows its key" property.
- **Accent**: cyan for confirmations/selections, blue for plain notifications
  (which have a single `OK` and are dismissed by any key or click).

Every dialog in the app, with its real copy:

| Dialog | Shape | Copy / buttons |
|---|---|---|
| **New Agent Session Tab** | list + input + buttons | Title: `New Agent Session Tab   (↑/↓ agent · type branch · Tab = run from base branch)`. A **radio list of agents** (`(•) Claude Code` / `( ) OpenCode` / `( ) Codex CLI`), a **branch-name text field**, buttons `Enter Create` · `Tab Run from base: off|<branch>` · `Esc Cancel`. When "run from base" is on, the branch field is **hidden** and the title becomes `Runs on base branch '<base>' in the project root — no worktree.` |
| **New agent — pick a backend** | buttons | One digit-accelerated button per configured agent, plus `Esc Cancel`. |
| **Rename this Agent Session Tab** | input | `Enter Rename` · `Esc Cancel` |
| **Set status override** | buttons | `i In progress` · `w Waiting` · `b Blocked` · `d Done` · `c Clear` · `Esc Cancel` |
| **Close tab** | buttons | `Close tab — how should running processes be handled?` with digit-accelerated actions + `Esc Cancel` |
| **Close child terminal** | buttons | `Close <label>?` — `y Close` · `n Cancel` |
| **Close agent** | buttons | `Abandon the worktree, or just close the agent?` — `a Abandon` · `c Close` · `n Cancel` |
| **Push confirm** | buttons | `The worktree has uncommitted changes. Push the committed changes only?` — `p Push committed` · `c Cancel` |
| **Abandon worktree** | buttons | Clean: `Abandon this worktree?` — `y Abandon`. Dirty: `The worktree has uncommitted changes. Discard them and abandon it?` — `y Abandon (force)`. Plus `n Cancel`. |
| **Merge confirm** | buttons | `Merge <branch> into <base> then remove the worktree[ (stops the running agent)]?` — `y Merge` · `n Cancel` |
| **Rebase confirm** | buttons | `Rebase <branch> onto <base>[ (base moved N commits)][; agent is running — its HEAD will be rewritten]? Rewrites history; aborts on conflict.` — `y Rebase` · `n Cancel` |
| **Open project** | input + list | Title: `Open project — <current dir>   (↑↓ select · → open folder · ← parent · Enter to open · or type a path)`. A **folder browser list** (or `(no subfolders)`) plus a typed-path field. `Enter Open` · `Esc Cancel`. Must be a Git repository. |
| **Close project** | buttons | `Close this project? Its agents will be stopped.` — `y Close` · `n Cancel` |
| **Unpair phone** | buttons | `Unpair this phone? It loses access until you pair it again.` — `y Unpair` · `n Cancel` |

### 6.3 The configuration manager

A curated settings overlay (not a raw file editor), with a **two-scope model**
that the design must express clearly:

- A **scope selector** row: ` Global ` and ` Project (<name>) `, the active one
  inverted on cyan.
- An **`Editing: <path>`** line so the target file is unambiguous.
- Rows of curated settings. Each row: a `▸` selection marker, a **control**
  (`[x]` / `[ ]` for booleans, `‹value›` for a choice, inline text for free
  text), a fixed-width **label**, and an **origin tag** telling you where the
  effective value came from: `(set here)` green, `(global)` blue, `(default)`
  dim. This layered-inheritance display is the interesting design problem here.
- A standing yellow **note** that the default relay is restricted.
- A status line: a save confirmation (green) or `Unsaved changes` (yellow).
- A footer of keys: `↑↓ move · Space toggle / edit · Tab switch scope · c clear
  override · s save · e edit file in $EDITOR · Esc close`, and in edit mode
  `Type to edit · Enter save value · Esc cancel · Backspace delete`.

The curated fields, in order: OS notifications · Notification sounds · Notify
when finished · Notify when waiting · Notify when failed · Check for updates ·
Use F2 to leave terminal focus · Agent tab position (`left`/`right`) · Default
agent · Terminal mode color · App mode color · Mode border
(`off`/`dim`/`normal`/`bright`) · Dim terminal in app mode · FlightDeck Remote
(phone link) · Relay URL (free text).

---

## 7. Status semantics and the color system

Status is the **primary information** in this product. The exact mapping the TUI
uses — preserve the semantics, feel free to refine the hues for a screen:

| Interpreted state | Sidebar label | Color | Indicator glyph |
|---|---|---|---|
| Starting / Running / Working | `in progress` | **red** indicator, cyan label | animated braille spinner `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` (~12.5fps) |
| Idle / Completed / Stopped / Recovered / Unknown | `idle` | **green** | `●` |
| WaitingForInput / NeedsAttention | `waiting` | **red** | `●` |
| Failed / SessionLost | `error` | **red** | `●` |
| Manual override (`Ctrl-s`) | the chosen label | **cyan** | `●` |

Two rules worth designing around:

1. **A manual override takes color priority but never hides the real lifecycle
   state.** Both must remain readable.
2. **Attention beats busy.** On a project tab, "an agent needs input" outranks
   "an agent is working" outranks "manual" outranks "idle".

Status is never inferred from terminal output — it comes from explicit agent
lifecycle hooks — so the UI can be honest: unsupported agents stay **neutral**
rather than guessed. The design needs a credible "we don't know" state.

### Palette

The TUI paints with the **ANSI 16-color palette** on the user's terminal
background (near-black in practice), plus three literal RGB values:

- active project tab background `rgb(16, 38, 68)`
- terminal selection background `rgb(58, 90, 138)`
- the header ramp / blocks in cyan

Semantic assignments to carry over: **yellow** = selection / focus / warning
(and the active *agent* terminal tab), **cyan** = accent, interactive, upstream
info (and the active *shell* tab), **green** = healthy / idle / added / a
positive action, **red** = attention, error, destructive, close controls,
**magenta** = base drift and recovered sessions, **blue** = notifications and
inherited-from-global, **gray / dim gray** = secondary and unknown, **white +
bold** = primary text and selected rows.

The FlightDeck marketing site (`web/`) already uses a matching brand palette
worth aligning to: background `#07111f`, foreground `#edf4ff`, muted `#a9b8cf`,
accent `#61dafb`.

**Please design a light theme too**, or state explicitly why the app is
dark-only. The TUI inherits the terminal's theme and therefore ducks this
question; a browser app cannot.

---

## 8. States that must be designed

Beyond the happy path:

- **No projects / no sessions** — the first-run empty state (`No tabs. Ctrl-n to
  create.`).
- **Worktree being created** — spinner + `creating worktree…`, several seconds.
- **Git status not yet collected** — `git: ?` / `git: ?` in the info bar.
- **Agent exited / session lost** vs **agent restarting** (`Ctrl-r`).
- **Dirty base repository** — persistent, deduplicated **warnings** that stay on
  screen because merge is disabled.
- **Long-running destructive operation** — rebase, merge-back, abandon; each can
  fail (a rebase *aborts on conflict*) and the failure must be reported honestly.
- **Update available** — the non-modal status-bar chip.
- **Split view** with 2, 3, 4+ terminals.
- **Very long session names / branch names** — the TUI truncates with `…` at 28
  columns; decide the web equivalent.

### Web-only states the TUI never had — please design these

1. **Connection status.** The browser is remote from the FlightDeck process.
   Connected / reconnecting / disconnected / version-mismatch all need a
   persistent, honest indicator, and terminals must show clearly when their
   contents are stale.
2. **Authentication / pairing.** The desktop app trusts whoever is at the
   keyboard. A web app cannot. FlightDeck already has a QR + 4-digit-code pairing
   flow for its phone remote (§6.1) — the natural move is to reuse that visual
   language for browser pairing.
3. **Multiple viewers.** Two browser tabs, or a browser plus the desktop TUI, can
   be driving the same instance. Show it.
4. **Notifications.** The desktop posts native OS notifications with sounds on
   the edge from working → finished / waiting / failed. In a browser that becomes
   Web Push / permission prompts / an in-app activity feed — design the
   substitute, including the permission-request moment.
5. **Mobile / narrow viewport.** Not the primary target (that's the phone app),
   but a browser will be opened on a tablet. State how the three-level hierarchy
   collapses.

---

## 9. Things that are desktop-only

Design around these rather than for them:

- **Open worktree in file manager** (`Alt-o`) opens the *host's* Finder/Explorer.
  From a browser this is a host-side action with no visible result in the tab —
  it needs different framing (or hiding).
- **`e` edit file in `$EDITOR`** in the configuration manager spawns a terminal
  editor on the host.
- **Clipboard.** Drag-to-select copies to the *host* clipboard in the TUI; in a
  browser it must be the *viewer's* clipboard, which requires a user gesture.
- **Quit** (`Ctrl-q`) shuts down FlightDeck itself, killing agents — from a
  browser that is a dangerous button and needs to look like one.

---

## 10. Safety invariants the design must respect

FlightDeck's credibility rests on a strict Git ownership boundary, and the UI is
part of how that's honored:

- **Destructive actions are always confirmed**, with the consequence spelled out
  in the dialog title (see the exact copy in §6.2 — note how it names the
  branches, the commit counts, and whether a running agent will be stopped or
  have its HEAD rewritten). Do not soften this copy into "Are you sure?".
- **Abandon** is the one truly destructive operation; on the phone remote it
  requires typing the session name to confirm. Consider whether the web app —
  also remote, also possibly on someone else's screen — should do the same.
- **Rebase rewrites history and aborts on conflict**; the dialog says so.
- Reads are frictionless; state-changing commands are explicit and acknowledged.

---

## 11. What we'd like back from Claude Design

In rough priority order:

1. **The main screen**, full-fidelity, in a realistic populated state: 3 projects,
   4–5 sessions across the status spectrum (working, idle, waiting, error, manual
   override, creating), a live agent terminal with real ANSI output.
2. **The same screen in App mode** vs **Terminal mode**, showing exactly how focus
   and mode are communicated.
3. **Split view** with 3 terminals.
4. **The command palette**, populated and filtered.
5. **The New Agent Session Tab dialog**, in both branch and run-from-base states.
6. **The configuration manager**, showing the global/project scope switch and the
   three origin tags.
7. **One destructive confirmation** (rebase or abandon) as the template for the
   whole dialog family.
8. **The connection/auth story**: pairing screen + disconnected state.
9. **A narrow-viewport version** of the main screen.
10. **The color and type system** as a reference sheet: the semantic palette from
    §7 mapped to concrete values, in both themes if you do two.

---

## 12. Reference material

In this repository:

- `specs/screenshot.png` — the desktop TUI in its normal state (project tabs,
  agents sidebar, agent terminal, git info bar, status bar in App mode with the
  update-available chip).
- `README.md` — "Keyboard model", "Screen layout", "Multiple projects", "Agent
  status indicators" sections.
- `specs/SPECS.md` §19–§24 — terminal model, main layout, git status panel,
  interaction model, keyboard modes, agent status detection.
- `specs/MOBILE_REMOTE_BRIEFING.md` and `specs/MOBILE_REMOTE_PRD.md` — the
  *curated* phone companion. Useful for the status vocabulary and the safety
  invariants; **not** the model for this web app's scope.
- `specs/REMOTE_PROTOCOL.md` §8–§9 — the existing wire protocol for remote
  control (state snapshots, status updates, git detail, shell I/O, commands).
  Relevant because the web app will likely extend it rather than invent one.
- `web/src/app/globals.css` — the marketing site's brand palette.

### Screenshots to supply alongside this document

`specs/screenshot.png` covers the main screen only. The following are worth
capturing and attaching, because prose cannot substitute for them:

1. **Main screen in Terminal mode** with the live-pane border on, so the
   focus treatment is visible.
2. **The command palette** open and populated (two-column grouped list).
3. **The help overlay** (the full keybinding table as it actually renders).
4. **Split view** with 2–3 terminals side by side.
5. **The New Agent Session Tab dialog**, both with a branch name typed and with
   "run from base" toggled on.
6. **The configuration manager**, on both the Global and Project scope, showing
   the `(set here)` / `(global)` / `(default)` origin tags.
7. **A destructive confirmation** — the rebase or abandon dialog.
8. **The git status overlay**, ideally with a PR URL present.
9. **The remote pairing overlay** with the QR code and countdown.
10. **A sidebar with a busy mix**: a working spinner, an idle session, a waiting
    session, a manual override, and one `creating worktree…`.
11. **Narrow terminal** (~80 columns) so the narrow logo variant and the
    truncation behavior are visible.
