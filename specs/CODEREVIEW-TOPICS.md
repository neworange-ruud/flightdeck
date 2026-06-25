# FlightDeck Code Review Topics

This document splits the codebase into small, connected review topics. The goal is not to review findings here, but to define review units that can be inspected independently without pulling in unrelated files.

Prefer reviewing one topic at a time. If a reviewer finds a topic still too large, split it further along the line ranges listed here.

## Review Rules

- Review the listed primary scope first.
- Use supporting tests only to understand expected behavior and coverage for that scope.
- Do not expand into neighboring topics unless a direct bug path crosses that boundary.
- For cross-cutting flows, review the named call path only, not the entire referenced file.
- Treat `src/lib.rs`, `src/app/state.rs`, and `src/tui/render.rs` as split files; do not review them as single topics.

## Project Map

- Rust CLI/TUI crate: `Cargo.toml`, `src/main.rs`, `src/lib.rs`.
- Headless app core: `src/app/*`.
- TUI rendering/input: `src/tui/*`.
- Git workflows: `src/git/*`.
- PTY/session model: `src/terminal/*`.
- Config, state, recovery, filesystem seams: `src/config/*`, `src/persistence/*`, `src/fs/*`, `src/contracts/*`.
- Agent registry/status/integrations: `src/agents/*`, `src/notify/*`.
- Test support and integration coverage: `src/testing/mod.rs`, `tests/*`.

## Topics

### T01 - Crate Entrypoint and CLI Dispatch

Primary scope:
- `src/main.rs:1-9`
- `src/lib.rs:68-160`
- `src/lib.rs:342-379`

Supporting scope:
- `README.md:16-67`

Review focus:
- Thin binary boundary, `--help`, `--version`, and subcommand dispatch.
- Error reporting and exit behavior.
- Terminal title save/restore helpers only as used by startup and teardown.

Keep out:
- Interactive event loop internals.
- Setup-status artifact templates.

### T02 - Service Traits and Safety Boundary

Primary scope:
- `src/contracts/traits.rs:1-122`
- `src/contracts/error.rs:1-45`
- `src/contracts/mod.rs`

Supporting scope:
- `tests/guards.rs:53-98`
- `README.md:68-86`
- `specs/SPECS.md:108-138`

Review focus:
- Whether the trait surface preserves the git ownership boundary.
- Whether user-reachable errors can be represented and surfaced safely.
- Whether trait contracts are specific enough for fakes and production implementations.

Keep out:
- Concrete Git, FS, and PTY implementations.

### T03 - Domain Types: Status, Config, State, Git Values

Primary scope:
- `src/contracts/domain.rs:1-393`

Supporting scope:
- Serialization users in `src/config/load.rs`, `src/persistence/project_state.rs`, and `src/app/state.rs` only as needed.

Review focus:
- Persisted schema shape and serde defaults.
- Status enum conversion helpers and manual-status labels.
- Config defaults represented in domain structs.
- `TabState` fields and relative-path assumptions.

Keep out:
- Validation rules and business logic.

### T04 - Real Filesystem and Clock Implementations

Primary scope:
- `src/contracts/real.rs:1-117`

Supporting scope:
- `src/contracts/traits.rs:56-71`
- Unit tests in `src/contracts/real.rs:101-117`

Review focus:
- `RealFs` behavior against the `FileSystem` trait.
- Append-only semantics for `append_line`.
- UTC timestamp formatting and monotonic-ish millis expectation.

Keep out:
- Fake filesystem behavior.
- Higher-level init/gitignore logic.

### T05 - Test Fakes: FakeFs

Primary scope:
- `src/testing/mod.rs:15-145`

Supporting scope:
- Unit tests in `src/testing/mod.rs:683-end` relevant to `FakeFs`.

Review focus:
- In-memory filesystem correctness for files, dirs, parent marking, reads/writes, appends, and sorted directory listing.

Keep out:
- FakeGit and FakePty.

### T06 - Test Fakes: FakeGit

Primary scope:
- `src/testing/mod.rs:147-465`

Supporting scope:
- Unit tests in `src/testing/mod.rs:683-end` relevant to `FakeGit`.
- Git workflow unit tests that use recorded fake calls.

Review focus:
- Scriptable GitExecutor behavior and recordings.
- Whether fake semantics are close enough to production for safety-sensitive unit tests.

Keep out:
- Production `GitCli`.

### T07 - Test Fakes: FakePty and FakeClock

Primary scope:
- `src/testing/mod.rs:466-682`

Supporting scope:
- Unit tests in terminal/session and app/state that depend on queued sessions or clock advancement.

Review focus:
- Fake PTY spawn queue, output/input recording, process state transitions, termination behavior.
- Deterministic clock behavior for status and notification tests.

Keep out:
- Production portable-pty implementation.

### T08 - Default Config Schema and Validation

Primary scope:
- `src/config/schema.rs:1-191`
- Config-related domain types in `src/contracts/domain.rs:161-311`

Supporting scope:
- `specs/SPECS.md:209-260` if needed.

Review focus:
- Default agent definitions and status patterns.
- Validation of required agents/default command fields.
- Notification config defaults.

Keep out:
- TOML parsing and file IO.

### T09 - Config Load, Parse, Serialize

Primary scope:
- `src/config/load.rs:1-131`

Supporting scope:
- `src/config/schema.rs:79-105`

Review focus:
- TOML parse/serialize round trip.
- Agent key population from map keys.
- Validation call after parse.
- Error mapping.

Keep out:
- First-run file creation.

### T10 - First-Run Initialization Artifacts

Primary scope:
- `src/config/init.rs:1-182`
- `src/persistence/project_state.rs:6-14`

Supporting scope:
- `tests/integration/init.rs:59-172`

Review focus:
- Creation of `.flightdeck/`, `config.toml`, `state.json`, `worktrees/`.
- Idempotence and non-overwrite behavior.
- State-file shape on first run.

Keep out:
- `.gitignore` mutation.
- Startup orchestration in `src/lib.rs`.

### T11 - Gitignore Updater

Primary scope:
- `src/fs/ignore.rs:1-237`

Supporting scope:
- `tests/integration/init.rs:104-155`
- `src/agents/setup.rs:48-67` only for the status-entry caller.

Review focus:
- Append-only behavior.
- Missing-file behavior.
- Duplicate prevention with trimmed matching.
- Core entries versus opt-in status entry.

Keep out:
- First-run init and setup-status template contents.

### T12 - Path Helpers and State Path Assumptions

Primary scope:
- `src/fs/paths.rs:1-136`

Supporting scope:
- `src/app/state.rs` calls to `to_relative`, `to_absolute`, and `worktree_path` only.

Review focus:
- Relative path storage invariants.
- Outside-root handling.
- Worktree path construction.

Keep out:
- Recovery scan logic.

### T13 - Startup Orchestration Before TUI Launch

Primary scope:
- `src/lib.rs:263-340`

Supporting scope:
- `src/config/init.rs`
- `src/config/load.rs`
- `src/fs/ignore.rs`
- `src/persistence/recovery.rs:42-140`

Review focus:
- Base-branch detection flow.
- First-run init order.
- Config fallback behavior.
- Gitignore notice.
- Recovery invocation and dirty-base warning.

Keep out:
- Interactive event loop.
- `AppState` command handling.

### T14 - Production GitCli Command Wrapper

Primary scope:
- `src/git/repo.rs:1-230`

Supporting scope:
- `src/contracts/traits.rs:13-54`
- `tests/guards.rs:53-98`

Review focus:
- Concrete git commands and cwd/root selection.
- Error handling for non-zero status.
- Dirty/status/upstream/remote handling.
- Merge command result mapping.
- Absence of prohibited git operations.

Keep out:
- Pure branch/worktree/remote planning helpers.

### T15 - Worktree List Parsing and Base Branch Detection

Primary scope:
- `src/git/repo.rs:231-354`

Supporting scope:
- `src/git/worktree.rs:19-35`

Review focus:
- `git worktree list --porcelain` parser.
- Detached worktree handling.
- Configured-base fallback rules.

Keep out:
- Worktree creation/removal commands.

### T16 - Branch Naming and Create-vs-Attach Decision

Primary scope:
- `src/git/branch.rs:1-150`

Supporting scope:
- `src/app/state.rs:689-701`
- `tests/integration/worktree.rs:58-134`

Review focus:
- Slug generation edge cases.
- Prefix enforcement.
- Branch existence decision and user-surfaced attach semantics.

Keep out:
- Worktree materialization.

### T17 - Worktree Planning, Creation, and Safe Removal

Primary scope:
- `src/git/worktree.rs:1-205`

Supporting scope:
- `src/app/state.rs:703-721`, `src/app/state.rs:975-1028`
- `tests/integration/worktree.rs:58-184`

Review focus:
- Managed-root detection.
- Refusal when branch is checked out elsewhere.
- Branch creation order versus worktree add.
- Dirty worktree removal refusal and forced removal boundary.

Keep out:
- App-level prompt/confirmation UI.

### T18 - Git Status Collection and Change Counting

Primary scope:
- `src/git/status.rs:1-113`

Supporting scope:
- `src/tui/render.rs:770-878`
- Background refresh path in `src/lib.rs:688-741`

Review focus:
- Porcelain parsing and change categorization.
- Ahead/behind and upstream behavior.
- Base drift calculation.
- Status object fields consumed by UI.

Keep out:
- Merge preconditions.

### T19 - Merge Preconditions and Merge-Back Helper

Primary scope:
- `src/git/status.rs:115-203`

Supporting scope:
- `src/app/state.rs:918-996`
- `tests/integration/merge_preconditions.rs:76-265`

Review focus:
- Dirty-base and dirty-agent refusal paths.
- Branch existence checks.
- Re-check-before-merge behavior.
- Conflict reporting without auto-resolution.

Keep out:
- Prompt confirmation UI.

### T20 - Remote Parsing, Push Planning, and PR URLs

Primary scope:
- `src/git/remote.rs:1-193`

Supporting scope:
- `src/app/state.rs:889-916`
- `tests/integration/push.rs:69-185`

Review focus:
- GitHub remote URL parsing.
- Dirty worktree push warnings.
- Push delegation and PR compare URL generation.

Keep out:
- Network or GitHub API behavior; app only prints compare URLs.

### T21 - Agent Registry

Primary scope:
- `src/agents/registry.rs:1-158`

Supporting scope:
- `src/config/schema.rs:11-77`
- `src/lib.rs:1196-1218` for agent picker usage.

Review focus:
- Config-to-registry cloning/key population.
- Default-agent lookup.
- Stable ordering for UI selection.

Keep out:
- Agent command validation and status classification.

### T22 - Agent Command Validation and LaunchSpec

Primary scope:
- `src/agents/adapter.rs:1-225`

Supporting scope:
- `src/app/state.rs:676-688`, `src/app/state.rs:794-803`, `src/app/state.rs:1128-1162`

Review focus:
- Command lookup rules for PATH and direct paths.
- Validation before git mutation.
- No initial prompt included in launched agent args.

Keep out:
- PTY spawn implementation.

### T23 - Agent Output Status Classification

Primary scope:
- `src/agents/status.rs:1-350`

Supporting scope:
- `src/app/state.rs:450-548`
- Status domain types in `src/contracts/domain.rs:15-159`

Review focus:
- Pattern precedence.
- Activity-based working/idle heuristic.
- Sticky signals and manual override combination.

Keep out:
- OS notification delivery.
- Status integration templates.

### T24 - Optional Status Integration Artifact Generation

Primary scope:
- `src/agents/setup.rs:1-326`

Supporting scope:
- CLI subcommand in `src/lib.rs:162-199`
- `src/fs/ignore.rs:13-33`

Review focus:
- Files written by `setup-status`.
- Idempotence.
- Hook/plugin keyword compatibility with `status_keyword_to_interpreted`.
- Safety of generated shell/JS snippets.

Keep out:
- Runtime polling of status files.

### T25 - OS Notification Delivery

Primary scope:
- `src/notify/mod.rs:1-127`

Supporting scope:
- Notification domain/config in `src/contracts/domain.rs:111-127`, `src/contracts/domain.rs:261-294`
- Setup-notifications CLI in `src/lib.rs:201-260`

Review focus:
- Non-blocking best-effort delivery.
- macOS command construction and AppleScript escaping.
- Non-macOS no-op behavior.

Keep out:
- Notification edge detection in `AppState`.

### T26 - Notification Edge Detection and Status File Polling

Primary scope:
- `src/app/state.rs:210-303`
- `src/app/state.rs:475-548`

Supporting scope:
- `src/agents/status.rs:38-76`
- `src/notify/mod.rs` only as delivery target.

Review focus:
- Status-file keyword mapping and unchanged-content behavior.
- Startup grace window.
- Armed-edge notification logic and category toggles.

Keep out:
- Platform notifier implementation.

### T27 - State File Load and Save

Primary scope:
- `src/persistence/project_state.rs:1-127`

Supporting scope:
- `src/app/state.rs:560-590`, `src/lib.rs:1757-1762`

Review focus:
- JSON load/save error mapping.
- Default state shape.
- Persistence call expectations.

Keep out:
- Recovery reconstruction.

### T28 - Recovery Scan and Stale Entries

Primary scope:
- `src/persistence/recovery.rs:1-374`

Supporting scope:
- `tests/integration/recovery.rs:56-136`
- Startup call in `src/lib.rs:298-310`

Review focus:
- Stored-tab validation against FS and git worktree list.
- On-disk worktree reconstruction.
- Stale-entry reporting without deletion.
- No auto-relaunch behavior.

Keep out:
- `AppState::resume_agents`.

### T29 - AppState Construction, Modes, Selection Helpers, Persistence

Primary scope:
- `src/app/modes.rs:1-11`
- `src/app/state.rs:305-448`
- `src/app/state.rs:551-599`
- `src/app/state.rs:1221-1237`

Supporting scope:
- AppState unit tests in `src/app/state.rs:1240-end` relevant to construction, mode, selection, persistence.

Review focus:
- Initial selection and recovered-tab runtime setup.
- Mode transitions and split-view flag.
- Selected-tab helper behavior.
- Runtime-to-persisted state conversion.
- Selection clamping after tab removal.

Keep out:
- Individual command handlers.

### T30 - Command and Effect Type Surface

Primary scope:
- `src/app/commands.rs:1-230`

Supporting scope:
- `src/tui/palette.rs:23-109`
- Dispatch match in `src/app/state.rs:604-640`

Review focus:
- Whether command payloads and effects match product workflows.
- Safety-related effect separation for warnings/refusals/confirmations.
- Close action defaults.

Keep out:
- Concrete command implementations.

### T31 - New Agent Tab: App Core Flow

Primary scope:
- `src/app/state.rs:642-827`

Supporting scope:
- `src/git/branch.rs`, `src/git/worktree.rs`, `src/agents/adapter.rs` only through direct calls.
- Unit tests in `src/app/state.rs` related to new-tab creation.

Review focus:
- Validation-before-git-mutation ordering.
- Slug/branch/worktree path derivation.
- Placeholder `Creating` tab behavior.
- Finalize/fail cleanup and persistence.

Keep out:
- Event-loop background worker mechanics.

### T32 - New Agent Tab: Event Loop Background Worker

Primary scope:
- `src/lib.rs:507-514`
- `src/lib.rs:605-685`
- `src/lib.rs:1196-1218`
- `src/lib.rs:1349-1362`

Supporting scope:
- `src/app/state.rs:664-827`

Review focus:
- Async creation handoff from prompt to `pending_jobs`.
- Worker thread, lock, outcome channel, finalize/fail behavior.
- UI responsiveness while worktree creation runs.

Keep out:
- Branch/worktree planning helper correctness.

### T33 - App Core: Rename, Switch, Manual Status

Primary scope:
- `src/app/state.rs:829-838`
- `src/app/state.rs:1058-1126`

Supporting scope:
- `src/app/commands.rs:100-104`, `src/app/commands.rs:135-140`
- Input/palette mapping only for corresponding commands.

Review focus:
- Rename should not mutate branch/slug/worktree metadata.
- Agent tab and child terminal selector behavior.
- Manual status persistence and display interaction.

Keep out:
- Prompt UI for collecting rename/status input.

### T34 - App Core: Close Tab and Process Handling

Primary scope:
- `src/app/state.rs:840-887`
- `src/app/commands.rs:26-78`
- `src/terminal/session.rs:362-405`

Supporting scope:
- Prompt handling in `src/lib.rs:1403-1419`

Review focus:
- No auto-escalation to force terminate.
- Ctrl-C primary/all behavior.
- If-all-stopped refusal.
- Runtime tab removal and persistence.

Keep out:
- Abandon worktree removal semantics.

### T35 - App Core: Push Branch Command

Primary scope:
- `src/app/state.rs:889-916`
- `src/app/commands.rs:80-87`, `src/app/commands.rs:112-117`, `src/app/commands.rs:175-177`

Supporting scope:
- `src/git/remote.rs:39-83`
- Prompt handling in `src/lib.rs:1420-1438`

Review focus:
- Dirty worktree warning and confirm/cancel flow.
- Remote selection from config.
- Compare URL effect versus generic success message.

Keep out:
- Remote parser internals beyond returned values.

### T36 - App Core: Finish / Local Merge Command

Primary scope:
- `src/app/state.rs:918-996`
- `src/app/commands.rs:118-122`, `src/app/commands.rs:181-192`

Supporting scope:
- `src/git/status.rs:115-203`
- Prompt handling in `src/lib.rs:1447-1454`

Review focus:
- Dirty-base warning persistence.
- Technical precondition check and explicit user confirmation.
- Session termination and worktree removal after successful merge.
- Cleanup failure behavior after merge already landed.

Keep out:
- Git merge helper internals except as direct dependency.

### T37 - App Core: Abandon Worktree Command

Primary scope:
- `src/app/state.rs:998-1029`
- `src/app/commands.rs:123-130`, `src/app/commands.rs:178-180`

Supporting scope:
- `src/git/worktree.rs:60-78`
- Prompt handling in `src/lib.rs:1439-1446`

Review focus:
- Dirty worktree confirmation boundary.
- Forced removal only after confirmation.
- Session teardown and state persistence.

Keep out:
- Close-tab process handling without worktree removal.

### T38 - App Core: Child Terminals, Restart, Resume, Status Command

Primary scope:
- `src/app/state.rs:1031-1056`
- `src/app/state.rs:1128-1219`
- `src/terminal/shell.rs:1-32`

Supporting scope:
- `src/terminal/session.rs:246-360`

Review focus:
- Child shell spawn in selected worktree.
- Child close selection behavior as used by app core.
- Primary restart validation/spawn.
- Resume-agents best-effort behavior.
- Show git status effect.

Keep out:
- Session internals and PTY implementation.

### T39 - Production PTY Backend

Primary scope:
- `src/terminal/pty.rs:1-211`

Supporting scope:
- `src/contracts/traits.rs:73-99`

Review focus:
- `portable-pty` spawn setup, cwd, args, size mapping.
- Reader thread and non-blocking output buffer.
- Input write, resize, process_state, Ctrl-C, terminate behavior.

Keep out:
- Higher-level session tab model.

### T40 - Terminal Session Model: Primary and Children

Primary scope:
- `src/terminal/session.rs:212-406`

Supporting scope:
- Unit tests in `src/terminal/session.rs:408-612`
- App core callers in `src/app/state.rs:1031-1056`, `src/app/state.rs:1128-1162`

Review focus:
- Primary versus child ownership.
- Selection/focus of active terminal.
- Child close index fixup.
- Ctrl-C and termination semantics.
- `all_stopped` definition.

Keep out:
- VT100 rendering and text selection.

### T41 - Terminal Selection and Scrollback Extraction

Primary scope:
- `src/tui/selection.rs:1-215`
- `src/terminal/session.rs:20-210`

Supporting scope:
- Mouse handling in `src/lib.rs:803-879`, `src/lib.rs:889-934`
- Render selection highlight in `src/tui/render.rs:576-645`

Review focus:
- Rows-from-bottom coordinate model.
- Selection range math and clamping.
- Selection extraction from visible and scrolled content.
- Scroll behavior and selection clearing on resize.

Keep out:
- Clipboard command implementation.

### T42 - Key Mapping and PTY Key Encoding

Primary scope:
- `src/tui/input.rs:1-670`
- `src/app/modes.rs:1-11`

Supporting scope:
- `src/lib.rs:1026-1086` for how `KeyAction` is consumed.

Review focus:
- Terminal versus App mode behavior.
- Global shortcuts in both modes.
- Ctrl-V paste interception.
- ANSI/VT key byte encoding.

Keep out:
- Prompt and palette key handling.

### T43 - Command Palette Model

Primary scope:
- `src/tui/palette.rs:1-366`

Supporting scope:
- `src/lib.rs:1513-1590` for confirmed action handling.

Review focus:
- Completeness of command entries.
- Filtering and selection wrap behavior.
- Which entries dispatch directly versus require prompts.

Keep out:
- Palette overlay rendering.

### T44 - Interactive Prompt State Machine

Primary scope:
- `src/lib.rs:381-484`
- `src/lib.rs:1088-1511`
- `src/lib.rs:1513-1590`

Supporting scope:
- `src/app/commands.rs`
- Unit tests in `src/lib.rs:1834-end` related to prompts and effects.

Review focus:
- Prompt capture priority and cancellation.
- New/rename text entry.
- Agent picker behavior.
- Manual status, close, push, abandon, and merge confirmation prompts.
- Effect-to-overlay mapping consistency.

Keep out:
- App command implementation details.

### T45 - Main Event Loop Tick and Background Status Refresh

Primary scope:
- `src/lib.rs:486-618`
- `src/lib.rs:688-741`

Supporting scope:
- `src/tui/render.rs:41-44`
- `src/git/status.rs:83-113`

Review focus:
- Per-tick ordering: PTY drain, create outcomes, status results, status-file polling, notifications, render, input.
- Background git status refresh and in-flight guard.
- Render loop non-blocking expectations.

Keep out:
- Individual input handlers.
- Worktree creation worker details covered in T32.

### T46 - PTY Drain, Input Write, Paste, Resize, Teardown

Primary scope:
- `src/lib.rs:1592-1770`

Supporting scope:
- `src/tui/clipboard.rs:25-44`
- `src/terminal/session.rs`

Review focus:
- PTY output drain into VT parser and status ingestion.
- Active terminal input write behavior.
- Clipboard image path paste fallback.
- Session resizing and split-view size sync.
- Persist-on-quit and terminate-all-sessions teardown.

Keep out:
- Platform clipboard internals.

### T47 - Mouse Hit Testing and Local/Forwarded Mouse Handling

Primary scope:
- `src/tui/render.rs:67-174`
- `src/lib.rs:754-1024`

Supporting scope:
- `src/tui/selection.rs`
- `src/terminal/session.rs:64-75`

Review focus:
- Click target calculation for sidebar and child tabs.
- Split-view hit testing.
- Local selection versus forwarded mouse-aware TUI events.
- Wheel scroll forwarding versus local scrollback.
- Mouse report encoding.

Keep out:
- Rendering of selected cells.

### T48 - Layout Math

Primary scope:
- `src/tui/layout.rs:1-460`

Supporting scope:
- `src/tui/render.rs:188-230`
- `src/lib.rs:743-751`, `src/lib.rs:1698-1751`

Review focus:
- Main layout rect computation.
- Header/sidebar/main/status geometry.
- Split-view region and column calculations.
- Overlay centering.

Keep out:
- Actual drawing styles.

### T49 - Rendering: Header, Sidebar, and Status Indicators

Primary scope:
- `src/tui/render.rs:188-478`

Supporting scope:
- `src/app/state.rs:181-207`
- `src/git/status.rs:69-113`

Review focus:
- Header fallback by width.
- Sidebar tab block structure.
- Creating-tab spinner.
- Status label/color collapse.
- Git indicator line behavior with missing cache.

Keep out:
- Terminal screen rendering and overlays.

### T50 - Rendering: Child Tabs, Terminal Viewport, VT Cells

Primary scope:
- `src/tui/render.rs:480-655`

Supporting scope:
- `src/terminal/session.rs:54-75`, `src/terminal/session.rs:101-209`
- `src/tui/selection.rs`

Review focus:
- Active terminal tab rendering.
- Empty/no-tab/creating states.
- VT100 cell-to-ratatui style conversion.
- Cursor positioning and selection highlight.

Keep out:
- Split view.

### T51 - Rendering: Split View

Primary scope:
- `src/tui/render.rs:656-764`
- `src/tui/layout.rs:129-203`
- `src/lib.rs:1698-1751`

Supporting scope:
- Click handling in `src/tui/render.rs:101-114`

Review focus:
- Column/header rendering.
- Active cursor behavior in split view.
- Separator placement.
- Agreement between layout, rendering, hit testing, and PTY sizing.

Keep out:
- Normal child-tab view.

### T52 - Rendering: Git Info Bar and Git Status Overlay

Primary scope:
- `src/tui/render.rs:766-1046`

Supporting scope:
- `src/git/status.rs:69-113`
- App command in `src/app/state.rs:1205-1219`

Review focus:
- Info bar fallback behavior when cache is missing.
- Change counts, upstream, base drift, and base branch display.
- Git status overlay content and no-diff boundary.

Keep out:
- Git status collection logic.

### T53 - Rendering: Help, Palette, Message, Snapshots

Primary scope:
- `src/tui/render.rs:1048-1220`
- Overlay-related tests in `src/tui/render.rs:1227-end`.

Supporting scope:
- `src/tui/palette.rs`
- `src/lib.rs:1138-1186`

Review focus:
- Overlay layering and clearing expectations.
- Help/keybinding text consistency with input mapping.
- Palette render behavior.
- Snapshot helper stability.

Keep out:
- Palette data model.

### T54 - Clipboard Text Copy and Image Paste Helpers

Primary scope:
- `src/tui/clipboard.rs:1-242`

Supporting scope:
- Paste caller in `src/lib.rs:1650-1672`
- Selection copy caller in `src/lib.rs:855-867`

Review focus:
- Platform command fallback behavior.
- OSC 52 encoding.
- Clipboard image extraction and temp path generation.
- Silent failure and fallback expectations.

Keep out:
- PTY write behavior after path generation.

### T55 - Integration Test Harness: Init, Worktree, Recovery

Primary scope:
- `tests/integration.rs:1-16`
- `tests/integration/init.rs:1-172`
- `tests/integration/worktree.rs:1-184`
- `tests/integration/recovery.rs:1-136`

Supporting scope:
- Modules under test as referenced by imports.

Review focus:
- Hermetic temp git setup.
- Real FS/Git coverage for first-run init, worktree operations, and recovery.
- macOS path canonicalization handling.

Keep out:
- Push and merge integration tests.

### T56 - Integration Test Harness: Push and Merge Safety

Primary scope:
- `tests/integration/push.rs:1-185`
- `tests/integration/merge_preconditions.rs:1-265`

Supporting scope:
- `src/git/remote.rs`
- `src/git/status.rs:115-203`
- `src/git/worktree.rs`

Review focus:
- Local bare remote setup with no network.
- Push planning and PR URL tests.
- Merge success/conflict tests.
- No-FlightDeck-created-commits guarantee.

Keep out:
- Unit tests for pure helpers.

### T57 - Guard Tests for Product Invariants

Primary scope:
- `tests/guards.rs:1-98`

Supporting scope:
- `README.md:68-86`
- `specs/SPECS.md:23-58`, `specs/SPECS.md:108-138`

Review focus:
- Placeholder-name guard.
- Forbidden git subcommand scan.
- Whether exceptions such as `worktree remove --force` are tightly scoped.

Keep out:
- Runtime GitExecutor implementation.

### T58 - Release and Distribution Automation

Primary scope:
- `scripts/release:1-86`
- `dist-workspace.toml:1-24`
- `.github/workflows/release.yml:1-343`

Supporting scope:
- `Cargo.toml:1-38`
- `README.md:16-36`

Review focus:
- Release version validation and clean-worktree checks.
- Quality gates run before tagging.
- cargo-dist config and generated CI permissions/artifact flow.
- Homebrew publishing target.

Keep out:
- Product runtime code.

### T59 - Documentation Consistency with Product Behavior

Primary scope:
- `README.md:1-332`
- `specs/SPECS.md:1-1039`
- `specs/PLAN.md:1-548`

Supporting scope:
- `Cargo.toml:1-20`
- User-facing help in `src/lib.rs:342-359`

Review focus:
- Public feature claims versus implemented commands and UI.
- Safety boundary wording.
- Keyboard model and screen layout consistency.
- Setup-status/setup-notifications docs versus implementation.

Keep out:
- Code correctness beyond checking documented behavior exists.

## Suggested Review Order

1. T02, T03, T57: establish contracts and non-negotiable safety invariants.
2. T14-T20: review the Git layer before app commands that call it.
3. T08-T13, T27-T28: review config, init, persistence, and recovery.
4. T21-T26: review agent/status/notification behavior.
5. T29-T38: review headless app command behavior.
6. T39-T47: review PTY, event loop, input, prompts, mouse, and clipboard plumbing.
7. T48-T53: review TUI layout and rendering.
8. T55-T56: review integration coverage after the behavior topics.
9. T58-T59: review release automation and docs.

## Large File Split Index

`src/lib.rs`:
- T01: public run entry, CLI dispatch, title helpers.
- T13: startup orchestration.
- T32: new-tab background worker handoff.
- T44: prompt and palette state machine.
- T45: event loop tick and status refresh.
- T46: PTY drain, paste, resize, teardown.
- T47: mouse handling.

`src/app/state.rs`:
- T26: status file polling and notification edge detection.
- T29: construction, modes, selection, persistence.
- T31: new agent tab app-core flow.
- T33: rename, switch, manual status.
- T34: close tab.
- T35: push.
- T36: finish/local merge.
- T37: abandon.
- T38: child terminals, restart, resume, git status command.

`src/tui/render.rs`:
- T47: hit testing.
- T49: header/sidebar/status indicators.
- T50: child tabs and VT terminal viewport.
- T51: split view.
- T52: git info/status overlay.
- T53: help, palette, message overlays, snapshots.
