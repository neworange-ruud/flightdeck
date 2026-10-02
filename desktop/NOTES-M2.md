# flightdeck-desktop: M2 notes (the Projects-view shell)

Written for beads issues `remote-control-bmej.3.1`, `.3.3`, `.3.4` and `.3.5`.

## Bundled fonts

| Family | Files shipped | Used for |
| --- | --- | --- |
| Geist | Regular (400), Medium (500), SemiBold (600) | all UI text |
| Geist Mono | Regular (400), Medium (500), Bold (700), Italic, BoldItalic | terminals, keycaps, branch names and meta text |

- **Source:** the official release `vercel/geist-font` **v1.7.2**
  (<https://github.com/vercel/geist-font/releases/tag/v1.7.2>), asset
  `geist-font-v1.7.2.zip` (sha256
  `7fc800d2ac6b92844895196e5041aca55d814c15db70c44f79b3b83ab82b04e2`). The
  files are the zip's static cuts, `Geist/ttf/` and `GeistMono/ttf/`,
  unmodified.
- **Licence:** SIL Open Font License 1.1, copyright 2024 The Geist Project
  Authors. The licence text ships next to the fonts as
  `desktop/assets/fonts/OFL.txt`, as the OFL requires for redistribution. The
  OFL allows bundling in any software, including commercial software, as long
  as the fonts are not sold on their own and keep their names. We do not
  rename or modify them.
- **How they load:** `desktop/src/fonts.rs` embeds each file with
  `include_bytes!` and registers them with GPUI's text system
  (`TextSystem::add_fonts`) before the first window opens, so the app renders
  the same on a machine that has never installed Geist. The binary grows by
  about 1.1 MB.
- Only the weights the design uses are shipped. Anything else (a terminal
  program asking for a light weight) is synthesised by the platform from the
  nearest one.

## Module layout (binary crate unless noted)

| Module | Role |
| --- | --- |
| `app.rs` | Start-up: args (`--isolated`/`-I`, the `--spike-snapshot` flags), `AppHost::open` on the cwd, PTY seed, resume, `start`, GPUI + fonts + theme + keys + overlay bindings, the window, the quit hook |
| `host.rs` | `HostModel`: the `AppHost<'static>` entity, its turn loop, `dispatch`, viewport hand-off and teardown |
| `commands.rs` | Keymap entry → `HostEvent` in one function (`perform_entry`), used by the chord handler, every button and every menu item; keycap spelling per OS |
| `shell.rs` | `FlightDeckWindow`, the root view: layout, key contexts, focus-follows-mode, the overlay layer, the update banner |
| `views/` | `titlebar`, `sidebar`, `git_strip`, `status_bar`, `icons` — pure renders of the shared view models |
| `terminal/view.rs` | The terminal view, now over a `TerminalSource`: its own terminal (spike) or the host's on-screen one (app) |
| `assets.rs`, `fonts.rs` | Embedded SVG icons (`AssetSource`) and the Geist fonts |
| `shell_tests.rs` | GPUI tests of the window against a live `AppHost` over fakes |
| library: `keys`, `theme`, `overlays` | Unchanged seams from M0/M1 plus the overlay layer the shell mounts |

## How the host lives in GPUI

- **Lifetimes.** `AppHost<'a>` borrows its services and is `!Send`. The app
  builds the real services once and leaks them (`RealServices::leak`), so the
  host is an `AppHost<'static>` inside an ordinary GPUI entity on the main
  thread. Tests do the same with the core's fakes.
- **Turns.** A foreground task wakes every 16 ms and calls `HostModel::turn`:
  apply a measured PTY size (`resize_projects`), `AppHost::tick`, and
  `cx.notify()` when the host reports a change or at least once a second (for
  elapsed times). A host quit request or a SIGTERM/SIGINT/SIGHUP flag
  (`flightdeck::signals`) calls `cx.quit()` from there.
- **Input.** Chords are GPUI bindings generated from the keymap table; the
  root's one action handler and every control call `commands::perform_entry`.
  Terminal-mode keys go through `keys::terminal_key_down` (the TUI's bytes),
  printable text through the IME input handler, both as
  `HostEvent::TerminalInput`.
- **Sizing.** The terminal element measures its cell and bounds each frame and
  hands the size to the model; the next turn resizes every project's PTYs.
  Before the first frame, agents spawn at `NOMINAL_PTY_SIZE` (123×38).
- **Teardown.** GPUI's `on_app_quit` runs `stop_services`, `persist`,
  `terminate_sessions`, `cleanup_isolated`, once, for every way out (window
  close, Cmd-Q, Ctrl-q, a confirmed quit, a signal).

## AppHost additions

`project_tabs`, `agent_rows`, `git_strip`, `mode_bar`, `now_unix_secs`,
`active_terminal`/`active_terminal_mut`, `HostEvent::OpenProject(PathBuf)`
(the folder browser's own open path, so the git check and its refusal are the
TUI's), and `host::testing` (an `AppHost` over the fakes) behind the root
crate's off-by-default `testing` feature, enabled only in this crate's
dev-dependencies.

## Verification (2026-09-29, macOS, Apple M2 Pro)

- GPUI tests (`shell_tests.rs`): a tab per project; tab click and
  Shift-Left/Right switch project; a sidebar click selects the agent and enters
  APP mode (Alt-2's command + `FocusApp`); Alt-N, Up and Alt-Down navigate;
  for New agent, New shell, Push, Pull base, Finish, the Command field and the
  help hint, the click dispatches exactly the event its chord does; a disabled
  git button dispatches nothing; the overlay layer answers the live host
  (Ctrl-g, typing, Enter runs a row; Esc; typing into a prompt; a dialog
  button is `Choose(id)`; Global chords are inert under an overlay); `+` with
  a picked non-repository folder is refused with the TUI's message; teardown
  persists every project, once.
- The real app, in a throwaway repository with a throwaway `HOME` and a
  stand-in agent script: launched for 10 s, then SIGTERM — exit 0, both agent
  children gone, `state.json` rewritten, nothing on stderr. Rendered frames
  (`--features spike-snapshot`, `--spike-snapshot PATH`, `--spike-keys`) were
  compared with the A1 mockup: empty project, New Agent dialog, two agents with
  a live terminal typed into (`echo hi` ran), TERMINAL mode, the palette.

## Not verified / gaps against the design

- **Linux and Windows were not built or run.**
- **Native chrome is not in the offscreen frames**: the macOS traffic-light
  inset and the Linux/Windows server-side decorations were not seen.
- No row detail the view models do not carry: the mockup's "Waiting for
  approval · Bash(cargo fmt)" and "PR #412 · checks ✓".
- Mission control is a static segment (M5); only its needs-you count is live.
- ~~Split view (Ctrl-b) is not drawn~~: done, see "Terminals within a
  session" below.
- Host terminals use the TUI's emulator (`TUI_EMULATOR`, vt100), not the
  desktop's alacritty backend, and their OSC 10/11 default colours are not
  set from the theme.
- One size for every project's PTYs (the TUI's per-mode chrome differences do
  not apply to the GUI yet). Since `.3.6` the selected agent in split view is
  the exception: its terminals get their panes' sizes.
- The context menu shows keycaps as trailing text, not aligned columns; the
  close `×` is only on the active project tab.
- Every host notify re-renders the whole window (fine at this size; a
  terminal-only redraw path is a later optimisation).

## Platform layer (`remote-control-bmej.3.7`, `.3.8`, `.4.7`, `9diy`)

- **Menus** (`menus.rs`): the macOS menu bar is generated from the keymap
  table; items are the same `KeymapAction`s as the chords, so a menu click and
  a chord both end in `commands::perform_entry`. Cmd-, / Cmd-O / Cmd-Q are the
  only platform-convention shortcuts; Cmd-W is deliberately unbound. Windows
  and Linux have no menu bar; the palette (Ctrl-g) and F1 help are the
  equivalent, and no titlebar hamburger is built. Mission control joins the
  View menu when its view exists.
- **F2**: `[ui] use_f2_to_leave_terminal_focus` is read once at start-up
  (the launch project's effective value, or the global config when launched
  without a project) and fixes the keymap for the process; changing it takes
  effect on the next launch. Option-as-Meta is not a setting yet.
- **Attention** (`notify.rs`): banners and sounds are the core's, through the
  TUI's own `SystemNotifier`. The Dock badge is the needs-you count
  (`AppHost::needs_you_count`); a new waiting agent asks the OS to flag an
  inactive window. Windows taskbar overlay and a Linux badge are not done
  (GPUI exposes neither; see the module docs).
- **Isolated** (`--isolated`): New agent, project switching and Open project
  are drawn disabled (button, menu item) from
  `KeymapEntry::refused_when_isolated`; the host refuses the chords and menu
  actions with the TUI's message; the `ISOLATED` badge shows; nothing is saved.
- **No repository** (`root.rs`): the window opens on an empty state with
  "Open project…" and the remembered projects (`host::recent_projects`);
  choosing a folder runs `AppHost::open` on it and swaps in the shell.
  `--isolated` there is still the TUI's error.

## Terminals within a session and split view (`remote-control-bmej.3.6`)

- **The tabs are the sidebar's nested rows.** Under the selected agent the
  sidebar lists its terminals (`agent`, `agent 2`, `shell 1`, …, from
  `flightdeck::view::terminal_views`, the list the TUI's tab bar and split
  columns use); the focused one is drawn raised and a click selects it and
  enters TERMINAL mode. The main area is always the host's active terminal
  (`AppHost::active_terminal`), so switching needs nothing of its own.
- **Decision: no tab strip above the terminal.** The nested rows already
  show every terminal, which one is focused, and its command, one click away.
  A strip would repeat them. The one place a label is needed next to the
  terminal is split view, where several are on screen at once, so each pane
  has a header (label + command, the active one highlighted in the accent),
  as the TUI's split columns do. Nothing is drawn above a single terminal.
- **Split view (Ctrl-b)** (`views/split.rs`): the TUI's `draw_split_view`.
  One pane per terminal, left to right in tab order, equal widths with a
  hairline between. The active terminal's pane holds the window's one
  `TerminalView`, which moves as the focus moves, so keys, IME, selection
  and mouse reporting all go to the active terminal only. Every other pane
  is a read-only `PaneView`: the element's row cache, layout and glyph
  painting at the same font and padding (a terminal keeps its size when it
  gains focus), no cursor (the TUI shows only the active column's), dimmed
  by `Palette::pane_dim` at `PANE_DIM_ALPHA`. A click on a pane's header or
  body selects that terminal and enters TERMINAL mode (the TUI's column
  click). The toggle shows the TUI's own "Split view on." message.
- **Per-pane PTY sizes.** Each pane measures its grid; the host model keeps
  the sizes for the current (project, agent, pane count) and applies them on
  its next turn through the new `AppHost::resize_terminal(project, tab_id,
  target, size)` (resizes only on a change; unit-tested in
  `src/host/tests.rs`), with `AppHost::tab_terminal_at` to read one named
  terminal. That is the TUI's `sync_terminal_sizes`: out of split view every
  terminal of the selected agent is put back to the viewport each turn.
  While split view is on, the viewport (every other project's size) is not
  updated; a window resize in split view reaches the other projects when
  split view is left. The rest of the "one size for every project" gap
  stays: the GUI's chrome is the same in every project, so one size is
  right there.
- **Not done:** scrolling a pane that is not the active one (click it
  first), and split view inside Mission control's focus view (it keeps one
  terminal full size).

### The Child Terminal Navigation help, item by item

| Help row | Desktop | Test (`shell_tests.rs`) |
| --- | --- | --- |
| Ctrl-t — New child terminal | the keymap chord; also the sidebar's New shell button; the new shell is active (and a new pane in split view) | `ctrl_t_adds_and_ctrl_w_closes_a_terminal_by_the_tuis_rules`, `ctrl_b_lays_the_terminals_side_by_side_each_at_its_own_size` |
| Ctrl-w — Close active child terminal | the TUI's confirmation (`CloseTerminal { label }`), n keeps it, y ends its process tree; on the agent it is refused with "No child terminal selected." (the agent closes with its session, Ctrl-k, which asks first) | `ctrl_t_adds_and_ctrl_w_closes_a_terminal_by_the_tuis_rules` |
| Left / Right (or Alt) — Cycle terminal tabs | bare arrows in APP mode, Alt-arrows in both modes, wrapping; bare arrows in TERMINAL mode go to the program | `left_right_and_alt_arrows_cycle_the_agents_terminals`, and in split view `in_split_view_focus_moves_between_panes_and_only_the_active_one_gets_input` |
| Ctrl-b — Toggle split view | panes as above, off again restores one size | `ctrl_b_lays_the_terminals_side_by_side_each_at_its_own_size` |
| Mouse click — Select terminal tab | a nested sidebar row; in split view a pane header or body | `clicking_a_nested_terminal_row_selects_and_focuses_it`, `in_split_view_focus_moves_between_panes_and_only_the_active_one_gets_input` |

Rendered frames (`--features spike-snapshot`, a throwaway HOME and repo, keys
typed through `--spike-keys`) of split view with two and with three
terminals were checked by eye: equal columns, the active header underlined
in the accent, the other panes dimmed and cursorless, output in a read-only
pane drawn as the terminal element draws it.

## Spinners (S4 follow-up)

`views/spinner.rs`. The working arc no longer uses GPUI's `with_animation`,
which asks for a frame on every display refresh. It turns in 8 steps of 45°,
one every 125 ms, from one app-wide clock (`SpinnerClock`, a GPUI global).
Each arc is its own small entity (`Window::use_keyed_state`, so it lives as
long as the arc is on screen), embedded as a cached view; a step notifies
only those. GPUI still re-renders the views that contain an arc (the window
root, Mission control), because it has no narrower repaint, but the
cached tiles and the other arcs are replayed, not drawn again. The clock
runs only while at least one arc is alive and the window is active and
visible (`observe_window_activation`, `observe_window_visibility`, which is
`NSWindow.occlusionState` on macOS; on Windows a covered window still counts
as visible, see GPUI's `WindowVisibility`). Otherwise the timer stops and
the arcs hold their step. The rule is `SpinnerSchedule::running`
(unit-tested), and a GPUI test checks the clock stops for an inactive and a
hidden window and resumes after.

Measured with `perf.py mission-idle --seconds 20` (4 Mission control tiles,
release builds, the key window, M2 Pro, other builds running, load ~30):

| 4 tiles | before | after |
| --- | --- | --- |
| waiting, idle shells | 1.39 % | 1.44 % |
| **working**, idle shells | **19.48 %** | **2.87 %** |
| waiting, tickers | 1.93 % | 2.22 % |
| **working**, tickers | **21.35 %** | **3.42 %** |

The arcs now cost about 1.2–1.4 % of a core on top of the waiting case. In a
background or hidden window they cost nothing.

## Terminal element (production) (`remote-control-bmej.3.2`)

The element and view in `desktop/src/terminal/` now carry the whole mouse,
cursor and sizing behaviour, for the app's host terminal and the spike alike.
The reference for every rule is the TUI (`handle_mouse_project`,
`autoscroll_drag`, `handle_scroll` in `src/lib.rs`, `dim_terminal` in
`src/tui/render.rs`).

| Area | What it does | vs the TUI |
| --- | --- | --- |
| Selection | Left drag selects; release copies (`cx.write_to_clipboard`, the system clipboard) and leaves it highlighted; a click that selected nothing clears it. The drag follows the pointer outside the element and window (window-level listeners registered in `paint`). Cmd-C (macOS) copies too. | Same rule. The TUI copies with `tui::clipboard::copy` (pbcopy / clip / wl-copy, OSC 52 fallback); the GUI owns a clipboard. No double/triple click: the TUI has none. |
| Auto-scroll | A drag past the top or bottom edge scrolls one line every 50 ms, pointer held still or not, and pins the selection head to the edge row. | Same step and rate as `autoscroll_drag` (1 line per 50 ms loop tick). The GUI triggers past the grid edge, the TUI on the edge row (a TUI pointer cannot leave the window). |
| Shift | Shift-drag selects over a program that reports the mouse. | Same. |
| Mouse reporting | Presses, releases and the motion the mode asks for: X10 `?9` presses only (no modifier bits), `?1000` press/release, `?1002` + drag motion, `?1003` + hover motion over the grid, once per cell as xterm does. Left, middle, right. Encodings: default, UTF-8 `?1005`, SGR `?1006`, all through the core's `encode_mouse_button` / `encode_mouse_report`. | Same encoders, byte for byte. The TUI forwards left-button presses/releases and a drag motion in any mode; the GUI follows the mode exactly, and adds middle/right buttons and modifier bits (xterm's). URXVT `?1015` is not reported by either emulator, so neither front-end has it. |
| UTF-8 encoding | `?1005` is now real UTF-8 in the core encoder (coordinates past 95 take two bytes, up to 2015). | A core fix: before, UTF-8 was sent as the legacy bytes, wrong past column 95, in both front-ends. |
| Wheel | With mouse reporting, one wheel report per notch goes to the program (Shift does not override). Otherwise it scrolls history, 3 lines a notch; trackpad pixels accumulate into whole lines. On an alternate screen without reporting it does nothing. | Same routing and 3 lines (`SCROLL_LINES`). The spike's "arrow keys on the alternate screen" is gone: the TUI does not do it. |
| Scrollbar | A 4 px thumb at the grid's right edge (token `separator`), only while scrolled into history. No track, no dragging, no fade timer. | The TUI has none. |
| Input | A key or paste drops the selection and returns to the live screen: the host does it for the app (`write_active_pty`), the view for the spike. | Same (the same code, for the app). |
| Paste | The app's paste is the host's (`encode_paste`, bracketed under `?2004`); the spike now uses the same encoder. | Byte for byte. See the follow-up below. |
| Cursor | DECSCUSR block / underline / bar; a hollow block when the terminal lacks key focus or the window is inactive. Blinks only when the program asks for a blinking cursor, the terminal is focused and the window active: 500 ms phases, so at most 2 frames/s, and none otherwise. Typing shows it and restarts the phase. | The TUI hands the cursor to its host terminal and never blinks it. A steady cursor (every shell's default, `CSI 0 SP q` in alacritty) costs no frames. |
| Font size | `[ui] desktop_terminal_font_size`, default 13, 8–32, validated at load, layered like every `[ui]` key, a configuration-manager row (a new `FieldKind::IntegerChoice`, written as a TOML integer), read each frame so a save applies at once. macOS: Cmd-= / Cmd-+ / Cmd-- zoom a point, Cmd-0 resets; the zoom is a GPUI global (every terminal in the window follows) and not saved. | TUI ignores it. No zoom on Linux/Windows: Ctrl-= / Ctrl-- / Ctrl-0 belong to the terminal (Ctrl-- is readline's undo, 0x1f) and the keymap table has no such chord. |
| Dimming | In APP mode with `[ui] dim_terminal_in_app_mode` on (default), every glyph takes the `faint` token and loses bold; backgrounds and the selection keep their colours. | The TUI's rule (`!focused && dim_terminal_in_app_mode`), which keys on the mode only. An inactive window hollows the cursor but does not dim: the TUI setting is about App mode, and dimming whenever another window has focus would grey out an agent you are watching. |
| Resize | The element's cell count is the host's viewport (`HostModel::set_viewport`), applied to every project's PTYs on the next turn; FlightDeck Web's `HostState.geometry` and each terminal's `geometry` are the grid the PTY then has. | D4, unchanged. |

**Tests** (`desktop/src/terminal/tests.rs`, GPUI's test platform; the fake
PTY's input is compared with bytes built by the core encoders): copy on
release, and nothing before it; a drag that leaves the window; auto-scroll past
the top edge (3 ticks, 3 lines, the copy starts three lines higher) and the
bottom; Shift-drag over `?1000`; Cmd-C; every mouse mode × encoding (press,
drag, hover, release, right button; X10 on vt100, which is the only emulator
with `?9`); wheel to history (notches, trackpad pixels), to the program, and on
an alternate screen; bracketed paste; DECSCUSR shapes, hollow when blurred and
when the window is inactive; blink phases, restart on typing and stop when
inactive; font size from config and Cmd zoom; dimming in APP mode and with the
setting off; a window resize reaching the PTY and `AppHost::web_host_state`
(new, `testing` feature). Plus layout (dimmed palette), row cache (a blink
phase relays one row), scrollbar geometry, zoom rules, the UTF-8 encoder, config
validation, layering and the manager row.

**Looked at** (`--features spike-snapshot`, with a new `--spike-mouse "down:X,Y
move:X,Y up:X,Y wheel:N blur"` flag): a three-line selection ending mid-row;
the scrollbar after 20 wheel notches over `seq 1 300`; the underline and bar
cursors; the filled block; the hollow block after `blur`.

**S4 re-run** (release builds, `perf.py gui-latency gui-idle app-idle --keys
300`, M2 Pro, 2026-09-29; "before" is `d8a06b9`, run right after):

| | before | after |
| --- | --- | --- |
| Key → glyph p50 / p95 | 6.33 / 10.47 ms | 6.32 / 10.41 ms |
| Idle CPU, 1 terminal / + ticker | 0.56 / 0.76 % | 0.56 / 0.76 % |
| Idle CPU, 4 terminals / + tickers | 0.66 / 1.02 % | 0.69 / 1.02 % |
| Idle frames/s, 1 and 4 terminals | 0 | 0 |
| App idle / + ticker (terminal frames/s) | 0.73 % (0.03) / 0.92 % (0.96) | 0.72 % (0.03) / 0.92 % (0.96) |

Within noise everywhere: a steady cursor adds no frames, and the new
window-level mouse listeners cost nothing when no one moves the mouse.

**Not done / follow-ups**

- Selection by word or line (double/triple click), and dragging the scrollbar:
  the TUI has neither.
- A zoom chord off macOS, and zoom in App mode (the chord is handled by the
  focused terminal).
- `encode_paste` does not strip an embedded `ESC[201~` from bracketed text (the
  old spike encoder did). Changing it changes the TUI, the app and the phone
  relay together, so it is filed (`remote-control-dzfe`) rather than done here.
- The help screen does not list Cmd-C / Cmd zoom (help is shared with the TUI
  and the web).
