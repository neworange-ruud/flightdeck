# Isolated Mode — Design

Status: implemented. This design shipped across the ISOLATED_MODE_PLAN task
series (Tasks 1-12); see specs/ISOLATED_MODE_PLAN.md for the implementation
record and SPECS §32 for the canonical, as-built description.
Date: 2026-08-25
Reserves SPECS §32.

## 1. Purpose

`flightdeck --isolated` (`-I`) launches a throwaway FlightDeck: one fresh agent
session in the current working directory, nothing continued from a previous run,
nothing left behind on disk. It exists for testing FlightDeck itself and for
poking at a repository without accreting state.

A normal startup keeps its behavior: no worktree changes, no new refusals, and
no new UI. One thing is not preserved: the agent status hooks a normal run
generates changed shape as part of this series (§8) — every non-containerized
run's hooks now carry an absolute, single-quoted status-file path instead of
the old cwd-relative one. Isolated mode itself still adds only a run-time
switch; it introduces no new configuration and no new persisted state.

## 2. Definition

An isolated run is defined by four properties:

1. **No continuation.** Nothing is read from a previous run and nothing is
   replayed — not `state.json`, not the workspace file, not a captured resume
   command.
2. **No FlightDeck-initiated writes.** FlightDeck writes nothing on its own.
   Explicit user actions that exist to write (saving in the configuration
   manager, pairing a phone) still write; the rule constrains FlightDeck, not
   the process. One documented exception applies to containerized runs (§8.2).
3. **No worktrees.** No branch is created, no `git worktree add` is run — no git
   mutation of any kind happens at startup.
4. **One session, in the cwd.** Exactly one tab, running the default agent with
   the repository root as its working directory.

## 3. The flag

`--isolated` and `-I` are recognized in `run()` alongside the existing
`--version`/`--help` scans, before the subcommand match.

Combining the flag with a subcommand (`flightdeck -I doctor`) is an error with a
clear message, not a silent ignore.

It is represented as a runtime-only `pub isolated: bool` on `AppState`, next to
`split_view`. The command dispatcher, the renderer and the palette all already
reach `AppState`, so no new plumbing is needed.

**It is deliberately not a configuration setting.** It is a per-run decision, and
a persisted file that silently suppressed persistence would be a trap. This is
the one intended exception to `flightdeck-config-conventions`.

## 4. Startup

Reads the effective configuration (global + project) if it exists. Writes
nothing.

| Step | Normal | Isolated |
|---|---|---|
| `initialize()` first-run config write | on demand | **never** — runs on defaults + whatever config exists |
| `state.json` read + `recover()` | yes | **skipped** |
| `resume_agents()` | yes | **skipped** |
| workspace file read | yes | **skipped** — only the cwd project is open |
| `update::start_check` | per config | **`enabled = false`** — no network call, no cache write |
| `ui.auto_continue` | per config | **forced `false`** for the run |
| initial tabs | recovered | **exactly one**, created fresh |

A git repository is still required, and base-branch detection is unchanged.

`auto_continue` is forced off so that even an in-session **Restart Agent** starts
a fresh session instead of replaying a captured resume command. Without this,
"no continuation" would hold only until the first restart.

The update check is disabled because it writes a cache file and makes a network
call, both of which an isolated run should not do.

## 5. The single tab

Created through the existing `begin_base_agent_tab`:

- agent: `ui.default_agent`
- working directory: the repository root
- `runs_on_base = true`, `needs_create = false` — so `materialize_worktree` is a
  no-op and **not one git mutation occurs**
- `resume_args` empty: a fresh session

**Branch label fix.** A base tab used to label itself with the *base* branch,
which was wrong whenever HEAD was on something else. The fix landed in the
shared `begin_base_agent_tab` (`src/app/state.rs:1111-1115`), so it is not
isolated-mode-specific: every base tab, in every run, is now labelled with
the branch actually checked out (falling back to the base branch on a
detached HEAD or a git failure — see SPECS §32). This is the same fix the
CHANGELOG's `Bug fixes` entry announces.

## 6. Blocked actions

**As built, this is not a dispatcher-level refusal.** Open/Close/Switch Project
and New Agent Session Tab are workspace-level actions — they never reach
`AppState::dispatch` at all, so the dispatcher has no occasion to refuse them
even in principle. The guard instead lives inside the flow functions
themselves: `start_new_tab_flow`, `start_open_project_flow`,
`start_close_project_flow`, and a `switch_project` helper (shared by the
keybinding, mouse, and command-palette-next/prev paths). Each checks
`state.isolated` first and, if set, calls `ui.message(ISOLATED_REFUSAL)` and
returns — one shared constant, so the refusal reads identically wherever the
user meets it, and one guard per flow covers every entry point (keybinding,
mouse, palette) at once. The command palette additionally hides the blocked
entries outright, which is presentation only; the flow-function guard is the
real gate.

Blocked:

- Open Project, Close Project, Next Project, Previous Project
- the project tab bar's New button
- New Agent Session Tab

Already refused for free, because the tab is `runs_on_base`:

- Finish / Local Merge, Rebase Worktree, Abandon Worktree

Explicitly still available: Push Branch, Pull Base, Open Configuration, Pair
Phone, child terminals, additional agents in the same tab, Open Shell, Show Git
Status, split view, rename, restart, close tab.

## 7. Teardown

- `persist_quietly` is skipped for every project
- `save_workspace` is skipped
- all sessions are terminated, exactly as in a normal run
- the temporary status directory (§8) is removed

## 8. Status plumbing redirect

Before this series, spawning an agent always wrote its status plumbing into
the working tree: `.flightdeck/runtime/status/…` (a Claude plugin directory,
an OpenCode plugin) plus a seeded `.flightdeck/agent-status`, and the
generated hooks appended to `.flightdeck/agent-status` **relative to the
agent's working directory**, guarded with `[ -d .flightdeck ]` in case the
hook fired somewhere unrelated.

As built, every non-containerized run — isolated or not — now builds its
status file path from an explicit status root and templates that **absolute**
path directly into the hook body; the relative-path form and the
`[ -d .flightdeck ]` guard are both gone (the guard is unnecessary once the
directory is FlightDeck-created and absolute — see §8.1 for the exact
emitted form). Isolated mode redirects that status root to a temporary
directory outside the repository, so the status chips and OS notifications
keep working while the project directory stays untouched; normal mode's
status root is still the worktree, unchanged.

`prepare_status_launch` takes a status-root parameter. Normal mode passes the
worktree — the *root* is byte-identical to before — and isolated mode passes
a temp directory (`flightdeck-isolated-<pid>/`), removed on teardown.

What became path-aware:

- `agent_status_file(root)` — derived from the status root, not the worktree
- the Claude plugin hooks (`claude_plugin_hooks`) — templated with the
  absolute status-file path, guard dropped
- `codex_hook_override` — the same, in its `--config hooks.…` overrides
- the OpenCode runtime plugin JS — the same
- `remote::bridge::agent_question_path` — so remote question-answering keeps
  working

`RuntimeTab::status_file` is already an absolute `PathBuf`, so polling itself
needs no change; only the two sites that construct it do.

### 8.1 Risk: shell and JSON escaping across platforms

The generated hooks are POSIX-shell one-liners with no guard and a
single-quoted absolute path, e.g. the "idle" hook body
(`src/agents/setup.rs`, `claude_plugin_hooks`/`codex_hook_override`, via
`shell_quote`):

```
printf 'idle\n' >> '/abs/status/root/.flightdeck/agent-status'; exit 0
```

embedded in JSON (Claude) or a TOML `--config hooks.…` override (Codex).
Templating an absolute path into them means two layers of escaping, and a
Windows path brings backslashes into both. This is the most likely thing in
this design to break. It is squarely `flightdeck-cross-platform-parity`
territory and must be tested on Windows, not reasoned about. **As of this
writing it has not been** — the implementation work happened on Linux, and
the escaping is argued correct by construction (the `serde_json`/`toml`
serializers own the escaping, not hand-rolled string work) but has not been
exercised on an actual Windows machine or CI runner. This stays open until
someone does.

### 8.2 Exception: containerized runs

`prepare_status_launch`'s `containerized` mode maps the status runtime to
`/workspace/…` precisely because it lives inside the bind-mounted worktree. A
temp directory outside the worktree is not mounted, so the redirect cannot work
there.

**Resolution:** when containers are enabled, an isolated run keeps the
in-worktree status path, and this exception is documented in the help text. A
containerized run already writes into the mounted worktree by its nature, so
this concedes little. Adding a second bind mount for the temp directory is the
alternative if the exception proves annoying in practice.

## 9. Visibility

Nothing persists and several actions are absent, so the mode must be
unmistakable:

- an `ISOLATED` badge in the status bar
- a line in the Help overlay stating what is off (no persistence, no
  continuation, no other projects, no new session tabs)

Without this, a forgotten `-I` looks exactly like data loss.

## 10. Verification

The strong tests are at `startup` level with `FakeFs`
(`isolated_startup_writes_nothing_under_the_repo`,
`isolated_startup_ignores_state_json_on_disk`,
`isolated_startup_still_reads_project_config`):

- the fake filesystem receives **zero writes** during an isolated startup
- the effective `auto_continue` is `false`
- `state.json` on disk is present but ignored — nothing is recovered from it

`startup` itself creates no tab (the single tab is created afterwards, by
`start_isolated_session`), so "exactly one tab exists, and it is
`runs_on_base`" is asserted one level up, in
`isolated_run_creates_exactly_one_base_tab` (`src/lib.rs`), not in a
`startup`-level test.

The workspace file is not read by `startup` at all — normal mode reads it in
`run()`, and an isolated run skips that read by passing `ws_path = None`
before `startup` is ever called (`src/lib.rs:207-215`). No test exercises
this the way the `startup`-level tests exercise `state.json`, because `run()`
has no test seam (the same limitation noted for teardown below); it is
covered by construction, not by an automated assertion.

Then:

- flow-function tests: `start_new_tab_flow`, `start_open_project_flow`,
  `start_close_project_flow`, and `switch_project` each leave `ui.message` set
  to `ISOLATED_REFUSAL` and take no other action when `state.isolated` is set
  — this is the real gate (§6); there is no dispatcher-level refusal to test,
  since these actions never reach `AppState::dispatch`
- palette test: the blocked entries are absent from the palette in isolated mode
  and present in a normal one
- `prepare_status_launch` with a root outside the worktree writes nothing under
  the worktree, and the absolute status path appears in the generated hooks
- a normal-mode regression test that the status plumbing paths are unchanged

**Open item: teardown is verified by reading, not by test.** The
`persist_quietly` skip and the workspace-file skip are real (§7), but
`workspace_state_path()` reads `HOME` directly and is not injected through
`Env`/`Services`, so a test cannot swap in a fake home directory to assert the
file was never written. Confirming this currently means reading the guard at
the call site, not running a test that would fail if the guard were removed.
Closing this gap needs `workspace_state_path()` (or its caller) to take an
injectable home-directory seam; until then, treat teardown as covered by
construction and manual verification, not by an automated test.

## 11. Non-goals

- Isolated mode is not a sandbox. The agent can still read and write the
  repository and reach the network; only FlightDeck's own bookkeeping is
  suppressed.
- Not a config setting, and not persisted anywhere (§3).
- No new isolated-only UI beyond the badge and the help line.
- Running outside a git repository stays unsupported.
