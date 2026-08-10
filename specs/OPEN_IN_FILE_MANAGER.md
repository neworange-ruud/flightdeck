# Open Worktree in File Manager — Design

Date: 2026-08-10

## Problem

While working in an agent session there is no way to open that session's
directory in the OS file manager (Finder, Explorer, or a Linux desktop file
manager). Users drop to a shell and type `open .` / `explorer.exe .` /
`xdg-open .` by hand.

## Behaviour

A new action, **Open Worktree in File Manager**, exposed two ways:

- Command palette: group `Worktree`, label `Open Worktree in File Manager`.
- `Alt-O`, registered as a global binding so it fires in both App mode and
  Terminal focus mode.

The action opens the **selected agent tab's worktree root**. When no agent tab
is selected (for example a freshly opened project with no sessions), it opens
the active project's git root instead.

Success is silent — the file-manager window appearing is the feedback. Failure
raises a refusal toast naming the command and the reason, e.g.
`Could not open file manager: 'xdg-open' not found`.

Container mode needs no special handling: worktrees live on the host and are
bind-mounted into the container, so the host path is always the correct target.

### Key choice: `Alt-O`

`Alt-O` is free on the platforms FlightDeck targets: it is not a GNOME Terminal
or Konsole menu mnemonic (those use `Alt+F/E/V/S/T/H`), it is unbound in Windows
Terminal, and it is not a standard readline or agent binding (`Alt+B/F/D/
Backspace` are the common ones), so making it global costs the agent nothing.

On macOS, `Option+O` emits `ø` unless the terminal maps Option to Meta.
FlightDeck already ships `Alt+1`–`Alt+9` and `Alt+Esc`, so this assumption is
not new. The palette entry is the guaranteed fallback wherever the key does not
arrive.

## Architecture

Follows the existing split: the app core returns data, the TUI wiring layer
performs the I/O. This mirrors `Effect::PrUrl`, and the launcher module mirrors
`tui/clipboard.rs`.

### App core

- `Command::OpenWorktreeInFileManager` in `src/app/commands.rs`.
- `Effect::OpenInFileManager { path: PathBuf, command: String }`, where
  `command` is the configured launcher override (empty string = per-OS default).
- `AppState::dispatch` resolves the path (selected tab's worktree, else repo
  root) and copies `config.ui.file_manager` into the effect. It performs no I/O,
  so it is unit-testable headlessly.

### Launcher

New module `src/tui/file_manager.rs`:

- `launcher(configured: &str) -> (String, Vec<String>)` — pure. Empty or
  whitespace-only input yields the per-OS default (`open` on macOS,
  `explorer.exe` on Windows, `xdg-open` elsewhere). A non-empty value is
  whitespace-split into program plus arguments, so
  `flatpak run org.gnome.Nautilus` works. No shell is involved and no quoting is
  interpreted.
- `open(path: &Path, configured: &str) -> Result<(), String>` — spawns the
  launcher detached with null stdin/stdout/stderr, so a chatty `xdg-open` cannot
  corrupt the alternate screen. Returns `Err` with a human-readable message when
  the spawn itself fails.

**We spawn without waiting for exit.** A configured GUI file manager (`nautilus`
with no already-running instance, for one) stays in the foreground for the life
of its window; waiting on it would freeze the TUI. The consequence is that
non-zero exit codes are not detected — only spawn failures (command not found,
permission denied), which are the failures that occur in practice.

**No headless/SSH guard.** Over SSH `xdg-open` typically exists and quietly does
nothing. Probing `DISPLAY`/`WAYLAND_DISPLAY` was considered and rejected: it
would refuse legitimate setups (containers or sessions that can still reach a
host opener) for the sake of a better error message in one case.

### Wiring

`src/lib.rs` handles `Effect::OpenInFileManager` at both existing `Effect` match
sites: call `file_manager::open`, and on `Err` surface the message as a refusal
toast. On `Ok`, nothing is shown.

### Input

`src/tui/input.rs`: `map_global` gains
`KeyCode::Char('o') if alt => KeyAction::Dispatch(Command::OpenWorktreeInFileManager)`,
matching plain `Alt` only.

## Config

New `[ui]` field:

```toml
[ui]
file_manager = ""   # empty = per-OS default (open / explorer.exe / xdg-open)
```

Typed as `String` with `#[serde(default)]`, defaulting to `""`, rather than
`Option<String>`. A plain string serializes into the generated `config.toml`
like every other default, so a fresh config documents the setting.

It does **not** get a configuration-manager row: that UI supports only `Bool`
and `Choice` field kinds, and adding a free-text kind is a larger change than
this escape-hatch setting justifies. It is edited in `config.toml` directly,
reachable through the existing `$EDITOR` path.

## Testing

- `src/app/state.rs` — dispatch returns `Effect::OpenInFileManager` carrying the
  selected tab's worktree path; falls back to the repo root when no tab is
  selected; carries the configured launcher string through.
- `src/tui/input.rs` — `Alt-O` maps to the dispatch in both App and Terminal
  mode.
- `src/tui/file_manager.rs` — per-OS default program (cfg-gated per target);
  configured override split into program plus args; whitespace-only config falls
  back to the default.
- `src/tui/palette.rs` — the entry exists with the expected group and action.

## Documentation

- README: keybinding table entry and a `ui.file_manager` line in the config
  reference.
- Help overlay (`draw_help_overlay` in `src/tui/render.rs`): a shortcut row.
- `CHANGELOG.md`: entry added when the pull request is opened, per `AGENTS.md`.

## Out of scope

- Following the live working directory of the focused terminal (would require
  reading child-process cwd per OS).
- Revealing a specific file rather than the directory.
- Editing `ui.file_manager` from the configuration manager overlay.
