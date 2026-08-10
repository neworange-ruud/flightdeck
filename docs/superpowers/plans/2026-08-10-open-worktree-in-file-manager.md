# Open Worktree in File Manager — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an action that opens the selected agent session tab's worktree directory in the OS file manager, reachable from the command palette and from `Alt-O` in both input modes.

**Architecture:** FlightDeck keeps its app core headless — `AppState::dispatch` returns an `Effect` and the TUI wiring layer in `src/lib.rs` performs the I/O. This feature follows that split exactly: a new `Command::OpenWorktreeInFileManager` resolves a path plus the configured launcher command and returns `Effect::OpenInFileManager`; a new `src/tui/file_manager.rs` (modelled on the existing `src/tui/clipboard.rs`) spawns the launcher.

**Tech Stack:** Rust 2021, ratatui + crossterm TUI, `std::process::Command` for the launcher, `serde`/`toml` for config. Tests are in-file `#[cfg(test)] mod tests` blocks using the existing `FakeGit`/`FakeFs`/`FakePty`/`FakeClock` doubles.

Design document: `docs/superpowers/specs/2026-08-10-open-in-file-manager-design.md`.

## Global Constraints

- CI runs `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, and `cargo test --locked`. All three must pass; clippy warnings are errors.
- The app core (`src/app/`, `src/git/`, `src/contracts/`) never executes fs/pty/process work directly (SPECS §27). All process spawning for this feature lives in `src/tui/file_manager.rs` and is called from `src/lib.rs`.
- Per-OS launcher defaults: `open` (macOS), `explorer.exe` (Windows), `xdg-open` (everything else).
- Config key: `ui.file_manager`, type `String`, default `""` (empty means "use the per-OS default").
- Keybinding: `Alt-O`, plain `Alt` only, registered globally (both App and Terminal mode).
- Palette entry: group `Worktree`, label `Open Worktree in File Manager`.
- No headless/SSH detection. Do not probe `DISPLAY` or `WAYLAND_DISPLAY`.
- Never block the TUI thread waiting for the launcher to exit.
- `CHANGELOG.md` must be updated when the pull request is opened (`AGENTS.md`).

---

### Task 1: Add the `ui.file_manager` config field

Adds the config surface first, because Task 3 reads it. `UiConfig` is constructed as an exhaustive struct literal in six places; all six must gain the new field or the crate will not compile.

**Files:**
- Modify: `src/contracts/domain.rs:265-282` (struct + `Default` impl)
- Modify: `src/config/schema.rs:57-61` (`default_config`)
- Modify: `src/agents/registry.rs:51-55` (test helper literal)
- Modify: `src/lib.rs:3703-3707` and `src/lib.rs:3759-3763` (test helper literals)
- Modify: `src/app/state.rs:1971-1975` (test helper literal)
- Test: `src/config/schema.rs` (tests module at the bottom), `src/config/load.rs` (tests module at the bottom)

**Interfaces:**
- Consumes: nothing.
- Produces: `UiConfig::file_manager: String` — read in Task 3 as `self.config.ui.file_manager`.

- [ ] **Step 1: Write the failing tests**

In `src/config/schema.rs`, inside the existing `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn default_config_leaves_file_manager_empty() {
        // Empty means "use the per-OS default launcher" (open / explorer.exe /
        // xdg-open); the key is still written so the global config documents it.
        let cfg = default_config("my-project", "main");
        assert_eq!(cfg.ui.file_manager, "");
    }
```

In `src/config/load.rs`, inside the existing `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn existing_ui_config_defaults_file_manager_to_empty() {
        let cfg = parse_config(
            r#"
[ui]
agent_tab_position = "left"
default_agent = "opencode"
"#,
        )
        .unwrap();

        assert_eq!(cfg.ui.file_manager, "");
    }

    #[test]
    fn ui_config_reads_file_manager_override() {
        let cfg = parse_config(
            r#"
[ui]
agent_tab_position = "left"
default_agent = "opencode"
file_manager = "nautilus"
"#,
        )
        .unwrap();

        assert_eq!(cfg.ui.file_manager, "nautilus");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --locked file_manager`
Expected: FAIL — compile error, `no field 'file_manager' on type 'UiConfig'`.

- [ ] **Step 3: Add the field to `UiConfig`**

In `src/contracts/domain.rs`, in the `UiConfig` struct after `use_f2_to_leave_terminal_focus`:

```rust
    /// Command used to open a worktree directory in the OS file manager.
    /// Empty (the default) means the per-OS default: `open` on macOS,
    /// `explorer.exe` on Windows, `xdg-open` elsewhere. A non-empty value is
    /// split on whitespace into a program plus arguments (no shell), so
    /// `flatpak run org.gnome.Nautilus` works.
    #[serde(default)]
    pub file_manager: String,
```

And in its `Default` impl, after `use_f2_to_leave_terminal_focus: false,`:

```rust
            file_manager: String::new(),
```

- [ ] **Step 4: Add the field to the five remaining struct literals**

Add `file_manager: String::new(),` to the `UiConfig { .. }` literal in each of:
- `src/config/schema.rs` (in `default_config`)
- `src/agents/registry.rs`
- `src/lib.rs` (both literals)
- `src/app/state.rs`

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --locked file_manager`
Expected: PASS — the three new tests pass.

- [ ] **Step 6: Run the full check**

Run: `cargo fmt --all && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add src/contracts/domain.rs src/config/schema.rs src/config/load.rs src/agents/registry.rs src/lib.rs src/app/state.rs
git commit -m "Add ui.file_manager config field"
```

---

### Task 2: Add the file-manager launcher module

A self-contained module with a pure resolution function (fully testable) and a thin spawn wrapper (not unit-tested — it would launch a real GUI).

**Files:**
- Create: `src/tui/file_manager.rs`
- Modify: `src/tui/mod.rs` (add `pub mod file_manager;`)
- Test: `src/tui/file_manager.rs` (in-file `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub fn launcher(configured: &str) -> (String, Vec<String>)`
  - `pub fn open(path: &std::path::Path, configured: &str) -> Result<(), String>`

  Both are called from `src/lib.rs` in Task 5 as `crate::tui::file_manager::open(&path, &command)`.

- [ ] **Step 1: Write the failing tests**

Create `src/tui/file_manager.rs` containing only the tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_the_platform_default() {
        let (program, args) = launcher("");
        assert!(args.is_empty());
        if cfg!(target_os = "macos") {
            assert_eq!(program, "open");
        } else if cfg!(target_os = "windows") {
            assert_eq!(program, "explorer.exe");
        } else {
            assert_eq!(program, "xdg-open");
        }
    }

    #[test]
    fn whitespace_only_config_uses_the_platform_default() {
        let (program, args) = launcher("   \t ");
        let (default_program, _) = launcher("");
        assert_eq!(program, default_program);
        assert!(args.is_empty());
    }

    #[test]
    fn configured_program_overrides_the_default() {
        let (program, args) = launcher("nautilus");
        assert_eq!(program, "nautilus");
        assert!(args.is_empty());
    }

    #[test]
    fn configured_command_splits_into_program_and_args() {
        // No shell is involved: the value is split on whitespace so a launcher
        // that needs fixed arguments still works.
        let (program, args) = launcher("flatpak run org.gnome.Nautilus");
        assert_eq!(program, "flatpak");
        assert_eq!(args, vec!["run", "org.gnome.Nautilus"]);
    }

    #[test]
    fn missing_program_reports_an_error_naming_it() {
        let err = open(
            std::path::Path::new("/tmp"),
            "flightdeck-no-such-file-manager",
        )
        .expect_err("spawning a nonexistent program must fail");
        assert!(
            err.contains("flightdeck-no-such-file-manager"),
            "error should name the command, got: {err}"
        );
    }
}
```

Add to `src/tui/mod.rs`, keeping the list alphabetical (after `pub mod config_manager;`):

```rust
pub mod file_manager;
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --locked tui::file_manager`
Expected: FAIL — compile error, `cannot find function 'launcher' in this scope`.

- [ ] **Step 3: Write the implementation**

Put this **above** the `#[cfg(test)]` block in `src/tui/file_manager.rs`:

```rust
//! Open a directory in the OS file manager (Finder / Explorer / the desktop's
//! `xdg-open` handler).
//!
//! Mirrors `tui::clipboard`: a per-OS default with a config escape hatch, and a
//! spawn that never blocks the TUI. We deliberately do not wait for the
//! launcher to exit — a file manager with no already-running instance (GNOME
//! Files, for one) stays in the foreground for the life of its window, so
//! waiting would freeze FlightDeck. That means non-zero exit codes go
//! unreported; spawn failures (missing command, not executable) do not.

use std::path::Path;
use std::process::{Command, Stdio};

/// Resolve the launcher program and its fixed arguments.
///
/// An empty or whitespace-only `configured` value yields the per-OS default.
/// Otherwise the value is split on whitespace into a program plus arguments —
/// no shell, no quote handling.
pub fn launcher(configured: &str) -> (String, Vec<String>) {
    let mut parts = configured.split_whitespace().map(str::to_string);
    match parts.next() {
        Some(program) => (program, parts.collect()),
        None => (default_program().to_string(), Vec::new()),
    }
}

/// The platform's default opener.
fn default_program() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer.exe"
    } else {
        "xdg-open"
    }
}

/// Open `path` in the file manager. Returns `Err` with a user-facing message
/// when the launcher cannot be started.
pub fn open(path: &Path, configured: &str) -> Result<(), String> {
    let (program, args) = launcher(configured);
    let child = Command::new(&program)
        .args(&args)
        .arg(path)
        // Null stdio: a chatty launcher must never write into the alternate
        // screen and corrupt the TUI.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not open file manager: '{program}' failed to start ({e})"))?;

    // Reap the child off-thread. `xdg-open` exits immediately, and FlightDeck is
    // long-running, so an unwaited child would linger as a zombie for the rest
    // of the session. The thread lives only as long as the launcher process.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });

    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --locked tui::file_manager`
Expected: PASS — all five tests.

- [ ] **Step 5: Run the full check**

Run: `cargo fmt --all && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add src/tui/file_manager.rs src/tui/mod.rs
git commit -m "Add file-manager launcher module"
```

---

### Task 3: Add the command, effect, and dispatch

**Files:**
- Modify: `src/app/commands.rs` (the `Command` enum, before `Quit`; the `Effect` enum, after `PrUrl`)
- Modify: `src/app/state.rs:208-227` (`requires_ready_tab`), the `dispatch` match (around `src/app/state.rs:769`), and a new `cmd_open_in_file_manager` method next to `cmd_show_git_status` (around `src/app/state.rs:1881`)
- Test: `src/app/state.rs` (tests module at the bottom)

**Interfaces:**
- Consumes: `UiConfig::file_manager` (Task 1).
- Produces:
  - `Command::OpenWorktreeInFileManager` (unit variant) — used by Task 4's palette entry and keybinding.
  - `Effect::OpenInFileManager { path: std::path::PathBuf, command: String }` — handled by Task 5.

- [ ] **Step 1: Write the failing tests**

In `src/app/state.rs`, inside the existing `#[cfg(test)] mod tests`, add:

```rust
    #[test]
    fn open_in_file_manager_targets_the_selected_worktree() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        pty.queue_session();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        app.dispatch(
            Command::NewAgentTab {
                name: "Task".to_string(),
                agent_key: None,
            },
            &svc,
        )
        .unwrap();

        let effect = app
            .dispatch(Command::OpenWorktreeInFileManager, &svc)
            .unwrap();
        match effect {
            Effect::OpenInFileManager { path, command } => {
                let expected = to_absolute(
                    &app.repo_root,
                    Path::new(&app.tabs[0].meta.worktree_path_relative),
                );
                assert_eq!(path, expected);
                // Nothing configured → the launcher module picks the per-OS default.
                assert_eq!(command, "");
            }
            other => panic!("expected OpenInFileManager, got {other:?}"),
        }
    }

    #[test]
    fn open_in_file_manager_falls_back_to_the_repo_root_without_tabs() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let config = config_with_agent(agent);
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        assert!(app.tabs.is_empty(), "no tabs in a fresh project");

        let effect = app
            .dispatch(Command::OpenWorktreeInFileManager, &svc)
            .unwrap();
        match effect {
            Effect::OpenInFileManager { path, .. } => assert_eq!(path, Path::new(REPO)),
            other => panic!("expected OpenInFileManager, got {other:?}"),
        }
    }

    #[test]
    fn open_in_file_manager_passes_the_configured_launcher_through() {
        let dir = TempDir::new().unwrap();
        let (agent, _cmd) = make_real_agent(&dir, "opencode");
        let mut config = config_with_agent(agent);
        config.ui.file_manager = "nautilus".to_string();
        let git = FakeGit::new().with_root(REPO).with_branches(["main"]);
        let fs = FakeFs::new();
        let pty = FakePty::new();
        let clock = FakeClock::default();
        let svc = services(&git, &fs, &pty, &clock);

        let mut app = fresh_state(config);
        let effect = app
            .dispatch(Command::OpenWorktreeInFileManager, &svc)
            .unwrap();
        match effect {
            Effect::OpenInFileManager { command, .. } => assert_eq!(command, "nautilus"),
            other => panic!("expected OpenInFileManager, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --locked open_in_file_manager`
Expected: FAIL — compile error, `no variant named 'OpenWorktreeInFileManager' found for enum 'Command'`.

- [ ] **Step 3: Add the command and effect variants**

In `src/app/commands.rs`, in the `Command` enum immediately before the `Quit` variant:

```rust
    /// Open the selected tab's worktree directory in the OS file manager.
    /// Falls back to the project's repo root when no tab is selected.
    OpenWorktreeInFileManager,
```

In the same file, in the `Effect` enum immediately after the `PrUrl` variant:

```rust
    /// A directory the UI should open in the OS file manager. `command` is the
    /// configured `ui.file_manager` override (empty = per-OS default); the TUI
    /// layer resolves and spawns it (SPECS §27 — the core never spawns).
    OpenInFileManager {
        /// Absolute directory to reveal.
        path: std::path::PathBuf,
        /// Configured launcher command, verbatim from `ui.file_manager`.
        command: String,
    },
```

- [ ] **Step 4: Wire up dispatch**

In `src/app/state.rs`, add to the `matches!` list in `requires_ready_tab`, after `| Command::ShowGitStatus`:

```rust
            | Command::OpenWorktreeInFileManager
```

In the `dispatch` match, after the `Command::ShowGitStatus => …` arm:

```rust
            Command::OpenWorktreeInFileManager => Ok(self.cmd_open_in_file_manager()),
```

And add this method directly after `cmd_show_git_status`:

```rust
    /// Resolve the directory to reveal in the OS file manager: the selected
    /// tab's worktree, or the project's repo root when no tab is selected (a
    /// freshly opened project). Performs no I/O — the TUI layer spawns the
    /// launcher (SPECS §27).
    fn cmd_open_in_file_manager(&self) -> Effect {
        let path = match self.selected() {
            Some(tab) => to_absolute(
                &self.repo_root,
                Path::new(&tab.meta.worktree_path_relative),
            ),
            None => self.repo_root.clone(),
        };
        Effect::OpenInFileManager {
            path,
            command: self.config.ui.file_manager.clone(),
        }
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --locked open_in_file_manager`
Expected: PASS — all three tests. (`cargo build` will still fail at this point only if you also ran a full build; the exhaustive `Effect` matches in `src/lib.rs` are completed in Task 5. If `cargo test --locked` reports non-exhaustive-match errors in `src/lib.rs`, add the two arms from Task 5 Step 3 now and note it in that task.)

- [ ] **Step 6: Commit**

```bash
git add src/app/commands.rs src/app/state.rs
git commit -m "Add OpenWorktreeInFileManager command and effect"
```

---

### Task 4: Add the palette entry and the `Alt-O` binding

**Files:**
- Modify: `src/tui/palette.rs` (the `ALL_ENTRIES` array, in the `Worktree` group)
- Modify: `src/tui/input.rs` (`map_global`, before the `_ => None` arm)
- Test: `src/tui/palette.rs` and `src/tui/input.rs` (tests modules at the bottom)

**Interfaces:**
- Consumes: `Command::OpenWorktreeInFileManager` (Task 3).
- Produces: nothing new — the existing generic `PaletteAction::Dispatch` and `KeyAction::Dispatch` paths carry it.

- [ ] **Step 1: Write the failing tests**

In `src/tui/palette.rs`, inside the existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn open_in_file_manager_entry_exists_in_the_worktree_group() {
        let entry = ALL_ENTRIES
            .iter()
            .find(|e| e.label == "Open Worktree in File Manager")
            .expect("palette must offer the file-manager action");
        assert_eq!(entry.group, "Worktree");
        assert_eq!(
            entry.action,
            PaletteAction::Dispatch(Command::OpenWorktreeInFileManager)
        );
    }
```

In `src/tui/input.rs`, inside the existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn alt_o_opens_the_file_manager_in_both_modes() {
        // Global binding: the common case is hitting it while the agent
        // terminal has focus, so it must not be App-mode only.
        assert_eq!(
            map_key(InputMode::App, alt(KeyCode::Char('o'))),
            KeyAction::Dispatch(Command::OpenWorktreeInFileManager)
        );
        assert_eq!(
            map_key(InputMode::Terminal, alt(KeyCode::Char('o'))),
            KeyAction::Dispatch(Command::OpenWorktreeInFileManager)
        );
    }

    #[test]
    fn plain_o_still_passes_through_to_the_terminal() {
        assert_eq!(
            map_key(InputMode::Terminal, key(KeyCode::Char('o'))),
            KeyAction::Passthrough(vec![b'o'])
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --locked entry_exists_in_the_worktree_group` then `cargo test --locked alt_o_opens_the_file_manager`
Expected: FAIL — `panicked at 'palette must offer the file-manager action'`, and an assertion failure showing `KeyAction::None` (App mode) / a `Passthrough` (Terminal mode) for `Alt-O`.

- [ ] **Step 3: Add the palette entry**

In `src/tui/palette.rs`, in `ALL_ENTRIES`, immediately after the `Abandon Worktree` entry:

```rust
    PaletteEntry {
        group: "Worktree",
        label: "Open Worktree in File Manager",
        action: PaletteAction::Dispatch(Command::OpenWorktreeInFileManager),
    },
```

- [ ] **Step 4: Add the keybinding**

In `src/tui/input.rs`, in `map_global`, immediately before the final `_ => None` arm:

```rust
        // Alt-o: open the selected worktree in the OS file manager. Global so it
        // works with a terminal focused (the common case). Alt-O is not a
        // standard readline/agent binding, so the PTY loses nothing.
        KeyCode::Char('o') if alt && !ctrl && !shift => {
            Some(KeyAction::Dispatch(Command::OpenWorktreeInFileManager))
        }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --locked entry_exists_in_the_worktree_group`, then `cargo test --locked alt_o_opens_the_file_manager`, then `cargo test --locked plain_o_still_passes_through`
Expected: PASS — all three tests.

- [ ] **Step 6: Run the full check**

Run: `cargo fmt --all && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked`
Expected: all pass, except any `non-exhaustive patterns` errors in `src/lib.rs` — those are closed in Task 5.

- [ ] **Step 7: Commit**

```bash
git add src/tui/palette.rs src/tui/input.rs
git commit -m "Add palette entry and Alt-O binding for opening the worktree"
```

---

### Task 5: Wire the effect into the TUI and the help overlay

`src/lib.rs` has two exhaustive `Effect` matches (`apply_effect` and `apply_effect_no_state`); both need the new arm or the crate will not compile.

**Files:**
- Modify: `src/lib.rs:2182-2190` (`apply_effect`) and `src/lib.rs:3040-3048` (`apply_effect_no_state`)
- Modify: `src/tui/render.rs:1774-1782` (`draw_help_overlay`, the `Global` group)

**Interfaces:**
- Consumes: `Effect::OpenInFileManager { path, command }` (Task 3) and `crate::tui::file_manager::open` (Task 2).
- Produces: nothing.

- [ ] **Step 1: Add the effect arm to both matches**

In `src/lib.rs`, in **both** `apply_effect` and `apply_effect_no_state`, immediately after the `Effect::PrUrl(url) => …` arm, add the identical arm:

```rust
        Effect::OpenInFileManager { path, command } => {
            // Success is silent — the file-manager window is the feedback.
            if let Err(e) = crate::tui::file_manager::open(&path, &command) {
                ui.message(format!("Refused: {e}"));
            }
        }
```

- [ ] **Step 2: Add the help-overlay row**

In `src/tui/render.rs`, in `draw_help_overlay`'s `Global` group, immediately after the `Ctrl-k` line:

```rust
        shortcut_line("  Alt-o", "Open worktree in file manager"),
```

- [ ] **Step 3: Verify the crate builds and every test passes**

Run: `cargo fmt --all && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked`
Expected: all pass, with no `non-exhaustive patterns` errors remaining.

- [ ] **Step 4: Smoke-test it by hand**

Run `cargo run` from inside a git repository with at least one agent session tab, then:
1. With the terminal focused, press `Alt-O` → your file manager opens the worktree directory (`.flightdeck/worktrees/<slug>`).
2. Press `?` → the help overlay lists `Alt-o  Open worktree in file manager`.
3. Press `Ctrl-g`, type `file` → `Open Worktree in File Manager` appears under `Worktree`; `Enter` opens it.
4. Set `file_manager = "flightdeck-not-a-real-command"` under `[ui]` in `.flightdeck/config.toml`, restart, and press `Alt-O` → a toast reads `Refused: could not open file manager: 'flightdeck-not-a-real-command' failed to start (…)`. Remove the setting afterwards.

Note in your report which of these you were able to run; a headless environment cannot verify steps 1 and 4's success path.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/tui/render.rs
git commit -m "Open the worktree in the file manager from the TUI"
```

---

### Task 6: Update the documentation and the specification

`specs/SPECS.md` is the living specification of shipped behaviour and must describe this feature; the design document in `docs/superpowers/specs/` records the reasoning and stays as written.

**Files:**
- Modify: `specs/SPECS.md` (§8 example global base, §22 palette actions, §23 required shortcuts, §26 required unit-test areas)
- Modify: `README.md` (§Configuration, §Keyboard model)
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Update `specs/SPECS.md` §8**

In the `Example global base` TOML block, in the `[ui]` section after `use_f2_to_leave_terminal_focus = false`:

```toml
file_manager = ""
```

Directly below that code block, add:

```markdown
`ui.file_manager` overrides the command used to open a worktree in the OS file
manager. Empty (the default) means the per-OS default: `open` on macOS,
`explorer.exe` on Windows, `xdg-open` elsewhere. A non-empty value is split on
whitespace into a program plus arguments; no shell is involved.
```

- [ ] **Step 2: Update `specs/SPECS.md` §22**

In the `Required command palette actions` block, add after `Abandon Worktree`:

```text
Open Worktree in File Manager
```

- [ ] **Step 3: Update `specs/SPECS.md` §23**

In the `Required shortcuts` block, in the `Global` group after the `Ctrl-k` line:

```text
  Alt-o           Open selected worktree in the OS file manager
```

- [ ] **Step 4: Update `specs/SPECS.md` §26**

In `Required Unit Test Areas`, add a bullet:

```markdown
- File-manager launcher resolution: empty config falls back to the per-OS
  default; a configured value is split into program plus arguments; a missing
  program produces an error naming the command.
```

- [ ] **Step 5: Update `README.md`**

In the **Configuration** section, after the bullet describing the F2 setting, add:

```markdown
- `ui.file_manager` (raw config only, via `e`) overrides the command used by
  **Open Worktree in File Manager**. Empty means the per-OS default (`open`,
  `explorer.exe`, `xdg-open`); set e.g. `file_manager = "nautilus"` or
  `file_manager = "explorer.exe"` under WSL.
```

In the **Keyboard model** section, in the `Common shortcuts:` paragraph, add after `` `Ctrl-r` restart agent ``:

```markdown
 · `Alt-o` open the worktree in your file manager
```

- [ ] **Step 6: Update `CHANGELOG.md`**

Follow the existing format at the top of the file, adding under the unreleased/next section:

```markdown
- Open the selected agent session tab's worktree in the OS file manager, from
  the command palette or `Alt-O` (works with a terminal focused). Override the
  launcher with `ui.file_manager` in `config.toml`.
```

- [ ] **Step 7: Verify nothing is stale**

Run: `cargo test --locked && grep -n "file_manager" specs/SPECS.md README.md CHANGELOG.md`
Expected: tests pass, and the grep shows the four `specs/SPECS.md` edits plus the README and CHANGELOG entries.

- [ ] **Step 8: Commit**

```bash
git add specs/SPECS.md README.md CHANGELOG.md
git commit -m "Document opening a worktree in the file manager"
```

---

## Self-Review

**Spec coverage.** Every design section maps to a task: behaviour and trigger → Tasks 3 and 4; app-core/launcher architecture → Tasks 2 and 3; no-wait spawn and child reaping → Task 2; config field → Task 1; wiring and feedback → Task 5; the four testing areas → Tasks 1–4; documentation including the `specs/SPECS.md` edits → Task 6. The design's "out of scope" list (live terminal cwd, revealing a file, a config-manager row) has no task, by intent.

**Type consistency.** `Command::OpenWorktreeInFileManager` and `Effect::OpenInFileManager { path, command }` are named identically in Tasks 3, 4, and 5. `launcher(&str) -> (String, Vec<String>)` and `open(&Path, &str) -> Result<(), String>` are used in Task 5 exactly as defined in Task 2. `UiConfig::file_manager` is read in Task 3 exactly as defined in Task 1.

**Known ordering wrinkle.** Task 3 adds an `Effect` variant that Task 5 finishes matching on, so a full `cargo build` between them fails with `non-exhaustive patterns` in `src/lib.rs`. Task 3 Step 5 says so explicitly and gives the fix. Tasks 1, 2, 4, and 6 each build clean on their own.
