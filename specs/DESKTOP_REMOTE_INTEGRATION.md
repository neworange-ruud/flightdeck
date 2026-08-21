# Desktop Integration Map — FlightDeck Remote bridges

> Working engineering notes for the remote-control epic (generated 2026-07-13 from
> code exploration; line refs are approximate anchors, verify before editing).
> Audience: implementers of src/remote/ (relay client + bridges).

## Architecture summary

FlightDeck is a single-threaded synchronous TUI. The render loop (`event_loop`,
`src/lib.rs:1132`) runs poll(50ms) -> service-all-projects -> render -> read-input.
The headless core (`AppState`, `src/app/state.rs:428`) does no I/O; side effects go
through the `Services` trait bundle (`src/app/state.rs:47`). Background work =
detached `std::thread` workers reporting over `std::sync::mpsc`, drained
non-blockingly each tick. NO async runtime in the main graph (tokio only behind
non-Windows self-update feature). No websocket/QR/relay/pairing code exists yet.

## 1. Agent status

- Types in `src/contracts/domain.rs`: `ProcessState` (:20), `InterpretedStatus` (:54,
  as_str/from_str_lossy :74/:93), `ManualStatus` (:146). Combined `DisplayStatus` in
  `src/agents/status.rs:8`, pure combinator `combine_status` (:28).
- Per-tab runtime: `RuntimeTab` (`src/app/state.rs:234`) — `interpreted` (:245),
  `status_file` (:249), `status_file_seen` (:253), `notify_armed` (:258). Persisted:
  `TabState` (`domain.rs:491`) with `last_known_status` (:507), `manual_status` (:509).
- **`RuntimeTab::display_status(now_ms)` (`src/app/state.rs:286`) is THE read-model
  to serialize a tab's status for the phone.**
- Status propagation is file-polling, NOT mpsc: agent hooks (injected at spawn by
  `src/agents/setup.rs`) append keywords to `<worktree>/.flightdeck/agent-status`;
  `AppState::poll_status_files` (`src/app/state.rs:595`) diffs and mutates
  `tab.interpreted` each tick (called from `src/lib.rs:1195`).
- Project roll-up: `project_status_flags` (`src/lib.rs:1108`) -> (attention, busy);
  `Workspace::tab_infos(now_ms)` (`src/lib.rs:1085`) -> `ProjectTabInfo`.

## 2. Notifications (finish / needs-input events)

- `Notifier` trait `src/contracts/traits.rs:149`; `Notification` (`domain.rs:122`);
  `NotificationSound` (:135). Production `SystemNotifier` (`src/notify/mod.rs:33`),
  constructed `src/lib.rs:252`, passed into event_loop (:253).
- Pure detection in core: `NotifyKind` (`state.rs:369`), `NotifyPhase`/`notify_phase`
  (:401/:408) — Idle/Completed->Finish, WaitingForInput/NeedsAttention->Waiting,
  Failed->Failed. Edge detector `AppState::take_finish_notifications(now_ms)`
  (`state.rs:644`), armed via `notify_armed`, grace window `begin_notification_grace`
  (:629).
- Loop hook: `src/lib.rs:1200-1203` drains notifications for every project per tick.
- **Cleanest remote seam: a CompositeNotifier decorator wrapping SystemNotifier**,
  plus richer structured events pushed alongside (Notification alone lacks
  tab/session identity — bridge should emit typed events with session ids).

## 3. Terminal / transcript

- PTY traits `src/contracts/traits.rs:114/:127` (`write_input`, `resize`,
  `try_read_output`, `send_ctrl_c`, `process_state`, `terminate_tree`); real impl
  `PortablePtyBackend` (`src/terminal/pty.rs`). Per-terminal `Terminal`
  (`src/terminal/session.rs:25`) wraps vt100::Parser (2000-line scrollback),
  `process_output` (:58), `screen()` (:93), `selected_text` (:191), `read_row` (:230).
- Draining: `drain_pty_output` (`src/lib.rs:3387`) each tick.
- **There is NO transcript/message model** — only vt100 screen buffers. The
  transcript feed must tee raw PTY bytes in/near `drain_pty_output` (stream
  semantics) or snapshot `Terminal::screen()` (rendered-screen semantics).
  `AppEvent::PtyOutput` exists (`src/app/events.rs:36`) but is not produced today.
- Prompt/permission detection is via agent hooks (`src/agents/setup.rs`):
  `StatusBackend {Claude, Codex, OpenCode}` (:35), `prepare_status_launch` (:84),
  Claude plugin hooks (:175), Codex overrides (:159), OpenCode plugin (:192).
- Injecting phone replies: `write_active_pty(state, bytes)` (`src/lib.rs:3419`) or
  per-tab via `Session::active_mut`; bracketed paste `handle_paste`
  (`src/lib.rs:2050`) / `Terminal::bracketed_paste()` (`session.rs:115`).

## 4. Lifecycle

All in `src/app/state.rs`:
- Create (two-phase async): `begin_new_agent_tab` (:812) -> `WorktreeJob` (:82) ->
  worker `materialize_worktree` (:102) -> `finalize_new_tab` (:933) / `fail_new_tab`
  (:1024). Sync test path `cmd_new_agent_tab` (:786). Worker wiring
  `spawn_worktree_job` (`src/lib.rs:1342`), drained by `drain_create_outcomes` (:1368).
- Restart: `cmd_restart_agent` (:1836) -> `start_primary_for` (:1760).
  Resume-on-startup: `resume_agents` (:1853).
- Close: `cmd_close_tab(action, ...)` (:1044), `CloseAction` (`commands.rs:32`).
  Abandon: `cmd_abandon(confirm, ...)` (:1316). Merge: `cmd_finish_merge` (:1129).

## 5. Git

- `GitExecutor` (`src/contracts/traits.rs:28`): status_porcelain, ahead_behind,
  upstream_of, merge_no_ff (:67), rebase_onto (:75), pull_base (:84), worktree ops.
- Workflow helpers: `src/git/status.rs` (collect_status, check_merge_preconditions,
  merge_back, base_drift), `src/git/worktree.rs` (remove_worktree_if_safe),
  `src/git/branch.rs`.
- **Remote commands must reuse existing `Command` variants via dispatch — never call
  GitExecutor directly — to preserve safety guards.**

## 6. Manual status

`cmd_set_manual_status(Option<ManualStatus>, ...)` (`state.rs:1552`), command
variant `Command::SetManualStatus` (`commands.rs:163`), read via
`display_status().manual`.

## 7. Threading / config / persistence

- Channels: per-project on `struct Project` (`src/lib.rs:990`): create_tx/rx (:1000),
  status_tx/rx (:1003), `git_lock: Arc<Mutex<()>>` (:1009). Workspace `update_rx`
  (:1151). All `try_recv()` drained per tick (:1184, :1207).
- Commands flow through `AppState::dispatch(Command, &Services) -> Result<Effect>`
  (`state.rs:737`); loop-side `dispatch_command` (`src/lib.rs:2092`) + `apply_effect`
  (:2176).
- **Websocket client thread idiom: mirror `update::start_check` (`src/update.rs:245`)**
  — long-lived std::thread owning the socket; `Sender<RemoteInbound>` in,
  `Receiver<RemoteOutbound>` out; loop drains inbound -> dispatch_command /
  write_active_pty; pushes outbound snapshots after poll_status_files (~lib.rs:1200).
  No new locking of AppState needed.
- Config: layered `~/.flightdeck/config.toml` + project `.flightdeck/config.toml`
  (`src/config/load.rs`, `parse_config` :54). Add `RemoteConfig` in
  `contracts/domain.rs` mirroring `NotificationsConfig` (:293) / `UpdateConfig`
  (:331); wire into `Config` (:464), `default_config` (`src/config/schema.rs:50`),
  optional `validate` (:78). Keep Eq (no floats).
- Persistence: `.flightdeck/state.json` (`ProjectState` domain.rs:526,
  `src/persistence/project_state.rs`). **Pairing/device identity belongs per-user in
  `~/.flightdeck/remote.json`**, mirroring `src/persistence/workspace.rs`
  (load/save_workspace :54/:67, best-effort, via FileSystem trait).

## 8. TUI surface for pairing screen

- `UiOverlay` enum (`src/tui/render.rs:51`); `Dialog` model (:141) with
  confirm/input/browser/notification constructors (:157-:198); prompts `enum Prompt`
  (`src/lib.rs:779`); palette `src/tui/palette.rs` (entries :60+, `PaletteAction`),
  handled `src/lib.rs:3093/:3130`.
- Pairing screen idiom: copy ConfigManager (`src/tui/config_manager.rs:84`,
  opened via `open_config_manager` `src/lib.rs:3155`, drawn by `draw_config_overlay`
  `render.rs:1839`): new `UiOverlay::Remote(...)` + palette entry + draw fn.
- No QR capability exists; add `qrcode` crate and draw half-block cells, or start
  with 4-digit code via `Dialog`.

## 9. Testing constraints

- Fakes in `src/testing/mod.rs` (FakeFs/FakeGit/FakePty/FakeClock/...). NO
  FakeNotifier yet — add one when needed.
- **`tests/guards.rs` scans ALL .rs under src/ (except src/runtime/)**: forbidden
  quoted literals include "commit", "amend", "cherry-pick", "reset",
  "filter-branch", "-f", "gh"; "rebase" only allowed in src/git/repo.rs; "--force"
  only on a worktree-remove line. src/remote/ code must avoid these literals in ANY
  string (e.g. no message type named "reset").
- Unit tests in `#[cfg(test)] mod tests` per module; integration in
  `tests/integration/`; insta + tempfile available.

## Recommended src/remote/ layout

```
src/remote/
  mod.rs       // module docs + re-exports
  protocol.rs  // pure mapping: flightdeck-remote-protocol types <-> app types
  pairing.rs   // pairing/device-token model, code generation
  state.rs     // ~/.flightdeck/remote.json persistence (FileSystem trait)
  client.rs    // relay websocket thread (mirror update::start_check)
  bridge.rs    // per-tick glue: drain inbound -> Command; push outbound snapshots
  notifier.rs  // CompositeNotifier / remote event forwarding
```

Minimal seams: (1) Notifier decorator at `src/lib.rs:252-253`; (2) channels + thread
near `src/lib.rs:1151`; (3) inbound drain in per-tick block (~:1184) ->
`dispatch_command`/`write_active_pty`; (4) outbound snapshot push after
`poll_status_files` (~:1200) using `display_status` + `tab_infos`; (5) RemoteConfig;
(6) `~/.flightdeck/remote.json`; (7) `UiOverlay::Remote` pairing overlay.
