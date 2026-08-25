# Isolated Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `flightdeck --isolated` / `-I`: a throwaway run with one fresh agent session in the current working directory, no continuation, no worktrees, and no FlightDeck-initiated writes to the project.

**Architecture:** A single runtime-only `isolated` flag on `AppState`, set from a CLI scan in `run()`. Three levers carry almost all of the behavior: `startup()` skips its write-and-recover steps, `AppState::persist` becomes a no-op, and the agent status plumbing is redirected to a temp directory. The one tab is created with the *existing* `begin_new_agent_tab_ex(.., run_on_base = true, ..)` path, which already performs zero git mutation.

**Tech Stack:** Rust 2021, ratatui + crossterm TUI, no CLI-parsing crate (hand-rolled `std::env::args()` scans), in-house fakes in `src/testing/mod.rs` for all I/O traits.

**Spec:** `specs/ISOLATED_MODE.md` (reserves SPECS §32). Read it before Task 1 — every task below argues from it.

## Global Constraints

- **Ship gate, mandatory, cited verbatim in every task's final step:** `cargo test -p flightdeck --lib`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo fmt --check`. `-D warnings` and `--locked` are not optional; fix lints, never `#[allow]` them away.
- **No new dependencies.** Argument parsing stays hand-rolled; do not introduce `clap`.
- **Normal (non-isolated) behavior must be byte-identical.** Every task that touches a shared function passes the existing default through unchanged, and several tasks below carry an explicit regression test for that.
- **Refusal paths need tests, not only success paths** (SPECS §26).
- **`isolated` is never a configuration setting** and is never serialized. It is a runtime-only field, like `AppState::split_view`.
- **Cross-platform parity** (macOS, Linux, Windows) is a hard requirement. Task 6 is the risky one; see its warning.
- **No writes** means *FlightDeck writes nothing on its own*. Explicit user actions that exist to write (config manager save, phone pairing) still write. Containerized runs keep the in-worktree status path (spec §8.2).
- Business logic lives behind the `contracts` traits; the TUI never touches git/fs/pty directly (SPECS §27).
- `CHANGELOG.md` is updated once, at PR time (Task 12) — not per commit.

---

### Task 1: Give `FakeFs` a write journal

The spec's strongest test is "an isolated startup writes nothing under the repo root" (spec §10). `FakeFs` currently records only final contents, so there is no way to assert that. This task adds the recorder and nothing else.

**Files:**
- Modify: `src/testing/mod.rs:26-36` (add the journal field), `src/testing/mod.rs:104-153` (record in the five mutating methods)
- Test: `src/testing/mod.rs` (inline `#[cfg(test)] mod tests`, or extend the existing one if present)

**Interfaces:**
- Consumes: nothing.
- Produces: `FakeFs::writes(&self) -> Vec<PathBuf>` — every path passed to `create_dir_all`, `write`, `symlink` (the *link* path), `append_line`, or `remove_dir_all`, in call order, including duplicates. Also `FakeFs::writes_under(&self, root: &Path) -> Vec<PathBuf>`, filtered to paths starting with `root`. Tasks 4, 7 and 8 assert on these.

- [ ] **Step 1: Write the failing test**

Add to the tests module in `src/testing/mod.rs`:

```rust
#[test]
fn fake_fs_records_every_mutating_call() {
    use crate::contracts::traits::FileSystem;
    let fs = FakeFs::new();
    fs.write(Path::new("/repo/a.txt"), "x").unwrap();
    fs.create_dir_all(Path::new("/repo/sub")).unwrap();
    fs.append_line(Path::new("/repo/.gitignore"), "ignored").unwrap();
    fs.symlink(Path::new("/repo/a.txt"), Path::new("/repo/link")).unwrap();

    let writes = fs.writes();
    assert_eq!(
        writes,
        vec![
            PathBuf::from("/repo/a.txt"),
            PathBuf::from("/repo/sub"),
            PathBuf::from("/repo/.gitignore"),
            PathBuf::from("/repo/link"),
        ],
        "every mutating call must be journalled in order"
    );
}

#[test]
fn fake_fs_writes_under_filters_by_root() {
    use crate::contracts::traits::FileSystem;
    let fs = FakeFs::new();
    fs.write(Path::new("/repo/inside.txt"), "x").unwrap();
    fs.write(Path::new("/tmp/outside.txt"), "x").unwrap();

    assert_eq!(
        fs.writes_under(Path::new("/repo")),
        vec![PathBuf::from("/repo/inside.txt")]
    );
    assert!(
        fs.writes_under(Path::new("/nowhere")).is_empty(),
        "a root with no writes under it yields nothing"
    );
}

#[test]
fn fake_fs_reads_are_not_journalled() {
    use crate::contracts::traits::FileSystem;
    let fs = FakeFs::new().with_file("/repo/a.txt", "x");
    let _ = fs.read_to_string(Path::new("/repo/a.txt"));
    let _ = fs.exists(Path::new("/repo/a.txt"));
    let _ = fs.is_dir(Path::new("/repo"));
    assert!(
        fs.writes().is_empty(),
        "seeding and reading must not count as writes"
    );
}
```

Note the third test: `with_file`/`with_dir` seed the fake and must **not** journal, or every test's "zero writes" assertion would be meaningless.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flightdeck --lib fake_fs_ -- --nocapture`
Expected: FAIL — `no method named 'writes' found for struct 'FakeFs'`.

- [ ] **Step 3: Implement the journal**

In `src/testing/mod.rs`, add the field to `FakeFsState` (around line 30):

```rust
#[derive(Debug, Default)]
struct FakeFsState {
    files: BTreeMap<PathBuf, String>,
    dirs: HashSet<PathBuf>,
    /// Symlinks as `link -> target`.
    symlinks: BTreeMap<PathBuf, PathBuf>,
    /// Every path handed to a mutating `FileSystem` method, in call order.
    /// Seeding helpers (`with_file`/`with_dir`) deliberately do not record, so
    /// a test can assert "this code path wrote nothing" against a pre-seeded
    /// filesystem.
    writes: Vec<PathBuf>,
}
```

Add the accessors in `impl FakeFs` (next to the existing snapshot helpers, around line 66):

```rust
/// Every path passed to a mutating `FileSystem` method, in call order,
/// duplicates included. Seeding via `with_file`/`with_dir` is not recorded.
pub fn writes(&self) -> Vec<PathBuf> {
    self.inner.lock().unwrap().writes.clone()
}

/// The journalled writes whose path lies under `root`.
pub fn writes_under(&self, root: &Path) -> Vec<PathBuf> {
    self.writes()
        .into_iter()
        .filter(|p| p.starts_with(root))
        .collect()
}
```

Then add one line to each of the five mutating impls. `create_dir_all` (line 104), `write` (121), `append_line` (141) and `remove_dir_all` record their `p`; `symlink` (128) records `link`, not `target`, and records only on the success path (after the already-exists check). Example for `write`:

```rust
fn write(&self, p: &Path, contents: &str) -> Result<()> {
    let mut st = self.inner.lock().unwrap();
    mark_parents(&mut st.dirs, p);
    st.files.insert(p.to_path_buf(), contents.to_string());
    st.writes.push(p.to_path_buf());
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flightdeck --lib fake_fs_`
Expected: PASS, 3 tests.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo test -p flightdeck --lib
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
git add src/testing/mod.rs
git commit -m "test: journal mutating calls in FakeFs

Lets a test assert that a code path wrote nothing, which the isolated-mode
startup tests need (specs/ISOLATED_MODE.md §10)."
```

---

### Task 2: Parse `--isolated` / `-I`

**Files:**
- Modify: `src/lib.rs:123-152` (the flag scans and subcommand match in `run()`), `src/lib.rs:758-787` (`print_help`)
- Test: `src/lib.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `fn parse_isolated(args: &[String]) -> Result<bool>` — a private free function in `src/lib.rs`. `args` is the full argv *including* argv[0]. Returns `Ok(true)` when `--isolated` or `-I` is present, `Ok(false)` when absent, and `Err(FlightDeckError::Config(_))` when the flag is combined with a subcommand. Tasks 4 and 7 consume the returned `bool`.

- [ ] **Step 1: Write the failing test**

Add to the tests module in `src/lib.rs`:

```rust
fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn parse_isolated_accepts_both_spellings() {
    assert!(parse_isolated(&argv(&["flightdeck", "--isolated"])).unwrap());
    assert!(parse_isolated(&argv(&["flightdeck", "-I"])).unwrap());
}

#[test]
fn parse_isolated_defaults_to_off() {
    assert!(!parse_isolated(&argv(&["flightdeck"])).unwrap());
}

#[test]
fn parse_isolated_is_not_confused_by_lowercase_i() {
    // `-i` is not the flag; only the documented `-I` is.
    assert!(!parse_isolated(&argv(&["flightdeck", "-i"])).unwrap());
}

#[test]
fn parse_isolated_refuses_a_subcommand() {
    let err = parse_isolated(&argv(&["flightdeck", "-I", "doctor"])).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("--isolated") && msg.contains("doctor"),
        "the error must name the flag and the offending subcommand: {msg}"
    );
}

#[test]
fn parse_isolated_refuses_a_subcommand_given_first() {
    assert!(parse_isolated(&argv(&["flightdeck", "image", "--isolated"])).is_err());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flightdeck --lib parse_isolated`
Expected: FAIL — `cannot find function 'parse_isolated' in this scope`.

- [ ] **Step 3: Implement the parser**

Add near `print_help` in `src/lib.rs`:

```rust
/// Subcommands that configure or inspect FlightDeck and exit without the TUI.
/// Kept next to [`parse_isolated`] so a new subcommand cannot silently become
/// combinable with `--isolated`.
const SUBCOMMANDS: &[&str] = &[
    "setup-status",
    "setup-notifications",
    "setup-update",
    "update",
    "image",
    "doctor",
];

/// Whether this invocation asked for an isolated run (SPECS §32).
///
/// `args` is the full argv, argv[0] included. Isolated mode launches the TUI, so
/// it cannot be combined with a subcommand — that combination is a hard error
/// rather than a silent ignore, because silently dropping the flag would let a
/// user believe a run was isolated when it was not.
fn parse_isolated(args: &[String]) -> Result<bool> {
    let isolated = args
        .iter()
        .any(|a| a == "--isolated" || a == "-I");
    if !isolated {
        return Ok(false);
    }
    if let Some(sub) = args.iter().find(|a| SUBCOMMANDS.contains(&a.as_str())) {
        return Err(FlightDeckError::Config(format!(
            "--isolated launches the TUI and cannot be combined with the '{sub}' subcommand"
        )));
    }
    Ok(true)
}
```

Wire it into `run()`, immediately after the `--help` block (`src/lib.rs:135`) and *before* the subcommand match, so `flightdeck -I doctor` errors instead of running `doctor`:

```rust
let argv: Vec<String> = std::env::args().collect();
let isolated = parse_isolated(&argv)?;
```

Leave `isolated` unused for now — Tasks 4 and 7 consume it. To keep clippy quiet in the meantime, bind it as `let _isolated = ...` and rename it in Task 4; do **not** add an `#[allow]`.

Add the flag to `print_help`'s OPTIONS block (`src/lib.rs:784-786`), keeping the existing two-space alignment:

```rust
println!("OPTIONS:");
println!("    -h, --help       Print this help");
println!("    -V, --version    Print version");
println!("    -I, --isolated   Throwaway run: one fresh session in the current");
println!("                     directory. No continuation, no worktrees, no");
println!("                     other projects, and nothing written to the project.");
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flightdeck --lib parse_isolated`
Expected: PASS, 5 tests.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo test -p flightdeck --lib
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
git add src/lib.rs
git commit -m "feat: parse --isolated / -I and document it in --help

Refuses combination with a subcommand rather than silently ignoring the
flag (specs/ISOLATED_MODE.md §3)."
```

---

### Task 3: Carry the flag on `AppState` and neuter `persist`

`AppState::persist` is the single private funnel for every `state.json` write (`finalize_new_tab`, rename, close, status changes). Guarding it there is what makes "no writes" hold without auditing a dozen call sites.

**Files:**
- Modify: `src/app/state.rs:506-541` (struct fields), `src/app/state.rs:547-570` (`AppState::new`), `src/app/state.rs:796-799` (`persist`)
- Test: `src/app/state.rs` (existing tests module)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `AppState::isolated: bool` — public field, runtime-only, defaults `false`.
  - `AppState::isolated_status_root: Option<PathBuf>` — public field, `None` by default; `Some(dir)` redirects the agent status plumbing there (Task 6 reads it).
  - `pub fn AppState::set_isolated(&mut self, status_root: Option<PathBuf>)` — sets `isolated = true` and stores `status_root`. Tasks 6, 7, 9, 10 and 11 read the two fields.

`AppState::new`'s signature is deliberately **unchanged** — dozens of tests construct it, and a setter keeps them compiling.

- [ ] **Step 1: Write the failing test**

Add to the tests module in `src/app/state.rs`. Reuse whatever local helper the neighbouring tests use to build an `AppState` and a `Services`; the existing tests in this file already do so (see `startup`-adjacent tests around line 2489 for the pattern).

```rust
#[test]
fn isolated_state_never_writes_state_json() {
    let git = FakeGit::new(/* as neighbouring tests construct it */);
    let fs = FakeFs::new();
    let pty = FakePty::new();
    let clock = FakeClock::new();
    let mut app = AppState::new(
        default_config("proj", "main"),
        default_state("main"),
        "/repo",
        "/repo/.flightdeck/state.json",
    );
    app.set_isolated(None);
    assert!(app.isolated);

    // Renaming persists in a normal run; in an isolated one it must not.
    app.tabs.push(/* a ready tab, as the rename tests build one */);
    app.selected_tab = Some(0);
    app.dispatch(
        Command::RenameAgentTab { new_name: "renamed".to_string() },
        &services(&git, &fs, &pty, &clock),
    )
    .unwrap();

    assert_eq!(app.tabs[0].meta.name, "renamed", "the rename still applies in memory");
    assert!(
        fs.writes().is_empty(),
        "an isolated run must not write state.json: {:?}",
        fs.writes()
    );
}

#[test]
fn non_isolated_state_still_writes_state_json() {
    // Regression guard: the guard must not disable persistence generally.
    let git = FakeGit::new(/* as above */);
    let fs = FakeFs::new();
    let pty = FakePty::new();
    let clock = FakeClock::new();
    let mut app = AppState::new(
        default_config("proj", "main"),
        default_state("main"),
        "/repo",
        "/repo/.flightdeck/state.json",
    );
    assert!(!app.isolated, "isolated is off by default");
    app.tabs.push(/* a ready tab */);
    app.selected_tab = Some(0);
    app.dispatch(
        Command::RenameAgentTab { new_name: "renamed".to_string() },
        &services(&git, &fs, &pty, &clock),
    )
    .unwrap();
    assert!(
        fs.writes()
            .iter()
            .any(|p| p.ends_with("state.json")),
        "a normal run still persists"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flightdeck --lib isolated_state_never_writes`
Expected: FAIL — `no method named 'set_isolated'`.

- [ ] **Step 3: Implement the fields and the guard**

Add to the `AppState` struct after `update_available` (`src/app/state.rs:540`):

```rust
    /// Isolated run (SPECS §32): a throwaway session that continues nothing and
    /// writes nothing of its own. Runtime-only — never serialized, never a
    /// configuration setting, because a persisted file that suppressed
    /// persistence would be a trap.
    pub isolated: bool,
    /// Where the agent status plumbing lives when it must stay out of the
    /// project directory (SPECS §32). `None` means the tab's own worktree,
    /// which is the normal behavior.
    pub isolated_status_root: Option<PathBuf>,
```

Initialize both in `AppState::new` (`src/app/state.rs:568`, next to `update_available: None`):

```rust
            isolated: false,
            isolated_status_root: None,
```

Add the setter next to `reload_config` (around line 575):

```rust
    /// Mark this run isolated (SPECS §32), optionally redirecting the agent
    /// status plumbing to `status_root` so it stays out of the project
    /// directory. Call once, during startup, before the event loop.
    pub fn set_isolated(&mut self, status_root: Option<PathBuf>) {
        self.isolated = true;
        self.isolated_status_root = status_root;
    }
```

Guard `persist` (`src/app/state.rs:796`):

```rust
    fn persist(&self, services: &Services) -> Result<()> {
        // An isolated run continues nothing, so it records nothing: this is the
        // single funnel every `state.json` write passes through (SPECS §32).
        if self.isolated {
            return Ok(());
        }
        let state = self.to_project_state(services.clock.now_millis());
        save_state(services.fs, &self.state_path, &state)
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flightdeck --lib isolated_state_never_writes non_isolated_state_still_writes`
Expected: PASS, 2 tests.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo test -p flightdeck --lib
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
git add src/app/state.rs
git commit -m "feat: AppState::isolated, and persist() as a no-op under it

One guard on the single state.json funnel (specs/ISOLATED_MODE.md §4, §7)."
```

---

### Task 4: An isolated `startup()` that reads but never writes

**Files:**
- Modify: `src/lib.rs:614-694` (`startup`), and its call site in `run()` (find it with `rg 'startup\(' src/lib.rs`), plus the two existing `startup` tests at `src/lib.rs:5581` and `src/lib.rs:5615` (signature change)
- Test: `src/lib.rs` (existing tests module)

**Interfaces:**
- Consumes: `parse_isolated` from Task 2; `AppState::set_isolated` from Task 3.
- Produces: `fn startup(services: &Services, repo_root: &Path, cwd: &Path, isolated: bool) -> Result<AppState>` — the fourth parameter is new. Existing callers pass `false`. Task 7 passes the parsed flag.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn isolated_startup_writes_nothing_under_the_repo() {
    let git = FakeGit::new(/* current branch "main", not dirty — copy the
                              construction from startup_builds_state_and_
                              records_dirty_base_warning at line 5581 */);
    let fs = FakeFs::new();
    let pty = FakePty::new();
    let clock = FakeClock::new();
    let services = services(&git, &fs, &pty, &clock);

    let state = startup(&services, Path::new("/repo"), Path::new("/repo"), true).unwrap();

    assert!(
        fs.writes_under(Path::new("/repo")).is_empty(),
        "isolated startup must not touch the project: {:?}",
        fs.writes_under(Path::new("/repo"))
    );
    assert!(state.tabs.is_empty(), "startup itself creates no tab (Task 7 does)");
    assert!(
        !state.config.ui.auto_continue,
        "auto_continue is forced off so even Restart Agent starts fresh"
    );
}

#[test]
fn isolated_startup_ignores_state_json_on_disk() {
    let git = FakeGit::new(/* as above */);
    // A previous run's state that a normal startup would recover.
    let fs = FakeFs::new().with_file(
        "/repo/.flightdeck/state.json",
        r#"{"version":1,"base_branch":"main","tabs":[]}"#,
    );
    let pty = FakePty::new();
    let clock = FakeClock::new();
    let services = services(&git, &fs, &pty, &clock);

    let state = startup(&services, Path::new("/repo"), Path::new("/repo"), true).unwrap();

    assert!(state.tabs.is_empty(), "nothing is recovered in an isolated run");
    assert!(fs.writes_under(Path::new("/repo")).is_empty());
}

#[test]
fn isolated_startup_still_reads_project_config() {
    let git = FakeGit::new(/* as above */);
    let fs = FakeFs::new().with_file(
        "/repo/.flightdeck/config.toml",
        "[ui]\ndefault_agent = \"claude\"\n",
    );
    let pty = FakePty::new();
    let clock = FakeClock::new();
    let services = services(&git, &fs, &pty, &clock);

    let state = startup(&services, Path::new("/repo"), Path::new("/repo"), true).unwrap();

    assert_eq!(
        state.config.ui.default_agent, "claude",
        "isolated mode reads existing config; it only refuses to write"
    );
    assert!(fs.writes_under(Path::new("/repo")).is_empty());
}

#[test]
fn normal_startup_still_initializes_the_project() {
    // Regression guard for the untouched default path.
    let git = FakeGit::new(/* as above */);
    let fs = FakeFs::new();
    let pty = FakePty::new();
    let clock = FakeClock::new();
    let services = services(&git, &fs, &pty, &clock);

    let _ = startup(&services, Path::new("/repo"), Path::new("/repo"), false).unwrap();

    assert!(
        fs.writes_under(Path::new("/repo"))
            .iter()
            .any(|p| p.ends_with("config.toml")),
        "a normal first run still writes the project config"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flightdeck --lib isolated_startup normal_startup_still_initializes`
Expected: FAIL — `this function takes 3 arguments but 4 arguments were supplied`.

- [ ] **Step 3: Implement**

Change the signature and doc comment at `src/lib.rs:614`, then gate the four write steps and the recovery step. The order of `detect_base_branch` and the dirty check is unchanged.

```rust
fn startup(
    services: &Services,
    repo_root: &Path,
    cwd: &Path,
    isolated: bool,
) -> Result<AppState> {
```

- `initialize(...)` (line ~633): wrap in `if !isolated { ... }`.
- `ensure_global_config(...)` (line ~641): wrap in `if !isolated { ... }` — it writes the per-user global base. Still *read* the global path when it already exists, so an isolated run honours the user's global settings:

```rust
    let global_path = global_config_path();
    if !isolated {
        if let Some(gp) = &global_path {
            let _ = ensure_global_config(services.fs, gp);
        }
    }
    let config = match &global_path {
        // In an isolated run the global base may legitimately not exist,
        // because we never created it; fall back to the project layer alone.
        Some(gp) if services.fs.exists(gp) => load_layered_config(services.fs, gp, &config_path),
        _ => load_config(services.fs, &config_path),
    }
    .unwrap_or_else(|_| default_config(&project_name, &base_branch));
```

- `ensure_flightdeck_gitignore(...)` and its `eprintln!` (lines ~649-660): wrap the whole block in `if !isolated { ... }`.
- state load + `recover` (lines ~664-673): in isolated mode use a fresh default and skip recovery entirely:

```rust
    let (mut project_state, report) = if isolated {
        // Nothing is continued in an isolated run (SPECS §32): no state is
        // read, and `recover` is never called, so no tab is reconstructed.
        (default_state(&base_branch), RecoveryReport::default())
    } else {
        let mut ps =
            load_state(services.fs, &state_path).unwrap_or_else(|_| default_state(&base_branch));
        let report = recover(
            services.fs,
            services.git,
            repo_root,
            &worktrees_root,
            &mut ps,
        )?;
        (ps, report)
    };
```

Then after `AppState::new` (line ~677), mark the state and force the flag off:

```rust
    let mut state = AppState::new(config, project_state, repo_root, &state_path);
    if isolated {
        // Redirecting the status plumbing is Task 6's job; the root is attached
        // by the caller in `run()`, which owns the temp directory's lifetime.
        state.set_isolated(None);
        // Forced off, not merely defaulted: without this, "no continuation"
        // would hold only until the first Restart Agent replayed a captured
        // resume command (SPECS §32 §4).
        state.config.ui.auto_continue = false;
    }
```

`mut` on `project_state` may now be unnecessary in the isolated branch; let the compiler guide you and drop `mut` from the binding if clippy complains. Update the two existing `startup(...)` test call sites (`src/lib.rs:5581`, `src/lib.rs:5615`) and the production call site in `run()` to pass `false` for now — Task 7 replaces the production one.

`RecoveryReport` is already imported in `lib.rs`; if not, add `use crate::persistence::recovery::RecoveryReport;`.

Finally, disable the update check. It lives in `event_loop`, not `startup`: the
call is `crate::update::start_check(check_enabled, env.clock.now_unix_secs(), update_tx)`
at `src/lib.rs:1274`, and `check_enabled` is bound a few lines above it from
`config.update.check`. It both makes a network call and writes a cache file, so
an isolated run must not arm it (spec §4):

```rust
    // An isolated run makes no network call and writes no cache (SPECS §32).
    let check_enabled = !workspace.active_project().state.isolated && check_enabled;
```

Use whichever state handle is actually in scope at that point — read the
surrounding lines rather than assuming `workspace` is available there.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flightdeck --lib startup`
Expected: PASS — the four new tests plus the two pre-existing `startup_*` tests.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo test -p flightdeck --lib
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
git add src/lib.rs
git commit -m "feat: isolated startup reads config but writes nothing

Skips first-run init, the global config base, the .gitignore update, and
state load + recovery; forces auto_continue off (specs/ISOLATED_MODE.md §4)."
```

---

### Task 5: Label a base-branch tab with the branch actually checked out

Spec §5. A base tab currently records `branch: base_branch`, which lies whenever HEAD is on something else — and `cmd_push` pushes `meta.branch`, so the lie has teeth. This is a standalone correctness fix that also happens to be what isolated mode needs; a reviewer can accept or reject it independently of the rest.

**Files:**
- Modify: `src/app/state.rs:1058-1135` (`begin_base_agent_tab`)
- Test: `src/app/state.rs` (existing tests module)

**Interfaces:**
- Consumes: `GitExecutor::current_branch(&self, cwd: &Path) -> Result<String>` (`src/contracts/traits.rs:33`).
- Produces: no signature change. `TabState::branch` for a base tab now holds the checked-out branch; `TabState::base_branch` still holds the configured base.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn base_tab_records_the_checked_out_branch_not_the_base() {
    // Base is "main", but the repo root currently has "spike" checked out.
    let git = FakeGit::new(/* base "main", current_branch -> "spike" */);
    let fs = FakeFs::new();
    let pty = FakePty::new();
    let clock = FakeClock::new();
    let mut app = /* AppState with base_branch "main", as the base-tab test at
                     line 2817 builds one */;

    let job = app
        .begin_new_agent_tab_ex("", None, true, &services(&git, &fs, &pty, &clock))
        .unwrap();

    assert!(app.tabs[0].meta.runs_on_base);
    assert_eq!(
        app.tabs[0].meta.branch, "spike",
        "the tab must name the branch it is actually on — push uses this field"
    );
    assert_eq!(
        app.tabs[0].meta.base_branch, "main",
        "the configured base is unchanged"
    );
    assert_eq!(app.tabs[0].meta.name, "spike", "the blank name falls back to the branch");
    assert!(!job.needs_create, "a base tab still materializes no worktree");
    assert!(git.added_worktrees().is_empty(), "and runs no git mutation");
}

#[test]
fn base_tab_falls_back_to_base_when_head_is_detached() {
    // A detached HEAD makes current_branch fail; the base name is the fallback.
    let git = FakeGit::new(/* base "main", current_branch -> Err */);
    let fs = FakeFs::new();
    let pty = FakePty::new();
    let clock = FakeClock::new();
    let mut app = /* as above */;

    app.begin_new_agent_tab_ex("", None, true, &services(&git, &fs, &pty, &clock))
        .unwrap();

    assert_eq!(app.tabs[0].meta.branch, "main");
}
```

If `FakeGit` has no way to make `current_branch` fail, add one in the same spirit as its existing failure knobs (see how the other fallible methods are stubbed) and keep the addition minimal.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flightdeck --lib base_tab_records_the_checked_out_branch`
Expected: FAIL — `assertion failed: left: "main", right: "spike"`.

- [ ] **Step 3: Implement**

In `begin_base_agent_tab` (`src/app/state.rs:1058`), after the existing single-base-tab refusal and before the name fallback:

```rust
        // The branch a base tab is *actually* on. `base` is the configured base
        // branch, which is not necessarily what HEAD points at in the project
        // root — and `meta.branch` is what Push Branch pushes, so recording the
        // base here would push the wrong ref. A detached HEAD has no branch
        // name; fall back to the base.
        let head = services
            .git
            .current_branch(&self.repo_root)
            .unwrap_or_else(|_| base.clone());
```

Use `head` for the display-name fallback, the slug fallback and `TabState::branch`, leaving `base_branch` as `base`:

```rust
        let display_name = if name.trim().is_empty() {
            head.clone()
        } else {
            name.trim().to_string()
        };
        let slug = {
            let s = slugify(&display_name);
            if s.is_empty() { slugify(&head) } else { s }
        };
```

and in the `TabState` literal (line ~1096):

```rust
            branch: head.clone(),
            base_branch: base.clone(),
```

The returned `WorktreeJob` keeps `branch: base` → change it to `head` as well for consistency, but note `needs_create` is `false`, so nothing acts on it. Adjust the existing assertion at `src/app/state.rs:2820` if it asserted the old branch value.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flightdeck --lib base_tab`
Expected: PASS, including the pre-existing base-tab tests.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo test -p flightdeck --lib
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
git add src/app/state.rs
git commit -m "fix: a base-branch tab records the branch actually checked out

meta.branch is what Push Branch pushes, so recording the configured base
was a live bug when HEAD was elsewhere (specs/ISOLATED_MODE.md §5)."
```

---

### Task 6: Redirect the agent status plumbing to a status root

Spec §8. The largest task. `prepare_status_launch` writes a Claude plugin dir / OpenCode plugin into the worktree and seeds `.flightdeck/agent-status`; the generated hooks then append to a path *relative to the agent's working directory*. Redirecting means threading a root through and templating absolute paths into the hook bodies.

> **Cross-platform warning (spec §8.1).** The hook bodies are POSIX-shell one-liners embedded in JSON (Claude), TOML (Codex) and JavaScript (OpenCode). Templating an absolute path in means shell quoting *and* the host format's escaping, and a Windows path brings backslashes into both. Read `.agents/skills/flightdeck-cross-platform-parity` before starting. Serialize every JSON string through `serde_json::Value::String(..).to_string()` and every TOML string through `toml::Value::String(..)` (the file already does the latter at `src/agents/setup.rs:157`) — never hand-build a quoted string with `format!("\"{path}\"")`.

**Files:**
- Modify: `src/agents/setup.rs:84-156` (`prepare_status_launch`), `src/agents/setup.rs:158-165` (`codex_hook_override`), `src/agents/setup.rs:175-190` (`CLAUDE_PLUGIN_HOOKS`), the OpenCode runtime plugin constant (`OPENCODE_RUNTIME_PLUGIN`, ~line 200), `src/app/state.rs:389-391` (`agent_status_file`), `src/app/state.rs:1862` and `:1908` (the two `prepare_status_launch` calls), `src/app/state.rs:1196` and `:2112` (the two `agent_status_file` calls), `src/remote/bridge.rs:904-918` (`agent_question_path`)
- Test: `src/agents/setup.rs` (existing tests module) and `src/app/state.rs`

**Interfaces:**
- Consumes: `AppState::isolated_status_root` from Task 3.
- Produces:
  - `pub fn prepare_status_launch(fs: &dyn FileSystem, agent: &AgentDef, worktree: &Path, status_root: &Path, containerized: bool) -> Result<StatusLaunch>` — `status_root` is new and is where the plugin dir, the status file and `agent-question.json` live. Passing `worktree` for it reproduces today's behavior exactly.
  - `pub fn agent_status_file(status_root: &Path) -> PathBuf` — parameter renamed; semantics unchanged when the root is the worktree.
  - `fn AppState::status_root(&self, worktree: &Path) -> PathBuf` — private helper: `self.isolated_status_root.clone().unwrap_or_else(|| worktree.to_path_buf())`.
  - `pub fn agent_question_path(status_root: &Path) -> PathBuf` in `src/remote/bridge.rs` — parameter renamed.

- [ ] **Step 1: Write the failing test**

```rust
// in src/agents/setup.rs tests
#[test]
fn status_launch_writes_only_under_the_status_root() {
    let fs = FakeFs::new();
    let agent = AgentDef { /* command "claude", as neighbouring tests build it */ };

    let launch = prepare_status_launch(
        &fs,
        &agent,
        Path::new("/repo"),
        Path::new("/tmp/fd-isolated-1"),
        false,
    )
    .unwrap();

    assert!(
        fs.writes_under(Path::new("/repo")).is_empty(),
        "nothing may land in the project: {:?}",
        fs.writes_under(Path::new("/repo"))
    );
    assert!(
        !fs.writes_under(Path::new("/tmp/fd-isolated-1")).is_empty(),
        "the plumbing goes to the status root instead"
    );
    assert!(launch.explicit, "a known backend still reports explicit status");
    assert!(
        launch.args.iter().any(|a| a.contains("/tmp/fd-isolated-1")),
        "the plugin dir handed to the agent points at the status root: {:?}",
        launch.args
    );
}

#[test]
fn claude_hooks_target_the_absolute_status_file() {
    let fs = FakeFs::new();
    let agent = AgentDef { /* command "claude" */ };
    prepare_status_launch(&fs, &agent, Path::new("/repo"), Path::new("/tmp/root"), false).unwrap();

    let hooks = fs
        .file_contents(Path::new("/tmp/root/.flightdeck/runtime/status/claude/hooks/hooks.json"))
        .expect("hooks.json written under the status root");
    assert!(
        hooks.contains("/tmp/root/.flightdeck/agent-status"),
        "hook bodies must carry the absolute status path, not a cwd-relative one: {hooks}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&hooks).is_ok(),
        "the templated hooks must still be valid JSON: {hooks}"
    );
}

#[test]
fn codex_hook_override_quotes_the_absolute_path_as_toml() {
    let ov = codex_hook_override("Stop", "idle", Path::new("/tmp/root/.flightdeck/agent-status"));
    assert!(ov.contains("/tmp/root/.flightdeck/agent-status"));
    assert!(
        toml::from_str::<toml::Value>(&ov.replace("hooks.Stop=", "x=")).is_ok(),
        "the override must be parseable TOML: {ov}"
    );
}

#[test]
fn status_root_equal_to_the_worktree_is_todays_behavior() {
    // Regression guard for the normal path.
    let fs = FakeFs::new();
    let agent = AgentDef { /* command "claude" */ };
    prepare_status_launch(&fs, &agent, Path::new("/repo"), Path::new("/repo"), false).unwrap();

    assert!(
        fs.exists(Path::new("/repo/.flightdeck/agent-status")),
        "the seeded status file still lands in the worktree"
    );
    assert!(
        fs.exists(Path::new("/repo/.flightdeck/runtime/status/claude/hooks/hooks.json")),
        "so does the plugin"
    );
}
```

Use whatever contents accessor `FakeFs` already exposes (see `src/testing/mod.rs:66`); the name in the test above may need adjusting to match it.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flightdeck --lib status_launch_writes_only status_root_equal`
Expected: FAIL — `this function takes 4 arguments but 5 arguments were supplied`.

- [ ] **Step 3: Implement**

In `src/agents/setup.rs`, add the parameter and derive both paths from it:

```rust
pub fn prepare_status_launch(
    fs: &dyn FileSystem,
    agent: &AgentDef,
    worktree: &Path,
    status_root: &Path,
    containerized: bool,
) -> Result<StatusLaunch> {
```

Replace `worktree.join(STATUS_RUNTIME_DIR)` with `status_root.join(STATUS_RUNTIME_DIR)` and `worktree.join(".flightdeck/agent-status")` with the absolute status path:

```rust
    let runtime = status_root.join(STATUS_RUNTIME_DIR);
    let status_file = status_root.join(".flightdeck").join("agent-status");
    fs.create_dir_all(&runtime)?;
    fs.write(&status_file, "idle\n")?;
```

`worktree` stays a parameter: the containerized branch keeps mapping the runtime dir to `/workspace/...`, and spec §8.2 requires a containerized run to keep the in-worktree root (the caller enforces that; see below).

Turn `CLAUDE_PLUGIN_HOOKS` from a `const` into a function that templates the path. Every one-liner currently reads `[ -d .flightdeck ] && printf 'idle\n' >> .flightdeck/agent-status; exit 0`; the `[ -d .flightdeck ]` guard existed only because the path was relative, and with an absolute path the correct guard is on the parent directory:

```rust
/// The Claude plugin's hook bodies, targeting an absolute status file.
///
/// Each body is a POSIX-shell one-liner embedded in JSON, so the path is
/// serialized through `serde_json` — never interpolated into a hand-quoted
/// string — and a Windows path's backslashes are escaped correctly by that.
fn claude_plugin_hooks(status_file: &Path) -> String {
    let sf = status_file.to_string_lossy();
    let body = |state: &str| -> String {
        format!("printf '{state}\\n' >> {}; exit 0", shell_quote(&sf))
    };
    // Build the document with serde_json::json! so every string is escaped by
    // the serializer, then render it with `to_string()`.
    serde_json::json!({
        "SessionStart":     [{"hooks": [{"type": "command", "command": body("idle")}]}],
        "UserPromptSubmit": [{"hooks": [{"type": "command", "command": body("working")}]}],
        "Stop":             [{"hooks": [{"type": "command", "command": body("idle")}]}],
        "StopFailure":      [{"hooks": [{"type": "command", "command": body("idle")}]}],
        "PermissionRequest":[{"hooks": [{"type": "command", "command": body("waiting")}]}],
        // ... keep every event the current constant defines, including the
        // PreToolUse/AskUserQuestion entry that cats stdin into
        // agent-question.json — template that absolute path the same way.
    })
    .to_string()
}

/// Single-quote a path for a POSIX shell, escaping embedded single quotes.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
```

Transcribe **every** event from the existing constant (`src/agents/setup.rs:175-190`) — `SessionStart`, `UserPromptSubmit`, `Stop`, `StopFailure`, `PermissionRequest`, `PreToolUse` (matcher `AskUserQuestion`, which also writes `agent-question.json`), `PostToolUse`, and the two OpenCode-ish matchers if present. Dropping one silently loses a status transition.

Give `codex_hook_override` the path (`src/agents/setup.rs:158`):

```rust
fn codex_hook_override(event: &str, state: &str, status_file: &Path) -> String {
    let command = format!(
        "printf '{state}\\n' >> {}; exit 0",
        shell_quote(&status_file.to_string_lossy())
    );
    format!(
        "hooks.{event}=[{{hooks=[{{type=\"command\",command={}}}]}}]",
        toml::Value::String(command)
    )
}
```

For OpenCode, the runtime plugin JS computes its own `fdDir`; pass the absolute status file in the same way — either by templating the constant into a function or by adding an environment variable the plugin reads. Templating keeps it consistent with the other two backends; whichever you choose, the test above must see the absolute path reach the plugin.

Then in `src/app/state.rs`:

```rust
/// Path to a tab's agent status event file under `status_root` (SPECS §24, §32).
/// The root is the tab's worktree in a normal run, and a temp directory in an
/// isolated one.
pub fn agent_status_file(status_root: &Path) -> PathBuf {
    status_root.join(".flightdeck").join("agent-status")
}
```

Add the private helper next to `build_primary_spawn`:

```rust
    /// Where this run keeps the agent status plumbing for a tab whose working
    /// directory is `worktree` (SPECS §32). Normally the worktree itself; an
    /// isolated run redirects it out of the project. Containerized runs always
    /// use the worktree, because a temp directory outside it is not bind-mounted
    /// into the container (specs/ISOLATED_MODE.md §8.2).
    fn status_root(&self, worktree: &Path) -> PathBuf {
        if self.config.containers.enabled {
            return worktree.to_path_buf();
        }
        self.isolated_status_root
            .clone()
            .unwrap_or_else(|| worktree.to_path_buf())
    }
```

Update the four call sites:
- `src/app/state.rs:1862` → `prepare_status_launch(services.fs, agent, worktree_abs, &self.status_root(worktree_abs), false)?`
- `src/app/state.rs:1908` → `prepare_status_launch(services.fs, agent, worktree_abs, worktree_abs, true)?` (containerized: always the worktree, per §8.2)
- `src/app/state.rs:1196` → `let status_file = agent_status_file(&self.status_root(&worktree_abs));`
- `src/app/state.rs:2112` → `let status_file = agent_status_file(&self.status_root(&cwd));`

And in `src/remote/bridge.rs:918`, rename the parameter to `status_root` and update its callers to pass the same root the tab's `status_file` was derived from (the tab already carries it — derive it from `tab.status_file`'s grandparent, or thread the root through; prefer whichever the surrounding code makes honest).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flightdeck --lib status`
Expected: PASS — the four new tests plus every pre-existing status test. Several existing tests assert on the old relative hook bodies; update them to the absolute form, and do **not** weaken an assertion to make it pass.

- [ ] **Step 5: Verify on Windows**

Run the test suite on a Windows host (or a Windows CI job) and confirm `claude_hooks_target_the_absolute_status_file` and `codex_hook_override_quotes_the_absolute_path_as_toml` pass with a real `C:\...` temp path. If they do not, stop and report — do not paper over it with a `#[cfg(unix)]` on the test.

- [ ] **Step 6: Run the full gate and commit**

```bash
cargo test -p flightdeck --lib
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
git add src/agents/setup.rs src/app/state.rs src/remote/bridge.rs
git commit -m "feat: agent status plumbing takes an explicit status root

Hook bodies now carry an absolute status path, so the plumbing can live
outside the project. Worktree root reproduces today's behavior; containers
keep the in-worktree path (specs/ISOLATED_MODE.md §8)."
```

---

### Task 7: Launch the single isolated session

**Files:**
- Modify: `src/lib.rs:123-152` (consume the parsed flag), `src/lib.rs:188-203` (skip the workspace reopen), `src/lib.rs:266-274` (create the tab instead of resuming), plus the `startup(...)` call site
- Test: `src/lib.rs` (existing tests module)

**Interfaces:**
- Consumes: `parse_isolated` (Task 2), `startup(.., isolated)` (Task 4), `AppState::set_isolated` (Task 3), `begin_new_agent_tab_ex` with `run_on_base = true` (Task 5), `AppState::status_root` (Task 6).
- Produces: `fn isolated_status_dir() -> PathBuf` — `std::env::temp_dir().join(format!("flightdeck-isolated-{}", std::process::id()))`. Task 8 removes it.

- [ ] **Step 1: Write the failing test**

`run()` owns the real terminal and cannot be tested. Extract the tab creation into a testable helper and test that:

```rust
#[test]
fn isolated_run_creates_exactly_one_base_tab() {
    let git = FakeGit::new(/* current branch "main" */);
    let fs = FakeFs::new();
    let pty = FakePty::new();
    let clock = FakeClock::new();
    let services = services(&git, &fs, &pty, &clock);
    let mut state = startup(&services, Path::new("/repo"), Path::new("/repo"), true).unwrap();
    state.set_isolated(Some(PathBuf::from("/tmp/fd-isolated-test")));

    start_isolated_session(&mut state, &services).unwrap();

    assert_eq!(state.tabs.len(), 1, "exactly one session");
    assert!(state.tabs[0].meta.runs_on_base, "it runs in the repo root");
    assert_eq!(state.selected_tab, Some(0));
    assert!(
        state.tabs[0].meta.resume_args.is_empty(),
        "a fresh session, never a continued one"
    );
    assert_eq!(pty.spawns().len(), 1, "the agent is spawned");
    assert!(
        git.added_worktrees().is_empty() && git.created_branches().is_empty(),
        "not one git mutation"
    );
    assert!(
        fs.writes_under(Path::new("/repo")).is_empty(),
        "and nothing written to the project: {:?}",
        fs.writes_under(Path::new("/repo"))
    );
}

#[test]
fn isolated_session_status_file_lives_outside_the_project() {
    let git = FakeGit::new(/* as above */);
    let fs = FakeFs::new();
    let pty = FakePty::new();
    let clock = FakeClock::new();
    let services = services(&git, &fs, &pty, &clock);
    let mut state = startup(&services, Path::new("/repo"), Path::new("/repo"), true).unwrap();
    state.set_isolated(Some(PathBuf::from("/tmp/fd-isolated-test")));

    start_isolated_session(&mut state, &services).unwrap();

    let status = state.tabs[0].status_file.clone().expect("a status file");
    assert!(
        status.starts_with("/tmp/fd-isolated-test"),
        "status must be redirected out of the project: {}",
        status.display()
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flightdeck --lib isolated_run_creates_exactly_one`
Expected: FAIL — `cannot find function 'start_isolated_session'`.

- [ ] **Step 3: Implement**

Add to `src/lib.rs` near `startup`:

```rust
/// The temp directory an isolated run keeps its agent status plumbing in
/// (SPECS §32). Per-process so two concurrent isolated runs cannot collide.
fn isolated_status_dir() -> PathBuf {
    std::env::temp_dir().join(format!("flightdeck-isolated-{}", std::process::id()))
}

/// Create the one session an isolated run consists of: the default agent, in the
/// repository root, on the branch already checked out, with no worktree and no
/// git mutation (SPECS §32). The base-tab path's `WorktreeJob` has
/// `needs_create == false`, so `materialize_worktree` is deliberately not called.
fn start_isolated_session(state: &mut AppState, services: &Services) -> Result<()> {
    let job = state.begin_new_agent_tab_ex("", None, true, services)?;
    debug_assert!(!job.needs_create, "an isolated session never creates a worktree");
    state.finalize_new_tab(&job.tab_id, services)?;
    Ok(())
}
```

In `run()`, rename `_isolated` back to `isolated`, pass it to `startup`, then:

- Skip the workspace reopen (`src/lib.rs:188-203`): wrap the whole `if let Some(ref wp) = ws_path { ... }` block in `if !isolated { ... }`. Also skip computing `ws_path` at all in isolated mode — bind it as `let ws_path = if isolated { None } else { workspace_state_path() };`, which makes Task 8's teardown skip fall out for free.
- Attach the status root right after `startup` returns, in `open_project` or immediately after the launch project is built, whichever keeps `run()` readable:

```rust
    if isolated {
        launch.state.set_isolated(Some(isolated_status_dir()));
    }
```

  (`startup` already called `set_isolated(None)`; this second call supplies the root. If that double call reads badly, thread the root into `startup` instead and drop the `None` there — either is fine, but say which in the commit message.)
- Replace the resume block (`src/lib.rs:266-274`) with:

```rust
    {
        let active = workspace.active;
        let p = &mut workspace.projects[active];
        let services = env.services(&p.git);
        if isolated {
            // One fresh session; nothing to resume, because nothing was recovered.
            if let Err(e) = start_isolated_session(&mut p.state, &services) {
                p.state.add_warning(format!("Isolated session failed to start: {e}"));
            }
        } else {
            let _ = p.state.resume_agents(&services);
        }
    }
```

`add_warning` is private to `AppState`; make it `pub(crate)` or use `p.state.warnings.push(..)` guarded against duplicates, matching how `startup` pushes its warnings.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flightdeck --lib isolated_run isolated_session`
Expected: PASS, 2 tests.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo test -p flightdeck --lib
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
git add src/lib.rs
git commit -m "feat: an isolated run launches one fresh session in the cwd

A base-branch tab in the repo root: no worktree, no branch, no git
mutation, status plumbing in a temp dir (specs/ISOLATED_MODE.md §5)."
```

---

### Task 8: Teardown leaves nothing behind

**Files:**
- Modify: `src/lib.rs:284-301` (the persist + workspace-save teardown block)
- Test: `src/lib.rs`

**Interfaces:**
- Consumes: `isolated_status_dir` (Task 7), `AppState::isolated` (Task 3).
- Produces: `fn cleanup_isolated_run(fs: &dyn FileSystem, status_dir: &Path)` — removes the temp status directory, ignoring failure.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn cleanup_isolated_run_removes_the_temp_status_dir() {
    let fs = FakeFs::new()
        .with_dir("/tmp/fd-isolated-9")
        .with_file("/tmp/fd-isolated-9/.flightdeck/agent-status", "idle\n");

    cleanup_isolated_run(&fs, Path::new("/tmp/fd-isolated-9"));

    assert!(
        fs.writes().iter().any(|p| p == Path::new("/tmp/fd-isolated-9")),
        "the temp dir must be removed"
    );
}

#[test]
fn cleanup_isolated_run_tolerates_a_missing_dir() {
    let fs = FakeFs::new();
    // Must not panic: the agent may never have started.
    cleanup_isolated_run(&fs, Path::new("/tmp/fd-isolated-absent"));
}
```

`persist_quietly` and `save_workspace` are called inside `run()`, which is untestable; their skip is covered by Task 3's `persist` guard plus the `ws_path = None` binding from Task 7. State that in the commit message rather than pretending a test covers it.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flightdeck --lib cleanup_isolated_run`
Expected: FAIL — `cannot find function 'cleanup_isolated_run'`.

- [ ] **Step 3: Implement**

```rust
/// Remove an isolated run's temp status directory (SPECS §32). Best effort: a
/// leftover directory under the OS temp dir is harmless, and teardown must never
/// fail on it.
fn cleanup_isolated_run(fs: &dyn FileSystem, status_dir: &Path) {
    let _ = fs.remove_dir_all(status_dir);
}
```

In `run()`'s teardown (`src/lib.rs:284`), skip persistence and clean up:

```rust
    let mut persist_result = Ok(());
    if !isolated {
        for p in workspace.projects.iter() {
            let services = env.services(&p.git);
            if let Err(e) = persist_quietly(&p.state, &services) {
                persist_result = Err(e);
            }
        }
    }
```

The `if let Some(wp) = &ws_path` workspace save below it needs no change — Task 7 made `ws_path` `None` in isolated mode. Add the cleanup after the session-termination loop at the end of `run()`, so it runs after the agents are dead and cannot race a hook still writing:

```rust
    if isolated {
        cleanup_isolated_run(&fs, &isolated_status_dir());
    }
```

Note that `persist` is already a no-op under `isolated` (Task 3), so the `if !isolated` above is belt and braces — keep it anyway, because `persist_quietly` is a free function that does not route through `AppState::persist`. **Verify that claim** by reading `persist_quietly`; if it does call `AppState::persist`, keep the guard regardless and say so in the commit message.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flightdeck --lib cleanup_isolated_run`
Expected: PASS, 2 tests.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo test -p flightdeck --lib
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
git add src/lib.rs
git commit -m "feat: isolated teardown persists nothing and removes its temp dir

specs/ISOLATED_MODE.md §7."
```

---

### Task 9: Refuse the blocked actions

Spec §6. Guards go **inside the flow functions**, not at their call sites: each flow is reachable from a keybinding, a mouse click and the palette, and guarding the function covers all three at once.

**Files:**
- Modify: `src/lib.rs:3498` (`start_new_tab_flow`), `src/lib.rs:3580` (`start_open_project_flow`), `src/lib.rs:3605` (`start_close_project_flow`), and the three `workspace.switch(..)` sites (`src/lib.rs:3243`, `:4515`, `:4520`)
- Test: `src/lib.rs`

**Interfaces:**
- Consumes: `AppState::isolated` (Task 3).
- Produces: `fn switch_project(workspace: &mut Workspace, env: &Env, sel: Selector, ui: &mut Ui)` — the guarded replacement for the three bare `workspace.switch(sel); resume_active_project_agents(..)` pairs. Also the shared copy `const ISOLATED_REFUSAL: &str`.

- [ ] **Step 1: Write the failing test**

```rust
const ISOLATED_MSG_FRAGMENT: &str = "isolated";

#[test]
fn isolated_refuses_the_new_tab_flow() {
    let mut state = /* an isolated AppState, via startup(.., true) as in Task 4 */;
    let mut ui = Ui::default();

    start_new_tab_flow(&state, &mut ui);

    let dialog = ui.take_notification().expect("a refusal is surfaced");
    assert!(
        dialog.body_contains(ISOLATED_MSG_FRAGMENT),
        "the refusal must say why: {dialog:?}"
    );
    assert!(
        matches!(ui.prompt_kind(), None),
        "and no new-tab prompt may open"
    );
}

#[test]
fn isolated_refuses_opening_another_project() {
    let mut workspace = /* a one-project workspace whose state is isolated */;
    let mut ui = Ui::default();
    let env = /* Env over the fakes */;

    start_open_project_flow(&workspace, &env, &mut ui);

    assert!(
        ui.take_notification()
            .expect("a refusal")
            .body_contains(ISOLATED_MSG_FRAGMENT)
    );
}

#[test]
fn isolated_refuses_switching_project() {
    let mut workspace = /* two projects, active one isolated */;
    let mut ui = Ui::default();
    let env = /* Env over the fakes */;
    let before = workspace.active;

    switch_project(&mut workspace, &env, Selector::Next, &mut ui);

    assert_eq!(workspace.active, before, "the active project must not change");
    assert!(ui.take_notification().is_some(), "and the user is told why");
}

#[test]
fn a_normal_run_still_opens_the_new_tab_prompt() {
    // Regression guard.
    let state = /* a non-isolated AppState */;
    let mut ui = Ui::default();
    start_new_tab_flow(&state, &mut ui);
    assert!(ui.prompt_kind().is_some(), "the normal flow is untouched");
}
```

`Ui::take_notification`, `Ui::prompt_kind` and `Dialog::body_contains` are illustrative: use whatever the existing `Ui` tests in `src/lib.rs` (around line 5303, which calls `start_new_tab_flow`) already use to inspect a prompt or a message. Do not add new accessors if equivalents exist.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flightdeck --lib isolated_refuses`
Expected: FAIL — the flows currently open their prompts, so `take_notification` is `None`.

- [ ] **Step 3: Implement**

Add the shared copy near `start_new_tab_flow`:

```rust
/// Why an action is unavailable in an isolated run (SPECS §32). One string, so
/// every refusal reads identically wherever the user meets it.
const ISOLATED_REFUSAL: &str =
    "Not available in an isolated run (--isolated): it has one session in this \
     directory and opens nothing else.";
```

Guard each flow at its first line:

```rust
fn start_new_tab_flow(state: &AppState, ui: &mut Ui) {
    if state.isolated {
        ui.message(ISOLATED_REFUSAL);
        return;
    }
    // ... unchanged
```

```rust
fn start_open_project_flow(workspace: &Workspace, env: &Env, ui: &mut Ui) {
    if workspace.active_project().state.isolated {
        ui.message(ISOLATED_REFUSAL);
        return;
    }
    // ... unchanged
```

```rust
fn start_close_project_flow(workspace: &Workspace, ui: &mut Ui, index: usize) {
    if workspace.active_project().state.isolated {
        ui.message(ISOLATED_REFUSAL);
        return;
    }
    // ... unchanged (the existing "only project" check follows)
```

Add the switch helper and use it at all three sites:

```rust
/// Switch the active project, resuming its agents (SPECS §22). Refused in an
/// isolated run, which has exactly one project by construction (SPECS §32).
fn switch_project(workspace: &mut Workspace, env: &Env, sel: Selector, ui: &mut Ui) {
    if workspace.active_project().state.isolated {
        ui.message(ISOLATED_REFUSAL);
        return;
    }
    workspace.switch(sel);
    resume_active_project_agents(workspace, env);
}
```

Replace `src/lib.rs:3243` (`KeyAction::SwitchProject`), `:4515` (`SwitchProjectNext`) and `:4520` (`SwitchProjectPrev`) with `switch_project(workspace, env, sel, ui)` / `Selector::Next` / `Selector::Prev`.

The mouse handlers at `src/lib.rs:2739` and `:2741` call `start_close_project_flow` and `start_open_project_flow`, so they are covered by the guards above. Check whether a project-tab *click* switches directly rather than through `workspace.switch` — if it does (look near `ProjectHit`), route it through `switch_project` too.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flightdeck --lib isolated_refuses a_normal_run_still_opens`
Expected: PASS, 4 tests.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo test -p flightdeck --lib
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
git add src/lib.rs
git commit -m "feat: refuse project actions and new session tabs when isolated

Guards sit inside the flow functions, so keybinding, mouse and palette
entry points are all covered (specs/ISOLATED_MODE.md §6)."
```

---

### Task 10: Hide the blocked entries from the palette

Presentation only — Task 9 is the real gate. The palette already has this exact pattern for the two Remote entries (`set_paired` / `entry_visible`), so follow it.

**Files:**
- Modify: `src/tui/palette.rs:236-268` (`CommandPalette` field, setter, `entry_visible`), and the site that calls `set_paired` when opening the palette (find it with `rg 'set_paired' src/lib.rs`)
- Test: `src/tui/palette.rs`

**Interfaces:**
- Consumes: `AppState::isolated` (Task 3).
- Produces: `pub fn CommandPalette::set_isolated(&mut self, isolated: bool)` — mirrors `set_paired`, resets the selection to 0.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn isolated_palette_hides_project_and_new_tab_entries() {
    let mut p = CommandPalette::new();
    p.set_isolated(true);
    let labels: Vec<&str> = p.filtered().iter().map(|e| e.label).collect();

    for hidden in [
        "Open Project",
        "Close Project",
        "Next Project",
        "Previous Project",
        "New Agent Session Tab",
    ] {
        assert!(
            !labels.contains(&hidden),
            "'{hidden}' must be hidden in an isolated run"
        );
    }
    for shown in ["Push Branch", "Pull Base", "Open Configuration", "Open Shell", "Quit"] {
        assert!(labels.contains(&shown), "'{shown}' stays available");
    }
}

#[test]
fn a_normal_palette_shows_everything() {
    // Regression guard: REQUIRED_ACTION_COUNT is the unfiltered normal count.
    let p = CommandPalette::new();
    let labels: Vec<&str> = p.filtered().iter().map(|e| e.label).collect();
    assert!(labels.contains(&"Open Project"));
    assert!(labels.contains(&"New Agent Session Tab"));
}
```

Use the palette's real accessor for the filtered list (the existing tests around `src/tui/palette.rs:565` show it); `filtered()` above is a placeholder for whatever it is called.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flightdeck --lib isolated_palette`
Expected: FAIL — `no method named 'set_isolated'`.

- [ ] **Step 3: Implement**

Add the field alongside `is_paired` (`src/tui/palette.rs:238`):

```rust
    /// Whether this is an isolated run (SPECS §32), which hides the project
    /// entries and "New Agent Session Tab". Presentation only — the flows
    /// themselves refuse independently, because keybindings bypass the palette.
    isolated: bool,
```

```rust
    /// Set whether this is an isolated run, which decides the visibility of the
    /// project and new-session entries. Resets the selection so it can never
    /// point past the shorter filtered list.
    pub fn set_isolated(&mut self, isolated: bool) {
        self.isolated = isolated;
        self.selected = 0;
    }
```

Extend `entry_visible` (`src/tui/palette.rs:258`):

```rust
    fn entry_visible(&self, entry: &PaletteEntry) -> bool {
        match entry.action {
            PaletteAction::PairPhone => !self.is_paired,
            PaletteAction::UnpairPhone => self.is_paired,
            // An isolated run has one session in one project (SPECS §32).
            PaletteAction::OpenProject
            | PaletteAction::CloseProject
            | PaletteAction::SwitchProjectNext
            | PaletteAction::SwitchProjectPrev
            | PaletteAction::NewAgentTab => !self.isolated,
            _ => true,
        }
    }
```

Leave `REQUIRED_ACTION_COUNT` at 30 — it counts the normal, unfiltered palette. Call `set_isolated` wherever `set_paired` is called when the palette opens, passing the active project's `state.isolated`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flightdeck --lib palette`
Expected: PASS — the 2 new tests plus every pre-existing palette test, `REQUIRED_ACTION_COUNT` included.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo test -p flightdeck --lib
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
git add src/tui/palette.rs src/lib.rs
git commit -m "feat: hide project and new-session palette entries when isolated

specs/ISOLATED_MODE.md §6."
```

---

### Task 11: Make the mode visible

Spec §9. Without this, a forgotten `-I` looks exactly like data loss.

**Files:**
- Modify: `src/tui/render.rs:1559-1567` (`draw_status_bar`), `src/tui/render.rs:1573-1633` (`status_bar_text`), `src/tui/render.rs:1856-1900` (`draw_help_overlay`), and the `draw_help_overlay` call site (find it with `rg 'draw_help_overlay' src`)
- Test: `src/tui/render.rs`

**Interfaces:**
- Consumes: `AppState::isolated` (Task 3).
- Produces:
  - `pub fn status_bar_text(mode: InputMode, ui: &UiConfig, update_available: Option<&str>, isolated: bool) -> Line<'static>` — fourth parameter new.
  - `pub fn draw_help_overlay(frame: &mut Frame, area: Rect, use_f2: bool, isolated: bool)` — fourth parameter new.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn status_bar_shows_the_isolated_badge() {
    let ui = crate::contracts::UiConfig::default();
    let line = status_bar_text(InputMode::App, &ui, None, true);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        text.contains("ISOLATED"),
        "an isolated run must be unmistakable: {text}"
    );
}

#[test]
fn status_bar_has_no_badge_in_a_normal_run() {
    let ui = crate::contracts::UiConfig::default();
    let line = status_bar_text(InputMode::App, &ui, None, false);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(!text.contains("ISOLATED"), "no badge normally: {text}");
}

#[test]
fn status_bar_shows_both_the_badge_and_the_update_hint() {
    // The two trailing spans must coexist, not overwrite each other.
    let ui = crate::contracts::UiConfig::default();
    let line = status_bar_text(InputMode::App, &ui, Some("9.9.9"), true);
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("ISOLATED") && text.contains("9.9.9"), "{text}");
}
```

For the help overlay, follow the buffer-rendering pattern the existing render tests use (see `status_bar_appears_at_bottom_of_buffer` at `src/tui/render.rs:3568`) and assert the isolated note appears when the flag is set and not otherwise.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p flightdeck --lib status_bar_shows_the_isolated`
Expected: FAIL — `this function takes 3 arguments but 4 arguments were supplied`.

- [ ] **Step 3: Implement**

In `status_bar_text`, add the parameter and push the badge **before** the update hint so the order is stable:

```rust
    // Isolated run (SPECS §32): nothing persists and several actions are gone,
    // so say so permanently rather than once at launch.
    if isolated {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "ISOLATED",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }
```

Pick a background that is not already used by the mode chips (`terminal_mode_color` / `app_mode_color`, default green/cyan) or the update hint (yellow); magenta is free.

Update `draw_status_bar` to pass `state.isolated`.

In `draw_help_overlay`, add the parameter and append a section when set:

```rust
    let mut help_text = vec![ /* ... existing lines ... */ ];
    if isolated {
        help_text.push(Line::raw(""));
        help_text.push(Line::from(Span::styled(
            "Isolated run (--isolated)",
            Style::default().fg(Color::Magenta),
        )));
        help_text.push(Line::raw("  Nothing is saved and nothing was continued."));
        help_text.push(Line::raw("  One session, in this directory, on the current branch."));
        help_text.push(Line::raw("  No other projects and no new session tabs."));
    }
```

The overlay is a fixed 64x40 (`layout::centered_overlay(area, 64, 40)`); four extra lines may overflow it. Check the rendered height and raise the overlay height in the isolated branch if needed, or trim to two lines — but do not let it clip silently.

Update the `draw_help_overlay` call site to pass `state.isolated`, and fix any pre-existing `status_bar_text` test call sites to pass `false`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p flightdeck --lib status_bar help_overlay`
Expected: PASS — the new tests plus every pre-existing status-bar test.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo test -p flightdeck --lib
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
git add src/tui/render.rs src/lib.rs
git commit -m "feat: ISOLATED badge in the status bar and a help-overlay note

A forgotten -I must not look like data loss (specs/ISOLATED_MODE.md §9)."
```

---

### Task 12: Documentation, SPECS §32, and the shipping gate

**Files:**
- Modify: `specs/SPECS.md` (append §32 after §31, which ends at line ~1320), `README.md` (the usage/flags section), `CHANGELOG.md`
- Create: nothing

**Interfaces:**
- Consumes: everything above.
- Produces: no code.

- [ ] **Step 1: Write SPECS §32**

Append to `specs/SPECS.md`, matching the numbered-section style of §31:

```markdown
## 32. Isolated Mode

`flightdeck --isolated` / `-I` launches a throwaway run: one fresh agent session
in the current working directory, with nothing continued and nothing written.

An isolated run:

- reads the effective configuration (global + project) if it exists, and writes
  none of it — no first-run `config.toml`, no `.gitignore` entry, no global base
- never reads `state.json` and never runs recovery, so no tab is reconstructed
- creates exactly one Agent Session Tab, running the configured default agent in
  the repository root on the branch already checked out, with no dedicated
  worktree and no git mutation of any kind
- forces `ui.auto_continue` off, so even Restart Agent starts a fresh session
- disables the update check (it makes a network call and writes a cache file)
- keeps its agent status plumbing in a temp directory outside the project,
  removed on exit; containerized runs are the one exception and keep the
  in-worktree path, because a temp directory is not bind-mounted into the
  container
- refuses Open / Close / Next / Previous Project and New Agent Session Tab, and
  writes neither `state.json` nor the workspace file on exit
- shows a permanent `ISOLATED` badge in the status bar

Push Branch, Pull Base, Open Configuration, phone pairing, child terminals and
additional agents in the session remain available. The rule constrains what
FlightDeck writes on its own; an explicit user action that exists to write still
writes.

Isolated mode is a per-run flag and never a configuration setting: a persisted
setting that suppressed persistence would be a trap. See
`specs/ISOLATED_MODE.md` for the design and `specs/ISOLATED_MODE_PLAN.md` for the
implementation record.
```

- [ ] **Step 2: Update the README**

Add `-I, --isolated` to the README's flag/usage listing with the same one-line description used in `print_help` (Task 2), and a short paragraph pointing at SPECS §32. Match the README's existing heading depth and tone; do not restructure it.

- [ ] **Step 3: Update the CHANGELOG**

Add an `Added` entry under the unreleased heading (create it in the existing style if absent):

```markdown
### Added

- `--isolated` / `-I`: a throwaway run with one fresh session in the current
  directory — nothing continued, no worktrees, no other projects, and nothing
  written to the project. The agent status plumbing lives in a temp directory
  removed on exit. See SPECS §32.

### Fixed

- A base-branch Agent Session Tab now records the branch actually checked out
  rather than the configured base, so Push Branch pushes the right ref.
```

- [ ] **Step 4: Run the full gate**

```bash
cargo test -p flightdeck --lib
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

Expected: all three green. Cite the actual output — do not claim green without it.

- [ ] **Step 5: Build a real binary and hand it to the user**

Per `.agents/skills/shipping-flightdeck-changes`: commit, then build, then let the user drive the app before the PR is finalized.

```bash
git add specs/SPECS.md README.md CHANGELOG.md
git commit -m "docs: SPECS §32 isolated mode, README flag, CHANGELOG"
cargo build --release --locked
```

Then tell the user the binary path and exactly what to try:

1. `cd` into any git repo and run `<path>/flightdeck --isolated`. One session appears, the badge reads ISOLATED, the agent runs in the repo root.
2. Confirm the status chip moves (idle → working → idle) as you use the agent — that proves the redirected plumbing works.
3. `git status` in the project: no `.flightdeck` churn, no new `config.toml`, no `.gitignore` change.
4. Ctrl-g: no Open Project, no New Agent Session Tab. Try Ctrl-n — it is refused with a message.
5. Quit, then relaunch normally: the isolated session is not remembered, and the previous normal session's tabs come back untouched.
6. `ls $TMPDIR/flightdeck-isolated-*` after quitting: gone.
7. `flightdeck -I doctor`: errors instead of running doctor.

---

## Self-Review

**Spec coverage.** §1 purpose → the whole plan. §2 definition → Tasks 3, 4, 7, 8. §3 flag → Task 2. §4 startup table → Task 4, with the update check disabled there (see the gap note below). §5 single tab and branch label → Tasks 5 and 7. §6 blocked actions → Tasks 9 and 10. §7 teardown → Task 8. §8 status redirect, §8.1 escaping, §8.2 container exception → Task 6. §9 visibility → Task 11. §10 verification → the test steps throughout, resting on Task 1. §11 non-goals → nothing to build.

**One gap found and closed inline:** spec §4 requires the update check to be disabled, and my first draft put that in `startup()` — where it does not live. The call is in `event_loop` at `src/lib.rs:1274`, so Task 4's Step 3 now covers it explicitly. `event_loop` has no test seam, so it is verified by hand in Task 12's Step 5 (no update hint, no cache file).

**Placeholder scan.** The `/* ... */` markers in the test bodies point at concrete existing constructions with file and line references (`FakeGit` as built at `src/lib.rs:5581`, the base-tab test at `src/app/state.rs:2817`, the palette accessor at `src/tui/palette.rs:565`). They are deliberate — this repo's fakes take arguments I would otherwise be inventing — but an executor must open those references rather than guess. No step says "add error handling" or "write tests for the above".

**Type consistency.** `status_root` is the parameter name in `prepare_status_launch`, `agent_status_file`, `agent_question_path` and `AppState::status_root` — consistent. `isolated` is the field name on `AppState` and `CommandPalette`, and the parameter name in `startup`, `status_bar_text`, `draw_help_overlay` and `parse_isolated`'s return. `isolated_status_root` (the `Option<PathBuf>` field) is deliberately distinct from `status_root` (the resolved path) so the two are never confused. `set_isolated` takes `Option<PathBuf>` on `AppState` and `bool` on `CommandPalette` — different types behind one name; that mirrors `set_paired` and stays clear in context, but an executor should not assume they match.

**Known soft spot.** Tasks 7 and 8 test extracted helpers rather than `run()`, which owns the real terminal and cannot be tested. The workspace-file skip and the `persist_quietly` skip are therefore covered by construction (`ws_path = None`, plus Task 3's guard) rather than by a test. Task 12's step 5 exercises them by hand. This is stated rather than hidden.
