# FlightDeck MVP — Implementation Plan (Subagent-Delegated Build)

This plan is written for a **main coding agent** (Claude Code, Opus 4.8, 1M context)
that will implement the entire FlightDeck MVP described in `specs/SPECS.md` in one
long-running session by **delegating each section to subagents**, each running in
its own fresh context window.

Read this whole document once before starting. Then execute the phases in order.

---

## 0. How to use this plan (delegation protocol)

### 0.1 Roles

- **Main agent (you):** the orchestrator. You own the *contracts* (shared types,
  traits, module skeleton, `Cargo.toml`), the *integration*, and the *acceptance
  gates*. You do **not** implement leaf modules yourself — you delegate them so each
  lands in its own context window. You keep the contract in *your* context so every
  subagent brief is accurate.
- **Subagents:** each implements exactly one task (one module group) against the
  contracts you give it, writes that module's unit tests, and reports back. A
  subagent never sees the other subagents' contexts — its brief must be
  self-contained.

### 0.2 The core principle that makes parallelism safe

> Subagents may depend only on the **contracts** (types + trait signatures defined
> in Phase 0). They must **never** depend on another subagent's concrete
> implementation, and must **never** edit a file they do not own.

Because Git, the filesystem, and the PTY all sit behind traits (per SPECS §26–27),
every logic module can be written and unit-tested against **fakes** that you ship in
Phase 0. This is what lets Phase 1 run as a wide parallel fan-out.

### 0.3 File-ownership rules (prevents merge collisions)

1. **Only the main agent edits shared files:** `Cargo.toml`, `src/main.rs`,
   `src/lib.rs`, every `mod.rs` / module-declaration file, and everything under
   `src/contracts/` (types, traits, errors) and `src/testing/` (fakes).
2. The main agent creates the **full module skeleton up front** (Phase 0): every
   `mod x;` declaration exists, every public type/trait is defined, every function
   body is `todo!()`. The project **compiles** at the end of Phase 0.
3. Each subagent is assigned a **disjoint set of leaf files** and fills in only
   those. It replaces `todo!()` bodies; it does not add or remove `mod`
   declarations, does not touch `Cargo.toml`, does not change trait signatures.
4. If a subagent believes a contract is wrong or insufficient, it **stops and
   reports the proposed change** rather than editing the contract. The main agent
   adjusts the contract centrally, then re-briefs affected subagents.

Because subagents own disjoint files and never touch shared files, parallel
subagents in the same working directory **cannot collide** — no git worktree
isolation is needed for them. (Reserve `isolation: "worktree"` only if you later
decide to let subagents run that mutate the same file; in this plan they don't.)

### 0.4 Model selection policy

Assign per task using this rule of thumb:

- **Opus** → trust-critical correctness, concurrency, and cross-cutting wiring:
  the Git safety logic, the PTY/session layer, the app event loop / command
  dispatch, and the integration test suite. Bugs here are expensive or dangerous
  (SPECS §5 — never mutate commit history).
- **Sonnet** → well-specified, mostly-mechanical modules with clear inputs/outputs
  and strong unit-test coverage: config, fs/paths/ignore, agent registry & status
  classification, persistence/recovery, and TUI rendering/layout.

Each task below states its model. If a Sonnet task surprises a subagent with
genuine difficulty, it should report back and you may re-run it on Opus.

### 0.5 Reasoning effort & background execution

- Spawn independent Phase 1 tasks **in a single message** (multiple `Agent` tool
  calls) so they run concurrently.
- For long tasks you want to overlap with your own integration work, you may use
  `run_in_background: true` and collect results when notified.
- Use `subagent_type: "general-purpose"` (full tool access) for all implementation
  tasks. Use `Explore` only if you need read-only reconnaissance.

### 0.6 Subagent brief template (copy/fill for every delegation)

```
TASK: <task id + title>
MODEL: <Sonnet | Opus>

CONTEXT YOU NEED (self-contained — you cannot see other agents' work):
- We are building FlightDeck, a Rust + Ratatui TUI. Read specs/SPECS.md §<sections>.
- The project skeleton and all shared contracts already exist and COMPILE.
- The contracts you implement against are in src/contracts/ and src/testing/.
  Relevant types/traits: <paste exact signatures the task needs>.

YOU OWN THESE FILES (edit ONLY these):
- <explicit list>

DO NOT:
- Edit Cargo.toml, any mod.rs, src/main.rs, src/lib.rs, src/contracts/*, src/testing/*.
- Change any trait or public type signature. If a contract is wrong, STOP and
  report the exact change you need; do not work around it.
- Add new dependencies. The full dependency set is already in Cargo.toml.

DELIVERABLES:
1. Implement <behavior>, replacing all todo!() in your files.
2. Write unit tests in the same files (#[cfg(test)] mod tests) covering: <list the
   SPECS §26 required test areas for this module, including refusal/negative paths>.
3. Use the fakes in src/testing/ (FakeGit, FakeFs, FakePty) for all tests — no real
   git, no real filesystem mutation outside tempdirs, no real PTY in unit tests.

ACCEPTANCE (must all pass before you report done):
- `cargo build` succeeds with no warnings in your files.
- `cargo test <module path>` passes.
- `cargo clippy -- -D warnings` is clean for your files.
- `cargo fmt` applied.

REPORT BACK: a short summary of what you implemented, the tests you added and what
they cover, any contract friction you hit, and any TODOs you left for integration.
```

### 0.7 Integration checkpoint (run after every phase / parallel batch)

After a batch of subagents reports done, the main agent:

1. Runs `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`.
2. Resolves any cross-module mismatch by adjusting the **contract** (centrally),
   not by editing leaf files ad hoc — then re-briefs the affected subagent.
3. Commits the batch (`git add -A && git commit`) with a clear message before
   starting the next batch, so each phase is a recoverable checkpoint.

---

## 1. Task board (quick reference)

| ID  | Task                                   | Phase | Model  | Depends on |
|-----|----------------------------------------|-------|--------|------------|
| P0  | Foundation: skeleton, contracts, fakes | 0     | (main) | —          |
| T1  | config (load/schema/init)              | 1     | Sonnet | P0         |
| T2  | fs (paths/ignore)                      | 1     | Sonnet | P0         |
| T3  | git (repo/worktree/branch/status/remote)| 1    | Opus   | P0         |
| T4  | agents (registry/adapter/status)       | 1     | Sonnet | P0         |
| T5  | persistence (project_state/recovery)   | 1     | Sonnet | P0         |
| T6  | terminal (pty/session/shell)           | 1     | Opus   | P0         |
| T7  | app core (state/events/commands/modes) | 2     | Opus   | T1–T6      |
| T8  | tui (layout/render/input/palette)      | 3     | Sonnet | T7         |
| T9  | main.rs wiring + startup/recovery flow | 4     | Opus   | T7, T8     |
| T10 | integration test suite                 | 4     | Opus   | T9         |
| T11 | QA sweep + docs + manual-smoke prep    | 5     | (main) | all        |

Phases 0 → 5 are sequential. **Phase 1 (T1–T6) is a 6-way parallel fan-out.**

---

## 2. Global conventions (set in Phase 0, enforced everywhere)

- **Language/stack:** Rust 2021, `ratatui` + `crossterm` backend, `portable-pty`
  for PTYs, `serde`/`serde_json` for `state.json`, `toml` for `config.toml`,
  `thiserror` for typed errors, `tempfile` + `insta` (snapshot) for tests.
- **Git access:** shell out to the `git` binary via `std::process::Command`, fully
  behind the `GitExecutor` trait (SPECS §27: "Git command execution must be
  abstracted behind interfaces"). Do **not** use `libgit2`/`git2` — explicit git
  commands keep the §5 safety boundary auditable.
- **No async runtime.** Sync event loop. PTY output is read on a background thread
  that pushes to an `mpsc`/`crossbeam` channel consumed by the event loop. This
  keeps complexity down and matches a TUI's needs.
- **Naming:** product "FlightDeck", binary `flightdeck`, folder `.flightdeck/`,
  branch prefix `flightdeck/`. The string "Agent Orchestrator" must appear
  **nowhere** (SPECS §2). Add a CI/test grep guard in T11.
- **Safety boundary (SPECS §5):** no code path may stage, commit, amend, squash,
  rebase, or rewrite history, or create GitHub PRs. The `GitExecutor` trait must
  not even expose such methods. Enforce by construction.
- **Errors:** every fallible service returns `Result<T, FlightDeckError>`. UI never
  panics on user-reachable errors.
- **Quality bars (every task):** `cargo build` clean, `cargo clippy -- -D warnings`
  clean, `cargo fmt` applied, module unit tests green.

---

## 3. Phase 0 — Foundation, contracts & fakes (MAIN AGENT, not delegated)

**Why the main agent does this directly:** the contracts are the shared vocabulary
of every subagent brief. You must hold them in your own context to write accurate
briefs. Keep this phase tight.

### 3.1 Steps

1. `git init` the FlightDeck source repo (currently not a repo). Add a Rust
   `.gitignore` (`/target`).
2. `cargo init --name flightdeck` (binary crate) and add a `src/lib.rs` so logic is
   testable as a library with a thin `main.rs` binary on top.
3. Populate `Cargo.toml` with the **complete** dependency set (so no subagent ever
   edits it):
   ```toml
   [dependencies]
   ratatui = "..."
   crossterm = "..."
   portable-pty = "..."
   serde = { version = "...", features = ["derive"] }
   serde_json = "..."
   toml = "..."
   thiserror = "..."

   [dev-dependencies]
   tempfile = "..."
   insta = "..."
   ```
   Pin to current stable versions (check docs.rs at build time).
4. Create the **full module tree** matching SPECS §27, with `mod` declarations and
   `todo!()`-bodied public items so everything compiles:
   ```
   src/
     main.rs            (thin: calls flightdeck::run())
     lib.rs             (pub mod ... for every module below)
     contracts/         (NEW — shared types/traits/errors; main-agent-owned)
       mod.rs
       error.rs         (FlightDeckError)
       domain.rs        (TabState, ProjectState, AgentDef, status enums, ids)
       traits.rs        (GitExecutor, FileSystem, PtyBackend, Clock)
     testing/           (NEW — fakes; main-agent-owned)
       mod.rs           (FakeGit, FakeFs, FakePty, FakeClock + builders)
     app/    { state.rs, events.rs, commands.rs, modes.rs, mod.rs }
     tui/    { layout.rs, render.rs, input.rs, palette.rs, mod.rs }
     git/    { repo.rs, worktree.rs, branch.rs, status.rs, remote.rs, mod.rs }
     terminal/ { pty.rs, session.rs, shell.rs, mod.rs }
     agents/ { registry.rs, adapter.rs, status.rs, mod.rs }
     config/ { load.rs, schema.rs, init.rs, mod.rs }
     persistence/ { project_state.rs, recovery.rs, mod.rs }
     fs/     { paths.rs, ignore.rs, mod.rs }
   tests/
     integration/ { init.rs, worktree.rs, recovery.rs, push.rs, merge_preconditions.rs }
   ```
5. Define the contracts (signatures below are the **blueprint** — adjust names for
   ergonomics but keep the shape). The bodies stay `todo!()` until their owning
   task fills them.

### 3.2 Contract blueprint (`src/contracts/`)

```rust
// error.rs
#[derive(thiserror::Error, Debug)]
pub enum FlightDeckError {
    #[error("git error: {0}")] Git(String),
    #[error("io error: {0}")] Io(String),
    #[error("config error: {0}")] Config(String),
    #[error("state error: {0}")] State(String),
    #[error("agent command not found: {0}")] AgentMissing(String),
    #[error("operation refused: {0}")] Refused(String), // §5/§13/§15 guard rails
    #[error("{0}")] Other(String),
}
pub type Result<T> = std::result::Result<T, FlightDeckError>;

// domain.rs  (serde-serializable; mirror SPECS §9 exactly)
pub struct TabId(pub String);

pub enum ProcessState { NotStarted, Starting, Running, Stopped, Exited(i32), Failed, Lost }

pub enum InterpretedStatus {
    Starting, Running, WaitingForInput, NeedsAttention,
    Completed, Failed, Stopped, SessionLost, Recovered, Unknown,
}
pub enum ManualStatus { InProgress, Waiting, Blocked, Done } // override; None = cleared

pub struct AgentDef {              // from config.toml [agents.*]
    pub key: String, pub display_name: String,
    pub command: String, pub args: Vec<String>,
    pub status_patterns: StatusPatterns,
}
pub struct StatusPatterns { pub waiting: Vec<String>, pub completed: Vec<String>, pub error: Vec<String> }

pub struct TabState {              // SPECS §9 — store RELATIVE paths
    pub id: String, pub name: String, pub slug: String, pub agent: String,
    pub branch: String, pub worktree_path_relative: String,
    pub base_branch: String, pub base_commit_sha: String,
    pub created_at: String, pub attached_existing_branch: bool,
    pub recovered: bool, pub last_known_status: String, pub manual_status: Option<String>,
}
pub struct ProjectState {
    pub version: u32, pub project_root_relative: String,
    pub base_branch: String, pub tabs: Vec<TabState>,
}

// traits.rs  — the seams that make everything testable (SPECS §26)
pub trait GitExecutor {            // NO commit/amend/rebase/squash/PR methods (§5)
    fn repo_root(&self, cwd: &Path) -> Result<PathBuf>;
    fn current_branch(&self, cwd: &Path) -> Result<String>;
    fn is_dirty(&self, cwd: &Path) -> Result<bool>;
    fn branch_exists(&self, name: &str) -> Result<bool>;
    fn create_branch(&self, name: &str, from: &str) -> Result<()>;
    fn rev_parse(&self, refname: &str) -> Result<String>;            // SHA
    fn add_worktree(&self, path: &Path, branch: &str) -> Result<()>;
    fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>>;
    fn remove_worktree(&self, path: &Path) -> Result<()>;
    fn ahead_behind(&self, base: &str, branch: &str) -> Result<(u32, u32)>;
    fn upstream_of(&self, branch: &str) -> Result<Option<String>>;
    fn push(&self, remote: &str, branch: &str, cwd: &Path) -> Result<()>;
    fn remote_url(&self, remote: &str) -> Result<Option<String>>;
    fn merge_no_ff(&self, branch: &str, cwd: &Path) -> Result<MergeOutcome>; // §15 guarded
    // ...extend as tasks require, but never add history-rewriting ops.
}
pub trait FileSystem {             // abstract per SPECS §26
    fn exists(&self, p: &Path) -> bool;
    fn create_dir_all(&self, p: &Path) -> Result<()>;
    fn read_to_string(&self, p: &Path) -> Result<String>;
    fn write(&self, p: &Path, contents: &str) -> Result<()>;
    fn append_line(&self, p: &Path, line: &str) -> Result<()>;       // .gitignore append-only
    fn list_dir(&self, p: &Path) -> Result<Vec<PathBuf>>;
}
pub trait PtyBackend {             // wrap PTY behind a testable boundary (§26)
    fn spawn(&self, cmd: &str, args: &[String], cwd: &Path, size: PtySize)
        -> Result<Box<dyn PtySession>>;
}
pub trait PtySession: Send {
    fn write_input(&mut self, bytes: &[u8]) -> Result<()>;
    fn resize(&mut self, size: PtySize) -> Result<()>;
    fn try_read_output(&mut self) -> Result<Vec<u8>>;   // non-blocking drain
    fn send_ctrl_c(&mut self) -> Result<()>;            // §25
    fn process_state(&self) -> ProcessState;
    fn terminate_tree(&mut self) -> Result<()>;         // §25 force path
}
pub trait Clock { fn now_iso8601(&self) -> String; }    // deterministic in tests
```

### 3.3 Fakes (`src/testing/`)

Ship `FakeGit`, `FakeFs`, `FakePty`/`FakePtySession`, `FakeClock` implementing the
traits with scriptable behavior (e.g. `FakeGit::with_branches([...])`,
`set_dirty(true)`, record pushes, return canned `ahead_behind`). Every Phase 1+
subagent uses these for unit tests, so define them generously.

### 3.4 Phase 0 acceptance gate

- `cargo build` compiles the whole skeleton with `todo!()` bodies.
- `cargo clippy` runs (warnings about `todo!`/unused are fine here).
- Commit: `chore: foundation skeleton, contracts, and test fakes`.

---

## 4. Phase 1 — Service modules (PARALLEL FAN-OUT: T1–T6)

Spawn all six in one message. Each depends only on Phase 0 contracts/fakes, owns
disjoint files, and writes its own unit tests. Map each to the SPECS §26 test list.

### T1 — config — **Sonnet** — files: `src/config/{load.rs, schema.rs, init.rs}`
Spec: §6, §7, §8. Implement: parse/serialize `config.toml` into `AgentDef`s and
project/ui/git settings; create default config; first-run init that creates
`.flightdeck/`, `config.toml`, `state.json` placeholder, `worktrees/`. Reject
invalid config with clear errors; preserve human-editable structure.
Tests (§26 "Config" + "Initialization"): creates default config; loads config;
rejects invalid config; init creates each dir/file; does not duplicate work if
already present.

### T2 — fs — **Sonnet** — files: `src/fs/{paths.rs, ignore.rs}`
Spec: §6. Implement: relative↔absolute path helpers (state stores relative paths,
runtime computes absolute — §9); `.gitignore` **append-only** updater that adds
`.flightdeck/state.json` and `.flightdeck/worktrees/` only if missing, preserving
all existing contents and order, and reports whether it changed anything.
Tests (§26 "Initialization"): appends missing entries; does not duplicate existing
entries; does not rewrite/sort/remove unrelated lines; returns a "changed" notice.

### T3 — git — **Opus** — files: `src/git/{repo.rs, worktree.rs, branch.rs, status.rs, remote.rs}`
Spec: §5, §10–§15. Implement the real `GitExecutor` (shelling to `git`) **and** the
higher-level git workflow logic built on the trait: repo-root/base-branch detection;
dirty detection; slug→branch naming with `flightdeck/` enforcement; branch
create-vs-attach decision (refuse silent attach — §11); worktree create/reuse/recover/
remove-when-safe; existing-checked-out refusal (refuse if checked out elsewhere — §11);
ahead/behind + upstream; base-drift calc from stored base SHA (§12); GitHub remote URL
parsing → PR compare URL (§14); push planning incl. uncommitted-changes warning (§14);
local merge-back **precondition checks** and refusal paths (§13, §15). **Never** expose
or call commit/amend/rebase/squash/PR operations (§5).
Tests (§26 "Branch naming" + "Git workflow"): slug generation; prefix enforcement;
existing-branch detection; tab-rename-doesn't-rename-branch; dirty base/worktree
detection; worktree creation planning; attach behavior; existing-checked-out refusal;
push confirmation + uncommitted warning; merge precondition checks (incl. refusal when
base dirty); base-drift calc. Use `FakeGit` for logic; real-git paths are exercised in
T10 integration tests.

### T4 — agents — **Sonnet** — files: `src/agents/{registry.rs, adapter.rs, status.rs}`
Spec: §8, §16, §17, §24. Implement: build the agent registry from config; PATH
existence check for an agent's command (§16 — must run **before** any git mutation);
build the launch command/args from config; output→status classifier using the
config `status_patterns` (substring match → `WaitingForInput`/`Completed`/`Failed`),
combine `ProcessState` + interpreted + manual override with manual taking visual
priority but not hiding process state (§24). No initial prompt is ever passed (§17).
Tests (§26 "Agent handling"): detects missing command before git mutation; builds
command from config; does not pass initial prompts; classifies output patterns;
manual override applied correctly.

### T5 — persistence — **Sonnet** — files: `src/persistence/{project_state.rs, recovery.rs}`
Spec: §9, §10. Implement: load/save `state.json` (serde, version field, relative
paths); recovery that loads state, validates tabs, scans `.flightdeck/worktrees/`
(via `FileSystem` + `GitExecutor::list_worktrees`), reconstructs missing tabs, marks
them `recovered=true`, and **never** auto-relaunches agents (§10, §24). Provide the
recovered-tab action set as data (restart/open-shell/push/merge/close/remove-stale).
Tests (§26 "Recovery" + "App state" persistence bits): loads valid state; handles
missing state; handles stale state; scans worktrees; reconstructs tabs; marks
recovered; does not auto-restart; round-trips save/load.

### T6 — terminal — **Opus** — files: `src/terminal/{pty.rs, session.rs, shell.rs}`
Spec: §17, §19, §25. Implement the real `PtyBackend`/`PtySession` over
`portable-pty`: spawn primary agent PTY and child shell PTYs in a given worktree cwd;
background reader thread → channel; non-blocking output drain; write input; resize;
send Ctrl-C; detect process exit and failed start; force terminate process tree
(§25). Model a `Session` that owns one primary + N child terminals, tracks the
selected child, and supports create/switch/close child (§19) where children may
outlive the primary and are not persisted.
Tests (§26 "Terminal/session abstraction"): creates primary; creates child;
switches child; closes child; sends Ctrl-C; handles process exit; handles failed
process start. Use `FakePty` for the session-logic tests; gate any real-PTY test
behind a smoke test that runs a trivial command like `true`/`echo`.

**Integration checkpoint after Phase 1.** Build, test, clippy, fmt, commit
(`feat: service modules (config, fs, git, agents, persistence, terminal)`).

---

## 5. Phase 2 — Application core (T7 — Opus)

Files: `src/app/{state.rs, events.rs, commands.rs, modes.rs}`.
Depends on T1–T6 (consumes their public APIs through the contracts/services).
Spec: §3, §4, §18, §22, §23, §24, §25.

Implement the headless application core — **no terminal I/O here** (SPECS §27: the
TUI must not execute git/fs/pty; it dispatches commands into services):

- `AppState`: tabs (the §3 invariant 1 tab = 1 worktree = 1 branch = 1 primary
  agent), selected tab + selected child terminal, modes, persistent warnings
  (dirty-base → merge disabled, §13).
- `Command` enum covering every §22 palette action (New/Rename/Close Agent Tab,
  Push, Finish/Local Merge, Abandon Worktree, New/Close Child Terminal, Switch
  Agent/Child, Set Manual Status, Restart Agent, Open Shell, Show Git Status, Show
  Help, Quit) plus a `dispatch(cmd) -> Result<Effect>` reducer that calls services.
- New-tab flow (§4, §16, §17): validate agent command **before** mutating git →
  prompt name → slug → branch → create/attach → worktree → spawn primary → focus.
- Tab rename independent of branch/slug/worktree (§18).
- Two input modes (Terminal vs App) and the mode transitions (§23).
- Close-tab choreography (§25): Ctrl-C primary / Ctrl-C all / force / close-if-stopped
  / cancel, default = Ctrl-C primary, never auto-escalate.

Tests (§26 "App state" + the command/mode logic): create/rename/switch/close tab;
maintains selected tab & child; new-tab validation order (agent check precedes git
mutation); manual status override; close-tab option set; mode transitions. All via
fakes — fully headless.

Checkpoint: build/test/clippy/fmt/commit (`feat: application core (state, commands, modes)`).

---

## 6. Phase 3 — TUI (T8 — Sonnet)

Files: `src/tui/{layout.rs, render.rs, input.rs, palette.rs}`.
Depends on T7 (renders `AppState`, emits `Command`s). Spec: §19–§24.

Implement with Ratatui/crossterm:

- `layout.rs`: the §20 layout — left Agent-Tabs sidebar + main pane (child-terminal
  tab bar, active terminal viewport, status/action bar). Layout math as **pure
  functions** of (area, state) so they're unit-testable without a terminal (§26).
- `render.rs`: sidebar rows (name, agent, interpreted status, process state, dirty,
  ahead/behind, base drift, recovered/existing markers — §20/§24); git status panel
  (§21, no diff view); mode-aware status bar strings (§23).
- `input.rs`: key handling for both modes and all §23 shortcuts, translating keys →
  `Command`s; terminal-focus passthrough to the active PTY.
- `palette.rs`: command palette listing every §22 action (the dependable fallback,
  §22/§23).

Tests (§26 "TUI rendering"): snapshot-test render functions with `insta` where
practical; unit-test layout calculations independently from terminal I/O; unit-test
key→command mapping for both modes.

Checkpoint: build/test/clippy/fmt/commit (`feat: TUI (layout, render, input, palette)`).

---

## 7. Phase 4 — Wiring & integration tests

### T9 — main.rs + startup/recovery wiring — **Opus**
Files: `src/main.rs`, `src/lib.rs` `run()` entry, plus any glue in `app` that needs
the real services injected (you, the main agent, own `lib.rs`/`main.rs`; if `app`
glue is needed, delegate a tightly-scoped edit or do it yourself).
Spec: §4, §7, §10, §13. Wire the event loop: construct real `GitExecutor`/`FileSystem`/
`PtyBackend`/`Clock`, run first-run init (§7), load state + recover worktrees without
relaunching agents (§10), show dirty-base warning (§13), enter the Ratatui loop
draining PTY channels + crossterm events and dispatching commands. Clean teardown on
quit (no orphaned child processes — §25).
This phase is mostly integration glue and is hard to unit-test; rely on T10.

### T10 — integration test suite — **Opus**
Files: `tests/integration/{init.rs, worktree.rs, recovery.rs, push.rs, merge_preconditions.rs}`.
Spec: §26 "Integration Tests". Use **real temp git repos** (`tempfile` +
`std::process::Command` git), never real GitHub. Cover: initialize in fresh repo;
create branch + worktree; attach to existing branch; recover worktree from disk;
detect dirty base; detect dirty agent worktree; simulate push through a **local bare
repo acting as the remote** (no network); block local merge when base dirty; allow
local merge only when all §15 preconditions pass. Assert the §5 boundary: no commits
are ever created by FlightDeck code paths.

Checkpoint: build/test/clippy/fmt/commit (`feat: wiring + integration tests`).

---

## 8. Phase 5 — QA sweep & handoff (T11 — main agent)

1. Full suite: `cargo build --release`, `cargo test`, `cargo clippy -- -D warnings`,
   `cargo fmt --check`.
2. **Naming guard:** `grep -ri "agent orchestrator" .` over source/docs/config must
   return nothing (SPECS §2). Add this as a test or CI check.
3. **Safety-boundary guard:** grep the `git/` module to confirm no `commit`,
   `--amend`, `rebase`, `cherry-pick`, or PR-creation invocations exist (§5).
4. Write a short `README.md`: what FlightDeck is, how to run it, the §5 boundary, and
   the dev/test commands.
5. **Manual smoke test (requires you, the human user):** subagents cannot drive a
   real attached terminal. Run `cargo run` from inside a scratch git repo and verify
   the primary flows by hand: new tab → agent launches → child terminal → push
   confirmation → git status panel → recovery on restart. List this checklist in the
   README so it's repeatable.

---

## 9. Risks, caveats & mitigations

- **Contract drift between subagents.** Mitigated by the file-ownership rule (§0.3):
  only the main agent edits contracts; subagents report friction instead of
  patching. Re-brief affected tasks centrally.
- **PTY/terminal behavior is the trickiest area.** Hence T6 and T9 are Opus, and the
  `PtyBackend` trait keeps session *logic* testable with `FakePty`; only thin smoke
  tests touch a real PTY.
- **Final interactive verification can't be automated by subagents** (no real TTY).
  Handled by the Phase 5 human smoke test — flagged, not a blocker.
- **Real agent binaries may be absent.** Never required; PATH check is unit-tested
  with fakes and the binaries are mocked everywhere.
- **Dependency versions** may have moved past training data — the main agent must
  confirm current crate versions when writing `Cargo.toml` in Phase 0.

---

## 10. Execution summary for the main agent

1. **Phase 0 (you):** init repo, `Cargo.toml`, full skeleton, contracts, fakes →
   compiles → commit.
2. **Phase 1:** spawn **T1–T6 in parallel** (Sonnet: T1,T2,T4,T5; Opus: T3,T6) →
   integrate → commit.
3. **Phase 2:** delegate **T7** (Opus) → integrate → commit.
4. **Phase 3:** delegate **T8** (Sonnet) → integrate → commit.
5. **Phase 4:** delegate **T9** (Opus) and **T10** (Opus) → integrate → commit.
6. **Phase 5 (you):** QA sweep, guards, README, hand off the human smoke-test
   checklist.

Each delegated task uses the §0.6 brief template, owns disjoint files, tests against
the §3.3 fakes, and must pass the §0.6 acceptance bar before reporting done. After
every batch, run the §0.7 integration checkpoint and commit, so the long-running
session is always recoverable.
