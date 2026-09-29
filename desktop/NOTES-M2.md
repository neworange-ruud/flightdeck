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
- Split view (Ctrl-b) is not drawn: the main area shows the focused terminal.
- Host terminals use the TUI's emulator (`TUI_EMULATOR`, vt100), not the
  desktop's alacritty backend, and their OSC 10/11 default colours are not
  set from the theme.
- One size for every project's PTYs (the TUI's per-mode chrome differences do
  not apply to the GUI yet).
- A launch from Finder has no git cwd: the app exits with the TUI's error
  instead of offering to open a folder.
- The context menu shows keycaps as trailing text, not aligned columns; the
  close `×` is only on the active project tab.
- Every host notify re-renders the whole window (fine at this size; a
  terminal-only redraw path is a later optimisation).
- `[ui] use_f2_to_leave_terminal_focus` is still not read (`remote-control-9diy`).
