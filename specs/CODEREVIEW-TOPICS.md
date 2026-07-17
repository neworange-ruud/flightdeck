# FlightDeck Code Review Topics

This document splits the current codebase into small, connected review topics. It is a review map, not a code review: no findings are recorded here.

The goal is independent, thorough review. Prefer too many small topics over fewer large ones.

## Review Rules

- Review the listed primary scope first.
- Use supporting scope only to understand expectations, call paths, or test coverage.
- Do not expand into neighboring topics unless a concrete bug path crosses that boundary.
- For large files, review only the named ranges and call paths.
- Treat `src/lib.rs`, `src/app/state.rs`, `src/tui/render.rs`, `src/testing/mod.rs`, and `src/terminal/session.rs` as split files; do not review them as single topics.

## Current Project Map

- Rust CLI/TUI crate: `Cargo.toml`, `src/main.rs`, `src/lib.rs`.
- Headless app core: `src/app/*`.
- TUI rendering/input: `src/tui/*`.
- Git workflows: `src/git/*`.
- PTY/session model: `src/terminal/*`.
- Container execution: `src/runtime/*`, `containers/README.md`, `specs/CONTAINER_SUPPORT_PLAN.md`.
- Self-update/update notices: `src/update.rs`, `Cargo.toml` feature gates.
- Config, persistence, filesystem seams: `src/config/*`, `src/persistence/*`, `src/fs/*`, `src/contracts/*`.
- Agent registry/status/integrations: `src/agents/*`, `src/notify/*`.
- Test support and integration coverage: `src/testing/mod.rs`, `tests/*`, `examples/keylog.rs`.

## Topics

### T01 - Crate Manifest, Features, and Dependencies

Primary scope:
- `Cargo.toml:1-66`

Supporting scope:
- `dist-workspace.toml:1-24`
- `src/lib.rs:22-48`
- `src/update.rs:1-18`

Review focus:
- Default `self-update` feature and Windows target-gating.
- Optional `axoupdater`/`tokio` dependency graph.
- Cross-platform package metadata and release profile.

Keep out:
- Runtime behavior of the updater.

### T02 - Binary Entrypoint

Primary scope:
- `src/main.rs:1-9`

Supporting scope:
- `src/lib.rs:98-228`

Review focus:
- Thin binary wrapper and process exit behavior.
- Error display boundary.

Keep out:
- CLI subcommand implementation.

### T03 - Public Run Entry and CLI Dispatch

Primary scope:
- `src/lib.rs:98-228`
- `src/lib.rs:581-610`

Supporting scope:
- `README.md:15-127`

Review focus:
- `--help`, `--version`, and subcommand routing.
- Terminal setup/teardown around the main event loop.
- Bracketed paste and keyboard enhancement setup.
- Service construction including `PodmanCli`.

Keep out:
- Individual subcommand internals.
- Event loop internals.

### T04 - Setup Status and Notification Subcommands

Primary scope:
- `src/lib.rs:230-320`

Supporting scope:
- `src/agents/setup.rs:1-326`
- `src/config/load.rs:1-131`
- `src/config/init.rs:1-198`

Review focus:
- `setup-status` artifact write flow.
- `setup-notifications` config mutation and first-run config creation.
- Repo-root discovery and user-facing guidance.

Keep out:
- Runtime notification edge detection and delivery.

### T05 - Setup Update Subcommand

Primary scope:
- `src/lib.rs:321-366`

Supporting scope:
- `src/update.rs:111-253`
- `src/config/schema.rs:63-78`

Review focus:
- `update.check` config creation/mutation.
- Opt-in semantics and user-facing text.
- Repo-root discovery and missing-config behavior.

Keep out:
- Actual update check network/cache logic.

### T06 - Image Build CLI Subcommand

Primary scope:
- `src/lib.rs:368-428`

Supporting scope:
- `src/runtime/image.rs:158-232`
- `src/runtime/podman.rs:58-119`
- `src/config/schema.rs:111-145`

Review focus:
- `flightdeck image build [agent]` parsing.
- Config/agent validation.
- Podman availability check.
- Image tag resolution and build call.

Keep out:
- Containerfile generation details.

### T07 - Doctor CLI Subcommand

Primary scope:
- `src/lib.rs:430-490`

Supporting scope:
- `src/runtime/podman.rs:18-72`
- `src/runtime/image.rs:46-52`

Review focus:
- Reporting behavior for disabled/enabled containers.
- Podman readiness guidance.
- Per-agent image readiness checks.

Keep out:
- Image build implementation.

### T08 - Startup Orchestration

Primary scope:
- `src/lib.rs:492-580`

Supporting scope:
- `src/config/init.rs:1-198`
- `src/fs/ignore.rs:1-237`
- `src/persistence/recovery.rs:42-144`
- `src/git/repo.rs:338-351`

Review focus:
- First-run init sequence.
- Config load/default fallback.
- `.gitignore` notice.
- State load/recovery without relaunch.
- Dirty-base warning setup.

Keep out:
- Event loop and app command dispatch.

### T09 - Service Traits and Safety Boundary

Primary scope:
- `src/contracts/traits.rs:1-199`
- `src/contracts/error.rs:1-45`

Supporting scope:
- `tests/guards.rs:53-122`
- `README.md:129-149`

Review focus:
- Git ownership boundary and sanctioned rebase/pull-base carve-outs.
- `FileSystem` removal API.
- `Clock::now_unix_secs` for update checks.
- `ContainerRuntime` control-plane seam.
- User-reachable error representation.

Keep out:
- Concrete implementations.

### T10 - Domain Types: Status, Config, State, Git, Container

Primary scope:
- `src/contracts/domain.rs:1-569`

Supporting scope:
- `src/config/schema.rs:1-145`
- `src/persistence/project_state.rs:1-129`
- `src/app/state.rs:847-863`

Review focus:
- Status and manual status models.
- Update and container config defaults/serde aliases.
- Persisted `TabState` container fields.
- `RebaseOutcome` and `ContainerState` value types.

Keep out:
- Validation and command behavior.

### T11 - Real Filesystem and Clock

Primary scope:
- `src/contracts/real.rs:1-163`

Supporting scope:
- `src/contracts/traits.rs:86-160`
- `src/git/worktree.rs:71-103`

Review focus:
- `RealFs` implementation including recursive removal.
- Windows retry behavior for directory deletion.
- ISO timestamp and Unix seconds clock behavior.

Keep out:
- Fake filesystem behavior.

### T12 - Test Fake: FakeFs

Primary scope:
- `src/testing/mod.rs:19-156`

Supporting scope:
- Tests in `src/testing/mod.rs:952-1033` relevant to FakeFs.

Review focus:
- In-memory file/dir behavior.
- Append and removal semantics.
- Directory listing shape used by recovery.

Keep out:
- Git/container fakes.

### T13 - Test Fake: FakeGit

Primary scope:
- `src/testing/mod.rs:158-549`

Supporting scope:
- `src/git/status.rs:282-594`
- `src/git/worktree.rs:128-316`

Review focus:
- Scripted branches, worktrees, dirty status, upstreams, remotes.
- Rebase and pull-base recordings/outcomes.
- Prune/remove error simulation.

Keep out:
- Production `GitCli`.

### T14 - Test Fake: FakePty

Primary scope:
- `src/testing/mod.rs:550-719`

Supporting scope:
- `src/terminal/session.rs:495-859`
- `src/lib.rs:2577-2600`

Review focus:
- Spawn queue and failure behavior.
- Output/input recording.
- Process state, Ctrl-C, termination behavior.

Keep out:
- Production portable-pty.

### T15 - Test Fake: FakeContainerRuntime

Primary scope:
- `src/testing/mod.rs:720-888`

Supporting scope:
- `src/app/state.rs:3347-3599`
- `src/runtime/image.rs:319-367`

Review focus:
- Image existence/labels/build recording.
- Container state and start/remove recording.
- Label discovery and host UID behavior.

Keep out:
- Production Podman implementation.

### T16 - Test Fake: FakeClock

Primary scope:
- `src/testing/mod.rs:890-950`

Supporting scope:
- `src/update.rs:169-190`
- Notification tests in `src/app/state.rs` that use `now_millis`.

Review focus:
- Fixed ISO time.
- Millis and Unix seconds controls.
- Suitability for idle/notification/update tests.

Keep out:
- Real clock formatting.

### T17 - Default Config and Container Validation

Primary scope:
- `src/config/schema.rs:1-278`
- Config domain types in `src/contracts/domain.rs:161-450`

Supporting scope:
- `containers/README.md`
- `specs/CONTAINER_SUPPORT_PLAN.md`

Review focus:
- Default agents and status patterns.
- `update` and `containers` defaults.
- Container runtime, Containerfile/customization, and port validation.
- Disabled-container tolerance.

Keep out:
- TOML parsing and file IO.

### T18 - Config Load, Parse, Serialize

Primary scope:
- `src/config/load.rs:1-131`

Supporting scope:
- `src/config/schema.rs:81-145`

Review focus:
- TOML parse/serialize round trip.
- Agent key population.
- Validation call after parse.
- Error mapping.

Keep out:
- First-run init.

### T19 - First-Run Initialization

Primary scope:
- `src/config/init.rs:1-198`
- `src/persistence/project_state.rs:6-15`

Supporting scope:
- `tests/integration/init.rs:59-172`

Review focus:
- Creation of `.flightdeck/`, `config.toml`, `state.json`, `worktrees/`.
- Idempotence and non-overwrite behavior.
- Default state/config shape.

Keep out:
- Gitignore mutation.

### T20 - Gitignore Updater

Primary scope:
- `src/fs/ignore.rs:1-237`

Supporting scope:
- `tests/integration/init.rs:104-155`
- `src/agents/setup.rs:48-67`

Review focus:
- Append-only behavior.
- Missing-file behavior.
- Duplicate prevention.
- Core entries versus opt-in status entry.

Keep out:
- Init and setup-status template contents.

### T21 - Path Helpers

Primary scope:
- `src/fs/paths.rs:1-144`

Supporting scope:
- `src/app/state.rs:819-844`
- `src/persistence/recovery.rs:42-144`

Review focus:
- Relative path storage invariants.
- Absolute path resolution.
- Worktree path construction.

Keep out:
- Recovery policy.

### T22 - State File Load and Save

Primary scope:
- `src/persistence/project_state.rs:1-129`

Supporting scope:
- `src/app/state.rs:667-697`
- `src/lib.rs:2306-2311`

Review focus:
- JSON load/save error mapping.
- Default state shape.
- Container fields in persisted tab state.

Keep out:
- Recovery reconstruction.

### T23 - Recovery Scan and Stale Entries

Primary scope:
- `src/persistence/recovery.rs:1-378`

Supporting scope:
- `tests/integration/recovery.rs:56-138`
- Startup call in `src/lib.rs:537-549`

Review focus:
- Stored-tab validation against FS and git worktrees.
- On-disk worktree reconstruction.
- Stale-entry reporting without deletion.
- Container field defaults on recovered tabs.
- No auto-relaunch behavior.

Keep out:
- `AppState::resume_agents`.

### T24 - Production GitCli: Basic Commands

Primary scope:
- `src/git/repo.rs:1-218`

Supporting scope:
- `src/contracts/traits.rs:15-66`
- `tests/guards.rs:53-122`

Review focus:
- Git command construction and cwd/root selection.
- Dirty/status/branch/worktree/upstream/remote behavior.
- Worktree prune.
- Error mapping.

Keep out:
- Merge/rebase/pull-base behavior.

### T25 - Production GitCli: Merge, Rebase, Pull Base

Primary scope:
- `src/git/repo.rs:219-289`

Supporting scope:
- `src/contracts/traits.rs:63-84`
- `tests/guards.rs:53-122`
- `src/git/status.rs:115-280`

Review focus:
- `merge --no-ff` result/conflict mapping.
- `rebase` and `pull --rebase` sanctioned carve-outs.
- Abort-on-failure behavior.
- No auto conflict resolution.

Keep out:
- App confirmation prompts.

### T26 - Worktree List Parsing and Base Branch Detection

Primary scope:
- `src/git/repo.rs:291-414`

Supporting scope:
- `src/git/worktree.rs:17-41`
- `tests/integration/util.rs:1-31`

Review focus:
- `git worktree list --porcelain` parsing.
- Detached worktree handling.
- Configured-base fallback rules.
- Canonical path considerations in tests.

Keep out:
- Worktree creation/removal commands.

### T27 - Branch Naming and Attach Decision

Primary scope:
- `src/git/branch.rs:1-150`

Supporting scope:
- `src/app/state.rs:804-816`
- `tests/integration/worktree.rs:58-134`

Review focus:
- Slug generation.
- Prefix enforcement.
- Create-vs-attach branch decision.
- Rename must not rename branch.

Keep out:
- Worktree materialization.

### T28 - Worktree Planning and Creation

Primary scope:
- `src/git/worktree.rs:1-58`

Supporting scope:
- `src/app/state.rs:818-886`
- `tests/integration/worktree.rs:58-184`

Review focus:
- Managed-root detection.
- Refusal when branch checked out elsewhere.
- Branch creation before worktree add.

Keep out:
- Removal/orphan cleanup.

### T29 - Worktree Removal and Orphan Cleanup

Primary scope:
- `src/git/worktree.rs:60-316`

Supporting scope:
- `src/contracts/real.rs:56-94`
- `src/app/state.rs:1223-1262`

Review focus:
- Dirty refusal without force.
- Forced removal boundary.
- Unregistered/orphan worktree fallback.
- Windows lock-error fallback and pruning.

Keep out:
- App prompt confirmation flow.

### T30 - Git Status Collection and Change Counting

Primary scope:
- `src/git/status.rs:1-113`

Supporting scope:
- `src/tui/render.rs:795-907`
- Background refresh in `src/lib.rs:972-1025`

Review focus:
- Porcelain parsing and change categorization.
- Ahead/behind and upstream behavior.
- Base drift calculation.
- Status fields consumed by UI.

Keep out:
- Merge/rebase preconditions.

### T31 - Merge Preconditions and Merge-Back Helper

Primary scope:
- `src/git/status.rs:115-203`

Supporting scope:
- `src/app/state.rs:1045-1124`
- `tests/integration/merge_preconditions.rs:76-265`

Review focus:
- Dirty-base and dirty-agent refusal.
- Branch existence checks.
- Re-check-before-merge.
- Conflict reporting without auto-resolution.

Keep out:
- UI confirmation prompt.

### T32 - Worktree Rebase Preconditions and Helper

Primary scope:
- `src/git/status.rs:205-280`

Supporting scope:
- `src/app/state.rs:1126-1183`
- `src/git/repo.rs:241-264`

Review focus:
- Clean-agent-worktree requirement.
- Branch existence checks.
- Re-check-before-rebase.
- Aborted-conflict messaging.

Keep out:
- Pull-base behavior.

### T33 - Remote Parsing, Push Planning, and PR URLs

Primary scope:
- `src/git/remote.rs:1-193`

Supporting scope:
- `src/app/state.rs:1016-1043`
- `tests/integration/push.rs:69-185`

Review focus:
- GitHub remote URL parsing.
- Dirty worktree push warning.
- Push delegation.
- Compare URL generation.

Keep out:
- GitHub API/PR creation.

### T34 - Agent Registry

Primary scope:
- `src/agents/registry.rs:1-158`

Supporting scope:
- `src/config/schema.rs:9-79`
- `src/lib.rs:1654-1676`

Review focus:
- Config-to-registry conversion.
- Default-agent lookup.
- Stable ordering for agent picker.

Keep out:
- Command validation and status classification.

### T35 - Agent Command Validation and LaunchSpec

Primary scope:
- `src/agents/adapter.rs:1-276`

Supporting scope:
- `src/app/state.rs:186-200`
- `src/app/state.rs:1394-1454`

Review focus:
- Command lookup rules for PATH/direct paths across platforms.
- Validation before git mutation.
- Local launch spec construction.
- No initial prompt injection.

Keep out:
- Container launch argv.

### T36 - Agent Output Status Classification

Primary scope:
- `src/agents/status.rs:1-350`

Supporting scope:
- `src/app/state.rs:282-308`
- `src/app/state.rs:557-656`

Review focus:
- Pattern precedence.
- Activity-based working/idle heuristic.
- Sticky signals.
- Manual status display combination.

Keep out:
- OS notification delivery.

### T37 - Optional Status Integration Artifacts

Primary scope:
- `src/agents/setup.rs:1-326`

Supporting scope:
- `src/lib.rs:230-260`
- `src/fs/ignore.rs:13-33`

Review focus:
- Generated Claude/Codex/OpenCode artifacts.
- Idempotence and gitignore entry.
- Keyword compatibility with runtime poller.
- Safety of generated shell/JS snippets.

Keep out:
- Runtime polling logic.

### T38 - OS Notification Delivery

Primary scope:
- `src/notify/mod.rs:1-154`

Supporting scope:
- `src/contracts/domain.rs:111-127`
- `src/app/state.rs:340-404`, `src/app/state.rs:618-656`

Review focus:
- macOS terminal-notifier/osascript fallback.
- Linux `notify-send` delivery.
- Non-blocking best-effort behavior.
- AppleScript escaping.

Keep out:
- Notification edge detection.

### T39 - AppState Construction, Modes, Selection, Persistence

Primary scope:
- `src/app/modes.rs:1-11`
- `src/app/state.rs:406-552`
- `src/app/state.rs:658-705`
- `src/app/state.rs:1665-1680`

Supporting scope:
- Unit tests in `src/app/state.rs:1683-end` relevant to construction, mode, persistence, and selection.

Review focus:
- Initial runtime state.
- Mode and split-view flags.
- Update notice field.
- Runtime-to-persisted state conversion.
- Selection clamping after removal.

Keep out:
- Individual commands.

### T40 - Status File Polling and Notification Edge Detection

Primary scope:
- `src/app/state.rs:311-404`
- `src/app/state.rs:557-656`

Supporting scope:
- `src/agents/status.rs:48-76`
- `src/notify/mod.rs`

Review focus:
- Status file keyword mapping.
- Unchanged-content handling.
- Startup grace window.
- Armed edge detection and category toggles.

Keep out:
- Platform notification backend.

### T41 - Command and Effect Type Surface

Primary scope:
- `src/app/commands.rs:1-264`

Supporting scope:
- `src/tui/palette.rs:41-147`
- Dispatch match in `src/app/state.rs:711-750`

Review focus:
- Command payloads for push, merge, rebase, pull base, copy env, abandon.
- Confirmation effects and safety separation.
- Close action defaults.

Keep out:
- Concrete command implementation.

### T42 - New Agent Tab: Planning and Placeholder

Primary scope:
- `src/app/state.rs:752-886`

Supporting scope:
- `src/git/branch.rs`
- `src/git/worktree.rs:1-58`
- `src/agents/adapter.rs`

Review focus:
- Validation-before-git-mutation.
- Slug/branch/worktree derivation.
- Placeholder `Creating` tab behavior.
- Container-mode validation difference.

Keep out:
- Primary spawn/container launch finalization.

### T43 - New Agent Tab: Finalize and Failure Cleanup

Primary scope:
- `src/app/state.rs:888-953`
- `src/app/state.rs:1394-1454`

Supporting scope:
- Worker outcome path in `src/lib.rs:919-970`

Review focus:
- Finalize primary spawn and transition to `Ready`.
- Container start-before-attach behavior.
- Persisted container metadata.
- Placeholder cleanup on materialize/spawn failure.

Keep out:
- ContainerSpec assembly details.

### T44 - App Core: Rename, Switch, Manual Status

Primary scope:
- `src/app/state.rs:955-964`
- `src/app/state.rs:1324-1392`

Supporting scope:
- `src/app/commands.rs:100-111`, `src/app/commands.rs:150-155`
- Relevant input/palette mappings.

Review focus:
- Rename only affects display name.
- Agent tab and child terminal selector behavior.
- Manual status persistence.

Keep out:
- Prompt UI.

### T45 - App Core: Close Tab and Container Teardown

Primary scope:
- `src/app/state.rs:966-1014`
- `src/app/state.rs:1543-1552`
- `src/app/commands.rs:26-78`

Supporting scope:
- `src/terminal/session.rs:420-463`
- Prompt handling in `src/lib.rs:1888-1904`

Review focus:
- No auto-escalation to force terminate.
- Ctrl-C primary/all and if-stopped behavior.
- Container removal on close.
- Runtime tab removal and persistence.

Keep out:
- Abandon worktree removal.

### T46 - App Core: Push Branch

Primary scope:
- `src/app/state.rs:1016-1043`
- `src/app/commands.rs:80-87`, `src/app/commands.rs:112-117`, `src/app/commands.rs:190-192`

Supporting scope:
- `src/git/remote.rs:39-83`
- Prompt handling in `src/lib.rs:1905-1923`

Review focus:
- Dirty warning and confirm/cancel flow.
- Remote selection.
- PR URL versus success message.

Keep out:
- Remote parser internals beyond returned values.

### T47 - App Core: Finish / Local Merge

Primary scope:
- `src/app/state.rs:1045-1124`
- `src/app/commands.rs:118-122`, `src/app/commands.rs:200-211`

Supporting scope:
- `src/git/status.rs:115-203`
- Prompt handling in `src/lib.rs:1932-1939`

Review focus:
- Dirty-base warning.
- Explicit confirmation.
- Session/container teardown and worktree removal after merge.
- Cleanup failure after merge landed.

Keep out:
- Git merge helper internals except direct dependency.

### T48 - App Core: Rebase Worktree

Primary scope:
- `src/app/state.rs:1126-1183`
- `src/app/commands.rs:123-130`, `src/app/commands.rs:212-226`

Supporting scope:
- `src/git/status.rs:205-280`
- Prompt handling in `src/lib.rs:1940-1947`
- User prompt text in `src/lib.rs:1734-1753`

Review focus:
- Rebase preconditions and explicit history-rewrite confirmation.
- Base-drift prompt context.
- Stored base SHA advance on success.
- Force-push warning message.

Keep out:
- Production Git rebase implementation.

### T49 - App Core: Pull Base

Primary scope:
- `src/app/state.rs:1185-1221`
- `src/app/commands.rs:131-135`

Supporting scope:
- `src/git/repo.rs:266-288`
- `src/tui/input.rs:128-136`

Review focus:
- Base branch checked out in repo root.
- Dirty-base refusal.
- `git pull --rebase` outcome handling.
- Global command behavior independent of selected tab.

Keep out:
- Agent worktree rebase.

### T50 - App Core: Abandon Worktree

Primary scope:
- `src/app/state.rs:1223-1262`
- `src/app/commands.rs:138-145`, `src/app/commands.rs:193-199`

Supporting scope:
- `src/git/worktree.rs:60-126`
- Prompt handling in `src/lib.rs:1924-1931`

Review focus:
- Always-confirm behavior, dirty flag in prompt.
- Session/container teardown before removal.
- Forced removal only after confirmation.
- State persistence and selection fixup.

Keep out:
- Worktree orphan cleanup internals.

### T51 - App Core: Copy Env File

Primary scope:
- `src/app/state.rs:1264-1284`
- `src/app/commands.rs:136-137`

Supporting scope:
- `src/tui/palette.rs:68-72`

Review focus:
- `.env.local` preference over `.env`.
- Base-to-worktree copy behavior.
- Missing-file refusal.

Keep out:
- General filesystem abstraction.

### T52 - App Core: Child Terminals, Restart, Resume, Status

Primary scope:
- `src/app/state.rs:1286-1322`
- `src/app/state.rs:1554-1663`

Supporting scope:
- `src/terminal/session.rs:260-463`
- `src/runtime/container.rs:97-114`

Review focus:
- Child shell local versus `podman exec` behavior.
- Child close selection behavior.
- Restart fresh versus resume attach.
- Resume best-effort behavior.
- Git status effect.

Keep out:
- ContainerSpec assembly.

### T53 - Container Spawn Spec Assembly in AppState

Primary scope:
- `src/app/state.rs:114-200`
- `src/app/state.rs:1394-1552`

Supporting scope:
- `src/runtime/container.rs:16-152`
- `src/runtime/name.rs:1-88`
- `src/runtime/image.rs:46-52`, `src/runtime/image.rs:226-232`

Review focus:
- Local versus container launch decision.
- Reattach to running containers.
- Missing-image refusal.
- Default auth mounts/env and `HOME` behavior.
- Platform mount flags and host UID.
- Container teardown.

Keep out:
- Pure argv builders and guardrails.

### T54 - Runtime Value Types and Naming

Primary scope:
- `src/runtime/mod.rs:1-28`
- `src/runtime/spec.rs:1-53`
- `src/runtime/name.rs:1-88`

Supporting scope:
- `src/app/state.rs:1394-1552`

Review focus:
- ContainerSpec fields.
- Auth mount representation.
- Container name sanitization.
- Repo hash and standard labels.

Keep out:
- Podman argv construction.

### T55 - Podman Run/Attach/Exec Arg Builders

Primary scope:
- `src/runtime/container.rs:1-310`

Supporting scope:
- `src/runtime/spec.rs:21-53`
- `src/app/state.rs:1394-1552`

Review focus:
- Detached run and attach model.
- Workspace bind mount at `/workspace`.
- Security posture args and resource limits.
- Loopback-only ports.
- Env and auth mounts as discrete argv elements.
- Image then agent command tail.

Keep out:
- Guardrail enforcement logic.

### T56 - Container Security Guardrails

Primary scope:
- `src/runtime/guards.rs:1-191`

Supporting scope:
- `src/runtime/container.rs:16-95`
- `src/app/state.rs:1443-1447`

Review focus:
- Rejection of privileged/env-host/runtime socket/home mounts.
- Loopback-only publish rule.
- `--flag value` and `--flag=value` parsing.
- Home mount canonicalization.

Keep out:
- Config validation.

### T57 - Container Image Tagging and Containerfile Generation

Primary scope:
- `src/runtime/image.rs:1-157`

Supporting scope:
- `src/config/schema.rs:111-145`
- `containers/README.md`

Review focus:
- Default trusted base image.
- Built-in install recipes.
- Project/base image tag shapes.
- Customization hash inputs.
- Generated Containerfile modes.

Keep out:
- Build-if-needed control flow.

### T58 - Container Image Ensure/Build Flow

Primary scope:
- `src/runtime/image.rs:158-424`

Supporting scope:
- CLI caller in `src/lib.rs:368-428`
- `src/contracts/traits.rs:162-199`

Review focus:
- Explicit image bypass.
- Generated/explicit Containerfile handling.
- Setup script body hashing.
- Build label staleness check.
- Missing image error text.

Keep out:
- Podman production shell-out.

### T59 - Production Podman Runtime

Primary scope:
- `src/runtime/podman.rs:1-209`

Supporting scope:
- `src/contracts/traits.rs:162-199`
- `src/lib.rs:430-490`

Review focus:
- `podman` availability and install guidance.
- Image inspect/build behavior.
- Detached start, state inspect, remove, label list.
- Host UID lookup.
- Error handling for missing binary versus not-ready runtime.

Keep out:
- Pure arg builders and guardrails.

### T60 - Self-Update Command

Primary scope:
- `src/update.rs:1-109`

Supporting scope:
- `Cargo.toml:21-57`
- Fallback module in `src/lib.rs:22-48`

Review focus:
- Install receipt eligibility check.
- Package-manager deferral guidance.
- `run_sync` result handling.
- No-op behavior for unsupported builds.

Keep out:
- Background update notice.

### T61 - Background Update Notice

Primary scope:
- `src/update.rs:111-358`

Supporting scope:
- `src/lib.rs:777-787`, `src/lib.rs:811-814`
- `src/tui/render.rs:913-988`
- `src/contracts/domain.rs:296-308`

Review focus:
- Once-a-day cache logic.
- Cache path selection.
- Background thread/network best effort.
- Cached immediate notice.
- Status bar hint behavior.

Keep out:
- Self-replacing update flow.

### T62 - Production PTY: Command Resolution and Spawn

Primary scope:
- `src/terminal/pty.rs:1-202`

Supporting scope:
- `src/contracts/traits.rs:106-132`

Review focus:
- Windows `.cmd`/`.bat` resolution through `cmd.exe /d /c`.
- Portable-pty open/spawn/cwd/args/size.
- Reader thread and output buffer.

Keep out:
- Session-level terminal parser.

### T63 - Production PTY: Process State and Termination

Primary scope:
- `src/terminal/pty.rs:204-388`

Supporting scope:
- `src/git/worktree.rs:112-126`
- `src/contracts/real.rs:61-94`

Review focus:
- Input/write/resize/read behavior.
- Process state polling.
- Windows `taskkill /T /F` tree termination.
- Wait-after-kill behavior for worktree removal reliability.

Keep out:
- Key mapping and app command close flow.

### T64 - Terminal Parser, Mouse, Selection, Bracketed Paste, CPR Replies

Primary scope:
- `src/terminal/session.rs:1-249`

Supporting scope:
- `src/tui/selection.rs:1-215`
- `src/lib.rs:2105-2221`
- Render selection in `src/tui/render.rs:605-683`

Review focus:
- VT100 parser lifecycle.
- Cursor-position report response (`ESC[6n`).
- Mouse and bracketed paste mode detection.
- Scrollback and selected text extraction.
- Resize behavior.

Keep out:
- Primary/child terminal ownership.

### T65 - Terminal Session Model: Primary and Children

Primary scope:
- `src/terminal/session.rs:251-859`

Supporting scope:
- App callers in `src/app/state.rs:1286-1322`, `src/app/state.rs:1554-1663`

Review focus:
- Primary and child spawn.
- Active terminal selection.
- Child close index fixup.
- Ctrl-C and terminate-all behavior.
- `all_stopped` definition.

Keep out:
- Production portable-pty implementation.

### T66 - Shell Resolution

Primary scope:
- `src/terminal/shell.rs:1-48`

Supporting scope:
- `src/app/state.rs:1286-1310`
- `src/runtime/container.rs:103-114`

Review focus:
- Default shell selection by platform/env.
- Child shell launch args.

Keep out:
- PTY spawn mechanics.

### T67 - TUI Platform Constants and Key Mapping

Primary scope:
- `src/tui/platform.rs:1-26`
- `src/tui/input.rs:1-725`

Supporting scope:
- `src/tui/render.rs:924-988`
- `src/lib.rs:1422-1482`

Review focus:
- Terminal/App mode distinction.
- Platform-default and optional F2 leave-terminal-focus behavior.
- Global shortcuts and Ctrl+U Pull Base.
- Ctrl-V image paste interception.
- VT key encoding.

Keep out:
- Prompt/palette key handling in `src/lib.rs`.

### T68 - Command Palette Model and Column Navigation

Primary scope:
- `src/tui/palette.rs:1-478`

Supporting scope:
- `src/tui/render.rs:1104-1203`
- `src/lib.rs:2020-2099`

Review focus:
- 20-entry action list and groups.
- Filtering and selection wrap.
- Left/right column navigation consistency with render split.
- Direct dispatch versus prompts.

Keep out:
- Overlay rendering details.

### T69 - Layout Math

Primary scope:
- `src/tui/layout.rs:1-460`

Supporting scope:
- `src/tui/render.rs:205-247`
- `src/lib.rs:1027-1036`, `src/lib.rs:2247-2300`

Review focus:
- Main layout rects.
- Header/sidebar/main/status geometry.
- Split-view region and columns.
- Overlay centering.

Keep out:
- Actual rendering style.

### T70 - Mouse Selection Geometry

Primary scope:
- `src/tui/selection.rs:1-215`

Supporting scope:
- `src/terminal/session.rs:138-248`
- `src/lib.rs:1045-1420`

Review focus:
- Rows-from-bottom coordinate model.
- Selection range and screen-row conversion.
- Scroll-stable selection behavior.

Keep out:
- Clipboard command implementation.

### T71 - Clipboard Helpers

Primary scope:
- `src/tui/clipboard.rs:1-276`

Supporting scope:
- `src/lib.rs:2163-2221`
- Selection copy caller in `src/lib.rs:1134-1146`

Review focus:
- Text copy via native commands and OSC 52 fallback.
- Image paste extraction and temp file path generation.
- Platform-specific clipboard behavior.
- Silent failure/fallback expectations.

Keep out:
- PTY paste encoding.

### T72 - Top-Level Rendering and Hit Testing

Primary scope:
- `src/tui/render.rs:1-247`

Supporting scope:
- Mouse wiring in `src/lib.rs:1045-1420`

Review focus:
- `UiOverlay` model.
- Sidebar/child/split-view hit testing.
- Top-level draw ordering.
- Sidebar chrome focus target.

Keep out:
- Individual render widgets below.

### T73 - Rendering: Header and Sidebar

Primary scope:
- `src/tui/render.rs:249-507`

Supporting scope:
- `src/app/state.rs:282-308`
- `src/git/status.rs:69-113`

Review focus:
- Full-width branded header.
- Sidebar tab structure.
- Creating spinner.
- Status colors and git indicators.

Keep out:
- Terminal viewport rendering.

### T74 - Rendering: Child Tabs, Terminal Viewport, VT Cells

Primary scope:
- `src/tui/render.rs:509-683`

Supporting scope:
- `src/terminal/session.rs:54-114`, `src/terminal/session.rs:138-248`

Review focus:
- Child tab rendering.
- Empty/no-tab/creating states.
- VT100 cell rendering, cursor positioning, selection highlight.

Keep out:
- Split view.

### T75 - Rendering: Split View

Primary scope:
- `src/tui/render.rs:685-793`
- `src/tui/layout.rs:129-203`
- `src/lib.rs:2247-2300`

Supporting scope:
- `src/tui/render.rs:114-130`

Review focus:
- Column/header rendering.
- Active cursor behavior.
- Separator placement.
- Agreement between rendering, hit testing, and PTY sizing.

Keep out:
- Normal child-tab view.

### T76 - Rendering: Git Info, Status Bar, Update Hint

Primary scope:
- `src/tui/render.rs:795-988`

Supporting scope:
- `src/git/status.rs:69-113`
- `src/update.rs:223-253`
- `src/app/state.rs:437-441`

Review focus:
- Info bar fallback and status formatting.
- Configured leave-focus key label.
- Update-available status bar hint.

Keep out:
- Git status overlay and update network/cache logic.

### T77 - Rendering: Git Status Overlay

Primary scope:
- `src/tui/render.rs:990-1102`

Supporting scope:
- App command in `src/app/state.rs:1649-1663`

Review focus:
- Overlay content.
- Dirty/upstream/ahead/behind/base drift rendering.
- PR URL optional display.
- No-diff boundary.

Keep out:
- Git status collection.

### T78 - Rendering: Palette, Help, Message Overlays

Primary scope:
- `src/tui/render.rs:1104-1320`
- Overlay-related tests in `src/tui/render.rs:1325-2002`

Supporting scope:
- `src/tui/palette.rs`
- `src/lib.rs:1580-1644`, `src/lib.rs:1963-2018`

Review focus:
- Two-column palette rendering and group headers.
- Help text consistency with key mapping.
- Message toast behavior.

Keep out:
- Palette data model internals.

### T79 - Event Loop Tick, Background Workers, and Update Notices

Primary scope:
- `src/lib.rs:750-902`
- `src/lib.rs:904-1036`

Supporting scope:
- `src/update.rs:223-253`
- `src/git/status.rs:83-113`

Review focus:
- Per-tick ordering.
- Worktree creation channel.
- Git status refresh worker.
- Background update notice channel.
- Render/input polling behavior.

Keep out:
- Mouse/key/paste handlers.

### T80 - Mouse Handling and Mouse Report Encoding

Primary scope:
- `src/lib.rs:1038-1420`

Supporting scope:
- `src/tui/render.rs:77-190`
- `src/terminal/session.rs:94-114`
- `src/tui/selection.rs`

Review focus:
- Sidebar/child/split-view click behavior.
- Selection target tracking across split panes.
- Local selection versus forwarded mouse-aware app events.
- Wheel forwarding versus local scrollback.
- Mouse report encoding.

Keep out:
- Rendering selection highlight.

### T81 - Key, Paste, Prompt, and Palette Wiring

Primary scope:
- `src/lib.rs:1422-2099`

Supporting scope:
- `src/tui/input.rs`
- `src/tui/palette.rs`
- `src/app/commands.rs`

Review focus:
- Modal priority and overlay dismissal.
- Bracketed paste routing into prompts/palette/terminal.
- Prompt state machine for new/rename/status/close/push/abandon/merge/rebase.
- Effect-to-overlay mapping.
- Palette key handling and action dispatch.

Keep out:
- App command implementations.

### T82 - PTY Drain, Paste Encoding, Resize, Teardown

Primary scope:
- `src/lib.rs:2101-2319`

Supporting scope:
- `src/terminal/session.rs:59-114`
- `src/tui/clipboard.rs:25-44`

Review focus:
- PTY output drain and status ingestion.
- Cursor-position query replies.
- Ctrl-V image paste path versus bracketed text paste.
- Newline normalization.
- Split-view resize sync.
- Persist-on-quit and terminate-all-sessions.

Keep out:
- Production PTY internals.

### T83 - Release and Distribution Automation

Primary scope:
- `scripts/release:1-86`
- `dist-workspace.toml:1-24`

Supporting scope:
- `Cargo.toml:1-66`
- `CHANGELOG.md:1-103`

Review focus:
- Version validation and clean-tree checks.
- Quality gates and dist checks.
- Release tag/push flow.
- Target platforms and Homebrew publishing.

Keep out:
- Runtime update code.

### T84 - Integration Test Harness: Init, Worktree, Recovery

Primary scope:
- `tests/integration.rs:1-19`
- `tests/integration/util.rs:1-31`
- `tests/integration/init.rs:1-172`
- `tests/integration/worktree.rs:1-184`
- `tests/integration/recovery.rs:1-138`

Supporting scope:
- Modules under test by imports.

Review focus:
- Shared temp git utilities.
- Cross-platform canonical path handling.
- Real FS/Git coverage for init, worktrees, and recovery.

Keep out:
- Push and merge integration tests.

### T85 - Integration Test Harness: Push and Merge Safety

Primary scope:
- `tests/integration/push.rs:1-185`
- `tests/integration/merge_preconditions.rs:1-265`

Supporting scope:
- `src/git/remote.rs`
- `src/git/status.rs:115-203`
- `src/git/worktree.rs`

Review focus:
- Local bare remote setup.
- Push planning and PR URLs.
- Merge success/conflict tests.
- No-FlightDeck-created-commits guarantee.

Keep out:
- Unit tests for pure helpers.

### T86 - Guard Tests for Product Invariants

Primary scope:
- `tests/guards.rs:1-122`

Supporting scope:
- `README.md:129-149`
- `src/contracts/traits.rs:15-84`

Review focus:
- Placeholder-name guard.
- Forbidden git subcommand scan.
- Rebase carve-out confinement to `src/git/repo.rs`.
- Runtime directory exclusion from git-subcommand scan.

Keep out:
- Runtime GitExecutor implementation details beyond tokens scanned.

### T87 - Example Key Logger

Primary scope:
- `examples/keylog.rs:1-56`

Supporting scope:
- `src/tui/input.rs:48-102`

Review focus:
- Debugging utility behavior.
- Raw mode/event capture safety.
- Platform key-inspection usefulness.

Keep out:
- Product runtime behavior.

### T88 - Documentation Consistency

Primary scope:
- `README.md:1-459`
- `containers/README.md`
- `specs/SPECS.md:1-1039`
- `specs/PLAN.md:1-548`
- `specs/CONTAINER_SUPPORT_PLAN.md`

Supporting scope:
- User-facing help in `src/lib.rs:581-610`
- Container CLI in `src/lib.rs:368-490`
- Update CLI in `src/lib.rs:321-366`, `src/update.rs`

Review focus:
- Public feature claims versus implemented behavior.
- Safety boundary wording, including rebase/pull-base carve-outs.
- Keyboard model and help text consistency.
- Container setup and guardrail docs.
- Update/install guidance.

Keep out:
- Code correctness beyond documented behavior existence.

## Suggested Review Order

1. T09, T10, T86: review contracts and safety invariants first.
2. T17-T23: review config, init, state, and recovery.
3. T24-T33: review Git behavior before app workflows that depend on it.
4. T54-T59: review container runtime primitives before AppState container launch flows.
5. T39-T53: review headless app behavior.
6. T60-T61: review update and update-notice behavior.
7. T62-T68: review PTY, session, key mapping, and palette primitives.
8. T69-T78: review rendering and layout.
9. T79-T82: review wiring/event loop/mouse/paste/teardown.
10. T83-T88: review tests, release automation, examples, and docs.

## Large File Split Index

`src/lib.rs`:
- T03: run entry and terminal setup/teardown.
- T04: setup-status/setup-notifications.
- T05: setup-update.
- T06: image build CLI.
- T07: doctor CLI.
- T08: startup.
- T79: event loop and background workers.
- T80: mouse handling.
- T81: key/paste/prompt/palette wiring.
- T82: PTY drain, paste encoding, resize, teardown.

`src/app/state.rs`:
- T39: construction, modes, selection, persistence.
- T40: status file polling and notification edge detection.
- T42: new-tab planning and placeholder.
- T43: finalize and spawn/failure cleanup.
- T44: rename, switch, manual status.
- T45: close tab and container teardown.
- T46: push branch.
- T47: finish/local merge.
- T48: rebase worktree.
- T49: pull base.
- T50: abandon worktree.
- T51: copy env file.
- T52: child terminals, restart, resume, status.
- T53: container launch/spec assembly.

`src/tui/render.rs`:
- T72: top-level draw and hit testing.
- T73: header and sidebar.
- T74: child tabs and VT terminal viewport.
- T75: split view.
- T76: git info, status bar, update hint.
- T77: git status overlay.
- T78: palette, help, and message overlays.

`src/testing/mod.rs`:
- T12: FakeFs.
- T13: FakeGit.
- T14: FakePty.
- T15: FakeContainerRuntime.
- T16: FakeClock.

`src/terminal/session.rs`:
- T64: terminal parser, mouse/bracketed modes, selection.
- T65: session ownership of primary/children and process controls.
