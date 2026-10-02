# FlightDeck Desktop — Requirements & Decisions

**Status:** preview. The Projects view and Mission control are implemented and
run on macOS. Linux and Windows have never been built or run. No packaged
release has been published.
**Epic:** `remote-control-bmej` · **This document:** `remote-control-bmej.1.5`
**Design:** Claude design canvas
<https://claude.ai/artifact/TRyC2w8Rh49UbvqtKkGotM> (artboards A1–A3 and the
shortcut map; see §5). The design files are not vendored in this repository.
**Sources consolidated here:** `desktop/NOTES-M0.md`, `desktop/NOTES-M2.md`,
`desktop/PACKAGING.md`, and the code under `src/host`, `src/view`,
`src/app/keymap`, `src/app/activity` and `desktop/`.

This is the living spec for the native desktop front-end (crate and binary
`flightdeck-desktop`, built on GPUI). The per-milestone notes in `desktop/`
stay as the record of how each piece was verified; this file holds the
decisions and the open questions, and says where they came from. Where a
decision has a known cost, the cost is written next to it.

---

## 1. What we are building

A native window for the same FlightDeck the TUI is: project tabs, one row per
agent, the active agent's terminal, a git strip, plus a second view, **Mission
control**, that shows every live session across every open project as tiles.

It is a second front-end over one core, not a second product. The window drives
the same `AppHost` the TUI drives, so git safety, worktrees, agent status,
persistence, notifications, isolated mode and the keymap behave the same way.
It does not embed the web server (`specs/WEB_INTERFACE.md`) and is not the phone
companion; it can pair with the phone through the same overlay the TUI uses.

### Non-goals

- Replacing the browser or phone surfaces.
- A second implementation of any FlightDeck rule. If the GUI needs a rule, it
  goes in the core and the TUI gets it too (D5).
- A light theme in this milestone. The theme is a set of semantic tokens
  (`desktop/src/theme.rs`), so one can be added; none is designed.

---

## 2. Decision log

### D1 — GPUI: go · **decided, macOS-only evidence**

GPUI is the toolchain for the desktop front-end. The go decision rests on four
spikes (M0), all run on one machine (macOS, Apple M2 Pro, Xcode 27, rustc
1.96.0):

| Spike | Question | Result |
| --- | --- | --- |
| S1 window | Does a themed window with custom chrome build and open from crates.io alone? | Yes. 1280×800 window (minimum 900×560), 44 px titlebar, 272 px sidebar, 30 px status bar; verified by window enumeration, and later by rendered frames. Clean build 89 s debug, 206 s release; binary 79 MB debug, 17 MB release (no LTO or strip). |
| S2 terminal element | Can GPUI draw a real terminal, including agents' TUIs? | Yes. Rendered frames were inspected for a glyph fixture (box drawing, quadrants, wide glyphs), interactive bash, vim on the alternate screen, and the first screens of opencode, claude and codex. Window resize reaches the PTY. |
| S3 keyboard parity | Can every TUI chord work, with Option acting as Alt? | Yes. A chord matrix of every table entry was driven through GPUI's real key dispatch on its headless test platform, and passes on macOS. |
| S4 performance | Is it fast enough? | Yes, on macOS (release builds; method and full tables in `desktop/NOTES-M0.md` "Performance (S4)"). Key → glyph p50/p95 about 6/9.5 ms in the GUI against about 55/60 ms in the TUI, whose 50 ms poll sets its floor. `cat` of 50 MiB and `seq 1 2000000` run at the PTY ceiling in both. Idle CPU is about 0.8% with 4 live terminals and about 1% in the full app, with 0 idle frames/s. Mission control with 4 waiting tiles uses 1.7–2.5%. Open: 4 *working* tiles cost 6–21% because the spinners redraw at display rate (being throttled), and RSS is about 100 MB against the TUI's 11 MB. |

> **Risk accepted:** everything above is verified on macOS only. Linux (X11 and
> Wayland over wgpu/Vulkan) and Windows (DirectX 11) are unverified: not built,
> not run, no key matrix, no packaging run. The dependency graph and build
> scripts say they should work; that is a reading, not a result.

### D2 — The pin: `gpui-pre =0.3.7` + `gpui-component 0.7.0`, from crates.io

| `desktop/Cargo.toml` name | crates.io package | Version |
| --- | --- | --- |
| `gpui` | `gpui-pre` | `=0.3.7` |
| `gpui_platform` | `gpui-pre-platform` | `=0.3.7` (features `font-kit`, `wayland`, `x11`; `runtime_shaders` via our default feature) |
| `gpui-component` | `gpui-component` | `0.7.0` |

`gpui-component` 0.7.0 depends on `gpui-pre = "=0.3.7"`, so the component
library fixes the GPUI version. `gpui-pre` is the Zed team's own crates.io
snapshot of `gpui` (same repository, Apache-2.0), published roughly weekly.
There are no git dependencies.

**Rejected:** crates.io `gpui` 0.2.2 (predates the platform split; no current
`gpui-component` builds against it); `gpui-ce` (community fork, different
bindings, not what `gpui-component` uses); a pinned git rev of `zed` (drags the
monorepo into every build and yields two copies of gpui next to the component
crate's own); `gpui-kit` (viable, but hides the pin behind a re-export).

**Upgrade policy.**
1. Move all three together and let `gpui-component` lead. Never bump `gpui-pre`
   alone.
2. Upgrade on purpose, not on a timer: when a fix or component is needed, at
   most about once per milestone, each upgrade a PR with nothing else in it.
3. On every upgrade: `cargo build -p flightdeck-desktop --locked`, clippy,
   tests, a real launch, the licence scan (D3), and the root-package
   `cargo tree` diff (the TUI's graph must not change).
4. If an unreleased upstream fix is ever needed, use `[patch.crates-io]` so
   there is still exactly one gpui.

> **Cost accepted:** a pre-1.0 weekly snapshot whose API breaks between minors,
> and a hard dependency on one component library's release cadence.

### D3 — Licences: nothing copyleft in the linked graph

A scan of the full resolve (all targets, normal and build edges from
`flightdeck-desktop`, 1003 packages at the time) found no GPL-only, AGPL,
LGPL-only or SSPL crate. None of Zed's GPL crates are in the graph; only
`gpui-pre-*` and its Apache-2.0 helpers come from Zed. Dual-licensed crates
(`self_cell` Apache-2.0 OR GPL-2.0-only, `r-efi`, `termina`) are taken under the
permissive option. MPL-2.0 file-level copyleft applies to `dwrote` and
`option-ext`, which are linked unmodified (Windows and font loading); shipping
them requires pointing to their source. `cbindgen` (MPL-2.0) is a build tool
and is not linked.

Zed's `terminal_view` (GPL) was not read or copied; the terminal element uses
only GPUI's public API. The bundled Geist and Geist Mono fonts (v1.7.2) are SIL
OFL 1.1 and ship unmodified with `desktop/assets/fonts/OFL.txt`.

> **Follow-up:** the scan was a one-off script that was not committed. Putting
> it, or `cargo-deny` with an allow-list, in CI is not done.

### D4 — Terminal emulator: `alacritty_terminal` for the desktop, `vt100` stays in the TUI

The desktop's terminals run on `alacritty_terminal` 0.26 (emulation core only;
its `tty` and event loop are unused, because our PTY stays behind
`PtySession`). The TUI stays on `vt100` 0.16.2.

Why alacritty for the desktop: it reflows on resize (vt100 truncates and loses
text, and can panic when shrinking through a wide character, which `Vt100Grid`
contains by rebuilding the parser); it honours synchronized output (`?2026`),
removing redraw flicker from Claude Code and opencode; it answers the capability
queries agents send (DSR, DA, OSC 4/10/11/12); it implements more SGR and OSC;
and Alacritty and Zed both maintain it. Its gaps, `?47` and X10 mouse (`?9`),
are legacy modes none of the tested agents (claude, codex, opencode) use. Real
agent screens parsed cleanly on both emulators.

Why the TUI stays on vt100 for now: in the TUI a real host terminal sits
outside FlightDeck, so answering OSC 11 or DA with FlightDeck's values would
misreport that terminal; reflow and sync buffering change what users see during
resizes; and the TUI's render tests pin vt100 behaviour. Switching is one line
(`TUI_EMULATOR` in `src/terminal/grid/mod.rs`) but not a pure swap; it is a
separate piece of work.

**The seam.** `src/terminal/grid/` in the core defines `GridView` (read-only:
size, per-cell grapheme, colours and attributes, cursor, modes, scrollback
offset) and `TerminalGrid: GridView` (feed, tick, query replies, resize,
scrollback, selection). `vt100_grid.rs` and `alacritty_grid.rs` are the only
files that name an emulator crate; `testing::FakeGrid` covers tests that need no
parser. The TUI renderer, `Terminal`, the mouse encoders and the desktop
element all go through the traits. Web replay streams raw PTY bytes and touches
none of this. The alacritty implementation lives in the core, not in `desktop/`,
so the conformance fixtures (`grid/fixtures.rs`) run both backends in the root
gate, and the root graph stays pure Rust (no `-sys` crate, no `cc`, checked per
target).

> **Cost accepted:** two emulators to keep conformant, and the TUI's library
> compiles alacritty's unused `tty`/`event_loop` modules.
> **Gap:** host terminals in the GUI still use `TUI_EMULATOR` (vt100), not
> alacritty, and their OSC 10/11 default colours are not set from the theme.

### D5 — One core, two front-ends: crate and module layout

The root is a cargo workspace, `members = [".", "desktop"]`, with
`default-members = ["."]`. Every root command (`cargo build`, `cargo test`,
`cargo clippy --all-targets`, cargo-dist) still covers only the `flightdeck`
package, so GPUI's graph never enters the TUI's gate or its pure-Rust Windows
build. Build the GUI with `-p flightdeck-desktop`. `remote/` stays a separate,
excluded workspace. The root carries a `[workspace.package]` table that mirrors
`remote/Cargo.toml`'s only because of a cargo nested-workspace quirk; keep the
two in sync. `desktop/` sets `dist = false` so cargo-dist does not release it.

**Core (`flightdeck`, `src/`)** owns everything that is a rule:

| Module | Role |
| --- | --- |
| `host` | `AppHost`: the front-end-neutral application. Owns projects, services, the turn (`tick`), `HostEvent` in, view models and `OverlayView` out. |
| `view` | Pure view models (project tabs, agent rows, git strip, mode bar, prompts, Mission control). They take the clock as a parameter. |
| `app/keymap` | The one keymap table: chords, contexts, entries, PTY encoders. |
| `app/activity` | Session activity: active, recent, needs-you, and the Mission control ordering. |
| `terminal/grid` | The emulator seam (D4). |

**Desktop (`flightdeck_desktop`, `desktop/src/`)** draws and translates:

| Module | Role |
| --- | --- |
| `app` | Start-up: arguments, `AppHost::open` on the working directory, GPUI, fonts, theme, keys, the window, the quit hook. |
| `root` | The no-repository launch: an empty state with "Open project…" and remembered projects (D9). |
| `shell` | `FlightDeckWindow`: layout, key contexts, focus follows mode, the overlay layer, the update banner. |
| `host` | `HostModel`: the `AppHost<'static>` inside a GPUI entity and its 16 ms turn loop. |
| `commands` | Keymap entry → `HostEvent`, one function, used by chords, buttons and menu items. |
| `views` | `titlebar`, `sidebar`, `git_strip`, `status_bar`, `icons`, `mission/` (grid, tile, prompt): pure renders of the view models. |
| `overlays` | Palette, dialogs, confirmations, help, about, config manager, pairing/remote, update banner: pure functions of `OverlayView`. |
| `keys` | Keymap table → GPUI bindings, chord spelling, terminal key handling, IME, paste. |
| `terminal` | The terminal element and view over a `TerminalSource`. |
| `selfupdate` | Daily check, install-kind detection, download and verified swap (`desktop/PACKAGING.md`). |
| `menus`, `notify`, `theme`, `fonts`, `assets` | Native menu bar, Dock badge and attention, semantic tokens, bundled Geist fonts, embedded icons. |

**Invariants.** The GUI never reimplements a rule: filtering, selection,
validation and every guard live in the host, so the GUI cannot drift from the
TUI. `AppHost<'a>` borrows its services and is `!Send`; the app leaks the real
services once (`RealServices::leak`) so the host is an `AppHost<'static>` in an
ordinary entity on the main thread. Teardown (`stop_services`, `persist`,
`terminate_sessions`, `cleanup_isolated`) runs once through GPUI's
`on_app_quit`, for every way out: window close, Cmd-Q, Ctrl-q, a confirmed quit,
SIGTERM/SIGINT/SIGHUP. The core's `testing` feature (an `AppHost` over fakes) is
enabled only in `desktop`'s dev-dependencies.

### D6 — Key contexts and the keymap

There is one keymap table, in the core. The desktop generates its GPUI bindings
from it (`keys::register`); no chord is written by hand in `desktop/`.

| GPUI context | Table context | Where it sits | Applies |
| --- | --- | --- | --- |
| `Global` | `Global` | window root | both modes; loses to anything deeper (so a text field in an overlay keeps its own Alt-Left). Bound with a named context because GPUI ranks a context-less binding above every context. |
| `Terminal` | `Terminal` | the focused terminal element | Terminal mode only. Unbound keys go to the PTY as the TUI's bytes (`encode_pty`), printable text through the IME input handler. |
| `App` | `App` | the element focused in App mode | App (command) mode only. |
| `Overlay` | none | the overlay layer | Every `Global` chord is bound to `NoAction` under it, so `Shift-Left` or `Alt-Up` cannot switch projects behind a dialog, as the TUI's modal swallows them. |

Two rules follow from how GPUI matches contexts: `App` must never be an
ancestor of `Terminal` (bare Up and `Ctrl-n` would fire inside a terminal), and
`Overlay` must never be an ancestor of `Terminal` either.

**Mode follows focus.** Focus-follows-mode: a sidebar click selects the agent
and enters App mode (Alt-2's command plus `FocusApp`); `Enter` focuses the
terminal; `Alt-Esc` (or `Shift-Esc`, or F2) leaves it.

**Option-as-Alt.** Bound chords match on the physical key, so Option always
works as Alt for `Alt-1..9`, `Alt-o`, `Alt-h`, `Alt-Esc` and `Alt-arrows`, with
no "Use Option as Meta" setting needed (the TUI needs it). For an *unbound*
Option+key in Terminal mode the policy is `OptionKey`: `Compose` (the macOS
default) types the composed character, `Meta` sends ESC + key, as the TUI does
with Option-as-Meta on. Compose is the default because German, French and Nordic
layouts put `@ [ ] { } | \ ~` behind Option; the cost is readline Meta-word
motions for US-layout users. It is not a setting yet (`remote-control-9diy`). On
Linux and Windows the policy makes no difference.

**F2.** `[ui] use_f2_to_leave_terminal_focus` is read once at start-up (the
launch project's effective value, or the global config with no project) and
fixes the keymap for the process; a change takes effect on the next launch.

**Cmd is left to the platform** on macOS. No FlightDeck chord uses Cmd, except
Cmd-V (paste, bound exactly) and the platform-convention Cmd-, / Cmd-O / Cmd-Q
in the menu bar. Cmd-W is deliberately unbound. A Cmd keystroke that is not bound
propagates to the platform (Cmd-C, Cmd-Q). The TUI's leniency does not extend to
Cmd (Cmd-Shift-V is not a paste). Paste is Ctrl-Shift-V elsewhere.

**Alt-m toggles Mission control, in App mode only.** `Alt-m` is taken inside the
terminal: zsh's emacs keymap binds Meta-m, and Claude Code falls back to it for
cycling permission modes where Shift-Tab is unavailable (Windows). A global
binding would swallow both on every OS. From a focused terminal it is
`Alt-Esc`, then `Alt-m`. The binding is exact, so Cmd-Alt-m stays the
platform's. A view-switch control in the titlebar and a View menu item exist for
the mouse.

**Leniency.** The TUI's `Trigger::tolerate` (Ctrl-Shift-g still opens the
palette) is not expanded into extra GPUI bindings; the key-down fallbacks look
the chord up in the table, so results match the TUI.

> **Not verified:** any of this on Linux or Windows (Windows AltGr, Linux
> `key_char` for Alt+letter, ibus/fcitx), and any of it with a physical
> keyboard on macOS: real Option composition under a German layout, an active
> CJK input source taking printable keys before the keymap, press-and-hold, a
> real IME candidate window.

### D7 — Mission control: what is shown and in what order

Mission control (designs A2 and A3) lists every live session across **all**
open projects. Its view model is `flightdeck::view::mission`, pure and shared;
the desktop only draws it. Persisted view state (which view is showing, the
scope) lives in `~/.flightdeck/workspace.json` (`MainView`, `MissionScope`).

- **Tiles.** Every session that is working, waiting for input, or needs
  attention (`app::activity::is_active`), whatever the scope's window says. An
  active session is always shown.
- **Cards.** Sessions with output, a status change or a git change inside the
  scope's window (`is_recent`) that are not live: compact cards.
- **Quiet.** Everything else in scope: counted, not shown.
- **Order (V1).** Needs you first (waiting for input, needs attention), then
  working, then most recently updated; ties break by project, then name, then
  id, so equal sessions never shuffle between frames (`compare_sessions`).
- **Scope.** Two settings: a recency window for the cards (`Day` — the default,
  24 h; `Week`, 7 d; `All`) and an optional project filter, stored by the
  project's absolute repository root (stable across reordering and restarts; a
  root that is no longer open reads as "all projects"). The filter narrows
  tiles, cards and the quiet count.
- **Needs-you count.** The view switch's badge, the Dock badge and the window
  attention flag use it. It is **not** narrowed by the project filter: it is an
  alert about the whole workspace and must not go quiet because the grid is
  filtered to one project.
- **Alt-N in Mission control** reaches the tile at that 1-based position in tile
  order, for the first nine.
- Grid columns and keyboard movement are computed in the core
  (`grid_columns`, `move_selection`); the GPUI layer draws what they decide.

> **Cost accepted:** a session's tile is only as informative as the view models
> carry (see §7, "Waiting for approval · Bash(…)").

### D8 — Inline Approve / Deny: scope

A waiting tile can be answered without opening the session. The scope is
deliberately narrow.

- **Nothing new detects prompts.** Detection is FlightDeck Remote's (needs-input
  edge, the agents' prompt sidecars, the Claude session-file ingest). With Remote
  off, `AppHost::track_prompts` runs only that part in a never-paired bridge
  that transmits nothing; the TUI does not call it and pays nothing. A tile
  offers exactly what the phone would.
- **Binary.** A permission prompt with both an allow and a deny option: Approve
  (allow once) and Deny.
- **Single-select.** One single-select question with at most four options
  (`MAX_INLINE_OPTIONS`), on a backend that has an answer keystroke for every
  option: one button per option.
- **Unsupported.** Anything else — checklists, multi-question forms, more than
  four options, a custom agent whose keystrokes FlightDeck does not know — shows
  an "open the session" action instead of guessing.
- **Same bytes as the phone.** A click becomes the phone's `permission_decision`
  (`HostEvent::AnswerPrompt`) and runs through the same translator, so the
  keystrokes a click types are those a phone tap types, by construction.

> **Not verified:** the buttons have not been exercised against a live agent's
> prompt (§7).

### D9 — Launch and shell behaviours

- **In a repository:** the window opens on that project plus the remembered
  ones. **Outside a repository:** the window opens on an empty state with "Open
  project…" and the remembered projects (`host::recent_projects`); choosing a
  folder runs `AppHost::open` on it (`HostEvent::OpenProject`, so the git check
  and its refusal are the TUI's) and swaps the shell in.
- **Launch environment.** Started outside a terminal (Finder, the Dock, a Linux
  launcher: stdin is not a TTY), the app adopts the login shell's environment
  before anything else runs (`$SHELL -l -i -c 'env -0'`, 5 s bound, best
  effort; `shell_env`). Without it, `PATH` is the session manager's
  (`/usr/bin:/bin:/usr/sbin:/sbin` on macOS), so `git` is Apple's `xcrun` shim
  and the agents are not found. Windows does nothing: Explorer already passes
  the user's environment.
- **`--isolated`/`-I`:** New agent, project switching and Open project are drawn
  disabled (`KeymapEntry::refused_when_isolated`); the host refuses the chords
  with the TUI's message; an `ISOLATED` badge shows; nothing is saved. Launching
  isolated without a project is still the TUI's error.
- **Attention.** Banners and sounds are the core's, through the TUI's
  `SystemNotifier`. The macOS Dock badge is the needs-you count; a new waiting
  agent asks the OS to flag an inactive window. Windows taskbar overlay and a
  Linux badge are not done (GPUI exposes neither).
- **Menus.** The macOS menu bar is generated from the keymap table; a menu click
  and a chord both end in `commands::perform_entry`. Windows and Linux have no
  menu bar; the palette (`Ctrl-g`) and F1 help are the equivalent.
- **PTY sizing.** The terminal element measures its cell and bounds each frame;
  the next turn resizes every project's PTYs. Before the first frame, agents
  spawn at 123×38. In split view each pane measures its own terminal, and the
  host model resizes that one terminal (`AppHost::resize_terminal`), as the
  TUI's `sync_terminal_sizes` does.
- **Terminals within a session.** The sidebar's nested rows under the selected
  agent are its terminal tabs; there is no tab strip above the terminal.
  Left/Right (APP), Alt-Left/Right (both modes), a row click, Ctrl-t and
  Ctrl-w are the TUI's. Split view (`Ctrl-b`) lays them side by side with a
  header per pane (`desktop/NOTES-M2.md`, "Terminals within a session").
- **Spinners.** The working arc turns in 8 steps a second from one app-wide
  clock that stops while the window is inactive or hidden
  (`desktop/src/views/spinner.rs`).
- **Self-update.** Once a day the app looks for a newer `desktop-v*` release and
  shows a banner (never a modal) whose offer depends on the install kind
  (`desktop/PACKAGING.md`).

---

## 3. Verification status

| Piece | Status |
| --- | --- |
| Shell, overlays, Mission control, keys | GPUI tests over a live `AppHost` on fakes, and rendered frames, on macOS. |
| The real app | Launched in a throwaway repository with a throwaway `HOME` and a stand-in agent: SIGTERM after 10 s exits 0, children gone, `state.json` rewritten. |
| Linux, Windows | Never built or run. |
| macOS `.app`, updater, CI workflow | Bundle built and launched unsigned; workflow passes `actionlint` but **CI has never run**; no release has ever been created. |
| Pixels | Compared against the A1 mockup by eye from offscreen frames. Native chrome (traffic-light inset, Linux/Windows decorations) is not in those frames. |

---

## 4. Crate and build facts

- Workspace and lock: one `Cargo.lock`. The root package's dependency counts are
  unchanged by the workspace (367 / 257 / 362 on darwin / windows-msvc / linux);
  the only lock change for the TUI graph was a patch bump of the `futures-*`
  crates that `gpui-pre` requires.
- macOS: the default feature `runtime-shaders` compiles Metal shaders at launch,
  because since Xcode 26 the `metal` compiler is a separate download. Release
  packaging should build with `--no-default-features` on a machine that has the
  Metal Toolchain (not verified).
- Windows: MSVC with the Windows 10/11 SDK (`fxc.exe`, `rc.exe`); unlike the TUI
  this is not a pure-Rust build and cannot use the windows-gnu setup.
- Linux: `build-essential pkg-config cmake clang libxkbcommon-dev
  libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev libxcb1-dev
  libfontconfig-dev libfreetype-dev`; at run time a Vulkan loader and driver.
- `--features spike-snapshot` (off by default) enables GPUI test-support and the
  `--spike-snapshot PATH [--spike-keys …] [--spike-wait-ms N]` flags, which type
  through the real key handler and write the window's own rendered frame; no
  Screen Recording permission is needed.

---

## 5. Design coverage

Canvas: <https://claude.ai/artifact/TRyC2w8Rh49UbvqtKkGotM>. The design files
are not copied into this repository. Summary of the artboards:

- **A1 — Projects view.** Titlebar with the project tabs and the view switch;
  a sidebar with one row per agent (name, agent, branch, status glyph and text,
  age, diff); the git strip above the main area (branch, ahead/behind, Push,
  Pull base, Finish); the focused terminal in the main area; a status bar with
  the mode and hints.
- **A2 — Mission control, grid.** A tile per live session across all projects,
  needs-you tiles first and drawn loudest, compact cards for recent quiet
  sessions, a quiet count, the scope menu (window and project filter), and the
  needs-you badge on the view switch. A waiting tile carries inline Approve /
  Deny.
- **A3 — Mission control, focus.** The selected session full size with its
  terminal, and the other sessions in a strip above it. There is no layout flag:
  App mode shows the grid (A2), Terminal mode shows the focus view (A3), so
  `Enter` opens the selected tile and `Alt-Esc` returns to the grid.
- **Shortcut map.** Every chord is the TUI's; the map adds Alt-m (Mission
  control, App mode only), and marks Cmd as the platform's on macOS.

The mockup shows details the view models do not carry yet (§7).

---

## 6. Open questions — for the owner

These are open. Where a recommendation is given it is the author's, not a
decision.

### Q1 — Keep the ratatui TUI · **decided 2026-10-01: keep it** (`bmej.1.6`)

**The TUI stays the main app.** The desktop app ships next to it as a preview
for testing and replaces nothing. The TUI keeps running on `AppHost` as a thin
client (D5) and stays the reference for keymap and byte parity.

**Maintenance cost, accepted:** two renderers over shared view models and one
shared keymap. Every new view-model field or chord needs a change in both
front-ends, or an explicit "GUI only" note. The two use different emulators (D4),
so both conformance suites stay in the root gate, and the TUI's render tests
remain their own maintenance surface.

### Q2 — Binary and launch layout · **decided 2026-10-01: separate app** (`bmej.1.7`)

**A separate `flightdeck-desktop` binary (`FlightDeck.app` on macOS), next to the
`flightdeck` CLI and distributed separately.** It is released by
`.github/workflows/desktop.yml` on `desktop-v<x.y.z>` tags as its own GitHub
Release, apart from cargo-dist's `v<x.y.z>` CLI releases. Its self-updater
follows that tag line, and a Homebrew cask can follow later. Launched from
Finder, the Start menu or a launcher it needs no repository. Launched outside a
repository it shows a project picker (D9). It does not need the CLI installed.

The alternative was one binary with the GUI as the default and `--tui` for the
terminal. It was rejected because the CLI would link GPUI. That brings GPUI's
system requirements (Vulkan, X11/Wayland on Linux; the MSVC + Windows SDK build
on Windows) to SSH and headless installs, and loses the TUI's pure-Rust Windows
build.

### Q3 — Release identifiers · **partly decided 2026-10-01**

1. Bundle id **`agency.neworange.flightdeck.desktop`** — confirmed.
2. Tag scheme **`desktop-v<version>`**, released by its own workflow
   (`dist = false`), never by cargo-dist — confirmed.
3. Signing: macOS reuses the CLI's Developer ID secrets (`CODESIGN_*`).
   Notarization credentials and a Windows signing certificate are still to be
   provided (`desktop/PACKAGING.md`).
4. Homebrew tap: the existing `neworange-ruud/homebrew-tap` is assumed for a
   later cask.

### Q4 — Pull base in the GUI · **decided 2026-10-01: as built** (`bmej.4.8`)

The git strip's Pull base button dispatches the same event as `Ctrl-u`, with the
TUI's guards and messages and no extra confirmation, in both front-ends.

### Q5 — App icon · **open**

The iOS app icon is reused as-is for the macOS/Windows/Linux packages
(`desktop/packaging/icons/`). A designed desktop icon is needed before a public
release.

### Q6 — Linux and Windows support level · **open**

What "supported" means for the first packaged release: build and CI only, or
verified runs on named distributions and Windows versions. Until a machine is
available, neither platform has any verification (D1).

### Q7 — Performance budget · **open**

S4 measured the app on macOS (see D1's table). The terminals meet "no worse than the TUI" for latency and throughput. Still to decide: the budget for animated chrome (working spinners), whether about 100 MB of RSS is acceptable, and the numbers Linux and Windows must reach once they can be measured (`desktop/benches/perf.py` and `flightdeck-desktop --bench` re-run the suite).

---

## 7. Known gaps

From `desktop/NOTES-M2.md`, `desktop/NOTES-M0.md` and `desktop/PACKAGING.md`:

- **No PR or checks data.** The mockup's "PR #412 · checks ✓" has no source in
  the view models.
- **No "Waiting for approval · Bash(…)" detail** on rows and tiles; only the
  status text the view models carry.
- **Per-project PTY sizing.** One viewport for every project's PTYs (the GUI's
  chrome is the same in every project), except the selected agent in split
  view, whose terminals get their panes' sizes. A window resize during split
  view reaches the other projects only when split view is left.
- **Linux and Windows:** never built or run; no taskbar overlay or Linux badge;
  server-side decorations not seen.
- **Live validation of Approve / Deny** against a real agent prompt has not been
  done (D8).
- **Pairing with the iOS app end-to-end** from the desktop overlay has not been
  exercised.
- **CI has never run;** the release, signing, notarization, updater download and
  swap, Homebrew cask, `.deb`/`.rpm`/AppImage and MSI paths are written but not
  run.
- Host terminals use vt100, not alacritty, and OSC 10/11 defaults are not taken
  from the theme (D4).
- The context menu shows keycaps as trailing text, not aligned columns; the tab
  close `×` shows only on the active project tab.
- Every host notify re-renders the whole window; the terminal has no damage
  tracking.
- No physical-keyboard or IME verification on macOS (D6).
- Option-as-Meta is not a setting.
- The licence scan is not in CI.
