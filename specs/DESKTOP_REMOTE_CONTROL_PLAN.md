# FlightDeck Desktop as a remote control — implementation plan

> Status: **draft for review** (2026-10-02). Nothing here is implemented.
> Scope: the Desktop app (GPUI, `desktop/`) connects to a FlightDeck instance
> running on another machine (TUI first, Desktop too) and controls it. Only the
> Desktop app gains the *controller* role; the TUI and the Desktop app already
> have the *controlled* role through the embedded web server.

## 0. Decisions taken with Ruud

| # | Question | Decision |
| --- | --- | --- |
| R1 | Reach in v1 | **LAN or VPN only.** Direct WebSocket to the controlled instance's embedded web server. No relay work. Tailscale/WireGuard covers remote machines. |
| R2 | Selection model | **Independent browsing.** The Desktop views any tab of the controlled instance without moving that instance's own selection. |
| R3 | Pairing | **Address + 4-digit code**, reusing the controlled instance's existing web access overlay. The resulting token is stored per remote; reconnects need no new code. |
| R4 | Sessions | **One remote per window.** Local projects and a remote instance never mix in one window. |
| R5 | Default seat | **Control straight away.** Attach as a writer like the browser; the input-lock arbiter keeps both users from typing over each other. Observe is one click away in the status bar. |
| R6 | Token store | **`~/.flightdeck/remotes.json`, mode 0600**, via the `FileSystem` trait, like the phone pairing state. No keychain crate. |
| R7 | Host marker | **Yes, a small marker** on the tab the controlled instance itself is viewing, driven by the `Selection` delta. No follow-host mode in v1. |

## 1. Why the web protocol and not the relay

Both existing stacks were researched in depth (`src/web/`, `webui/`,
`specs/WEB_INTERFACE.md`; `src/remote/`, `remote/`, `specs/REMOTE_PROTOCOL.md`).

**FlightDeck Web is already the right protocol.** Its `Snapshot` carries the
whole workspace (projects, sessions, status, git bar, terminals, dialogs,
palette inventory, config manager, activity), and the server tees **raw PTY
bytes of every terminal of every project to every viewer** with a 256 KiB ring
and byte-offset resume (`src/lib.rs:1717` `tee`, `src/web/stream.rs`,
`src/web/replay.rs`). Input is per terminal id with per-keystroke acks and
exactly-once replay across reconnects. An input-lock arbiter and seats already
handle "the local user and the remote user both type". The serde types live in
the same crate the Desktop links, so there is no second protocol to mirror.
The controlled instance needs **one** additive protocol change for R2 (§3.1).

**FlightDeck Remote (relay) is the wrong tool for this.** Roles are hard-coded
`desktop`/`phone`; there is no agent-terminal stream at all (the transcript is
rebuilt from the agent's own JSONL/SQLite logs); every message is JSON, then
AEAD, then a SQLite write on the single-replica relay; the per-pairing queue
drops at 1000 envelopes. Using it for interactive terminal mirroring would be
a redesign of the relay, not a reuse.

Performance of the chosen path: terminal bytes leave the host at its tick
cadence (TUI 50 ms, Desktop 16 ms while output flows) and are parsed by the
Desktop's existing alacritty-based emulator; the only added latency is the
LAN round trip. Reliability: resume offsets, input replay, reconnect backoff
and token revocation already exist and are covered by `tests/web_server.rs`.

## 2. Architecture

```
 controlled machine (TUI or Desktop)            controlling Desktop
 ┌──────────────────────────────┐                ┌──────────────────────────────┐
 │ AppHost                      │                │ GPUI window (remote kind)    │
 │  └ WebSurface ── axum /ws ───┼── LAN / VPN ───┼─ web::client (tokio task)    │
 │     tee: TermBytes (all)     │   plain WS     │   ├ RemoteWorkspace (mirror) │
 │     Snapshot + Delta         │                │   │   local selection        │
 │     Ack / GitStatus / Config │                │   ├ RemoteTerminal × N       │
 │  ← Input{terminal_id}        │                │   │   StreamPty → Terminal   │
 │  ← Command{name, args+target}│                │   └ input seq queue + resume │
 └──────────────────────────────┘                └──────────────────────────────┘
```

### 2.1 Controlled side (host) — stays the web server

No new server. The TUI and the Desktop app both own a `WebSurface` through
`AppHost` (`src/host/mod.rs:230`). The user starts the web interface, switches
the access overlay to network mode (`n`), and reads the address + code. The
only host-side change is §3.1 (explicit command targets), shipped in the same
release so both apps gain it.

### 2.2 Controlling side — new Rust client in the core crate

`src/web/client/` (new module in the `flightdeck` library, so it is unit-tested
against the real server with the `tests/web_server.rs` harness, not inside
GPUI):

- `exchange.rs` — `POST /auth/exchange` with the code, keep the
  `flightdeck_web` cookie; `GET /auth/session` to probe a stored token. Hand-
  rolled HTTP over `tokio::net::TcpStream` exactly as `tests/web_server.rs:173`
  does today; the crate has no HTTP client and must not grow one for two
  requests.
- `link.rs` — the WebSocket task on the shared runtime (`src/remote/runtime.rs`),
  modelled on `src/remote/client.rs`: `Attach{seat, cursors, resume_viewer,
  viewport}` → `Snapshot` → pump; backoff 250 ms → 8 s like the browser
  (`webui/src/wire/socket.ts:60`); `std::sync::mpsc` in both directions so the
  GPUI side stays synchronous, same idiom as `RemoteInbound`/`RemoteOutbound`.
- `mirror.rs` — `RemoteWorkspace`: applies `Snapshot` then every `Delta`
  (`project_upsert`, `session_upsert`, `status`, `git`, `terminal_upsert`,
  `terminal_closed`, `geometry`, `dialog_opened/closed`, `seats`, `activity`).
  Holds **its own selection** (R2) and records the host's `Selection` only as
  a marker. Pure, no I/O, snapshot-tested.
- `input.rs` — the held-input queue with one seq counter, replay after
  `Snapshot.last_input_seq` on reattach (the browser's §5.1 rule, in Rust).
- `terminals.rs` — one `StreamPty` per remote terminal id implementing
  `PtySession` (`src/contracts/traits.rs:153`): `try_read_output` drains the
  bytes the link delivered for that id, `write_input` enqueues
  `ClientMsg::Input`, `resize` is a no-op (host owns geometry, D4),
  `process_state` mirrors `SessionView.lifecycle`. Wrapped in the core's
  `Terminal` on the Desktop's alacritty profile so **the existing terminal
  element renders a remote terminal unchanged**. Per-terminal `next_offset`
  cursors feed the next `Attach`.

### 2.3 Desktop — a second surface behind the same views

Today every view reads `HostModel` → `AppHost` through ~20 methods
(`overlay`, `workspace_ui`, `active_state`, `active_terminal`, `agent_rows`,
`git_strip`, `project_tabs`, `remote_status`, `notices`, `needs_you_count`,
`pending_prompt`, …). Introduce one seam:

```rust
pub enum Surface { Local(AppHost<'static>), Remote(RemoteSession) }
```

and a read-model trait `SurfaceRead` with exactly the methods the views call,
returning the core's existing view structs (`view::AgentRowView`,
`GitStripView`, `ProjectTabView`, `OverlayView`, `TerminalRef`). `Local`
delegates to `AppHost`; `Remote` maps `RemoteWorkspace` → the same structs.
`HostModel::turn` becomes: pump the surface (local tick, or drain the link's
inbound), then redraw on change. `TerminalSource` (`desktop/src/terminal/view.rs:110`)
gains `Remote(Entity<HostModel>)` reading the selected `RemoteTerminal`.

Views stay untouched where they already consume view structs; the eight
`active_state()` call sites (`shell.rs`, `split.rs`, `sidebar.rs`, `mission/`,
`zoom.rs`, `terminal/view.rs`) are the ones that must move onto the trait
(mode, selected tab name, focused child, split flag, three `config.ui` values).

Writes go through `HostEvent` today; a remote surface translates the same
`HostEvent`s into `ClientMsg`: `TerminalInput` → `Input{terminal_id}`,
`Command(..)` → `Command{name, args: {session_id|terminal_id}}` using the
host's `Snapshot.commands` inventory, `Paste` → bracketed-paste bytes as
`Input`. Unknown or refused commands show the host's `Ack.detail` as a notice.

### 2.4 Window and persistence

- Launcher (`desktop/src/root.rs`) gains **Connect to remote…** beside
  Open project. The menu gets the same entry; it opens a new window of the
  remote kind (R4).
- Connect overlay: address (`host:port`, default port 7420), 4-digit code,
  a seat choice (Control / Observe), remembered remotes list with "forget".
- `~/.flightdeck/remotes.json` (mode 0600, via the `FileSystem` trait, like
  `src/remote/state.rs`): per remote `{label, address, token, last_seen,
  viewer_id, host_version}`. The token is a bearer secret; the controlled
  instance can revoke it per browser row in its access overlay, where the
  Desktop appears with user-agent label `FlightDeck Desktop/<version>`.
- Title bar / status bar for a remote window: host name + address, link state
  (connecting / live · 23 ms / reconnecting / revoked / host quit), seat and
  input-lock holder, and a "host is viewing <tab>" marker (§3.2).

## 3. Independent browsing (R2) — what it takes

### 3.1 Host change: explicit command targets (protocol v6)

Terminal bytes already flow for every terminal, and `Input` is already per
terminal id, so **viewing and typing into any tab needs no host change**. What
is bound to the host's selection is the `Route::Palette` command path
(`src/web/commands.rs:139`, `src/lib.rs:7189`): `close_agent_session_tab`,
`rename_agent_session_tab`, `restart_agent`, `new_child_terminal`,
`close_child_terminal`, `set_manual_status`, `push_branch`,
`finish_local_merge`, `rebase_worktree`, `abandon_worktree`, `show_git_status`,
`open_shell`, `toggle_split_view`, `new_agent`, `close_agent` all run through
`run_palette_action` against the **selected** tab, and the dialogs they open
(D13, shared) act on the selection at confirm time.

Plan: give the session-scoped `app::Command` variants an explicit target,
`target: Option<TabId>` (`None` = selected, so the TUI and every existing
caller are unchanged), and make `AppState::dispatch` resolve the tab from the
target instead of `selected_tab` for those variants. Dialogs opened by a
targeted command carry the target in their `DialogKind` facts and confirm
against it. On the wire: `Route::Palette` reads `args.session_id` /
`args.terminal_id` when present (today it ignores `args`); the browser keeps
sending none and keeps its shared-selection behaviour. `PROTOCOL_VERSION`
5 → 6 in `src/web/protocol.rs:204` and `webui/src/wire/frames.ts:61`
(additive; the browser only needs the constant bump).

The alternative — temporarily swapping `selected_tab` around the dispatch — is
smaller but breaks on anything two-phase (dialogs, the worktree create job),
so it is rejected.

Also in v6: the server must **not** treat a viewer's `Attach.viewport` or
`Resize` as a reason to change anything (it does not today; keep it so), and
`Selection` deltas keep flowing so the Desktop can draw the marker.

### 3.2 What the Desktop user sees

- Switching tabs on the Desktop changes only the Desktop. The host's own
  selection is drawn as a small marker in the sidebar ("● host") so the two
  users know where the other is.
- Geometry: a non-selected tab on the host keeps its last PTY size (the host
  resizes only its selected tab, `sync_selected_tab_sizes`). The Desktop
  letterboxes that grid, like the browser. Acceptable for v1; a
  viewer-requested size for unselected terminals is listed under Later.
- Dialogs the Desktop opens still appear on the host screen too (D13). That is
  today's web behaviour and stays in v1; noted as a Later item.

## 4. Security stance for v1

Same as FlightDeck Web (D1/D5/D17): plain WS, intended for a LAN or a VPN the
user trusts; the controlled instance must opt into network mode. The Desktop
warns once per remote that the link is not encrypted unless the address is a
Tailscale/WireGuard/loopback range, and never auto-connects on launch without
the user choosing the remote. Token at rest: `remotes.json` 0600. Rate limits,
single-use codes and revocation are the server's existing ones.

Later (not v1): a self-signed certificate per host, pinned on first connect
(TOFU), which would also make Internet use via port-forward safe; a relay
tunnel is explicitly out of scope per R1.

## 5. Milestones

Each milestone ends with the ship gate (`.agents/skills/shipping-flightdeck-changes`),
a changelog entry (root `CHANGELOG.md` for host/core changes, `desktop/CHANGELOG.md`
for the app), and a local build handed over. Tracked as one beads epic
(`area:gui area:web`) with one child per milestone, created after this plan is
approved.

### M0 — Host: explicit command targets (protocol v6)

Files: `src/app/commands.rs`, `src/app/state.rs` (target resolution),
`src/web/protocol.rs` (version, docs), `src/web/commands.rs` (read
`session_id`/`terminal_id` on `Route::Palette`), `src/lib.rs`
(`run_web_command`, dialog facts), `webui/src/wire/frames.ts` (constant),
`tests/web_server.rs` (a targeted `restart_agent` and `close_agent_session_tab`
against a non-selected tab; a targeted close confirmed through the shared
dialog acts on the target).
Acceptance: TUI behaviour unchanged (`cargo test -p flightdeck --lib`,
guards); browser e2e still green; the two new server tests pass.
Size: ~2 days.

### M1 — Core: `web::client` (headless, tested against the real server)

Files: `src/web/client/{mod,exchange,link,mirror,input,terminals}.rs`,
`src/web/mod.rs`; tests in `tests/web_client.rs` reusing the `web_server.rs`
harness: exchange a code, attach, receive snapshot, mirror deltas, receive
bytes for a non-selected terminal, type into it and see the host seam get the
bytes, reconnect with offsets and replay held input exactly once, survive
`Shutdown{restarting}`, stop on `token_revoked`.
Acceptance: all of the above green; `RemoteWorkspace` snapshot tests.
Size: ~3 days.

### M2 — Desktop: surface seam + remote terminal

Files: `desktop/src/host.rs` (`Surface`, `SurfaceRead`, turn loop),
`desktop/src/shell.rs`, `views/{sidebar,split,status_bar,titlebar,git_strip}.rs`,
`views/mission/mod.rs`, `terminal/view.rs` (`TerminalSource::Remote`),
`terminal/zoom.rs`, `overlays/mod.rs` (dialogs, palette from the host's
inventory, git status, config manager — all already carried by the snapshot).
Acceptance: existing GPUI tests unchanged for `Local`; new tests drive a
`Remote` surface from a recorded snapshot and assert the sidebar, git strip
and terminal grid; a manual check against a real TUI on another machine.
Size: ~4 days (the `active_state()` call sites are the fiddly part).

### M3 — Desktop: connect UX, saved remotes, window kind

Files: `desktop/src/root.rs` (launcher entry), `menus.rs`, new
`overlays/connect.rs`, `desktop/src/remotes.rs` (persistence via `FileSystem`),
`views/titlebar.rs`/`status_bar.rs` (link state, seat, host marker),
`app.rs` (window kind).
Acceptance: connect by address + code, relaunch and reconnect from the saved
list without a code, observe-only seat, take over the input lock, clear
"revoked"/"host quit"/"reconnecting" states, the unencrypted-link warning.
Size: ~3 days.

### M4 — Docs and release

`README.md` (a "Control another machine from FlightDeck Desktop" section),
`specs/WEB_INTERFACE.md` amendment (D3 revised: selection is shared for
browsers, local for native clients; D4 unchanged), both changelogs, desktop
version bump per `desktop-release-separation`. Size: ~1 day.

Total: roughly 13 working days, M0 and M1 independent of the GPUI work and
reviewable on their own.

## 6. Later (explicitly not in this plan)

- mDNS discovery of instances on the LAN.
- TLS with a pinned self-signed certificate per host.
- Relay tunnel for Internet reach (would reuse §2.2 unchanged behind the link).
- Viewer-requested geometry for terminals the host is not showing.
- Per-viewer dialogs instead of D13's shared dialog.
- Binary WebSocket frames for `TermBytes` (today base64 in JSON; fine on a LAN).
- Remotes alongside local projects in one window (R4 chose separate windows).

## 7. Risks

- **`active_state()` coupling in the Desktop.** Eight call sites read the
  `AppState` directly; if more appear during M2 the trait grows. Mitigation:
  M2 starts with an inventory and a failing compile, not with features.
- **Shared dialogs (D13).** A confirm the Desktop opens is also on the host
  screen; the host user can answer it. Acceptable in v1, documented.
- **Stale geometry for unselected tabs.** Letterboxed, possibly small. Known.
- **Plain WS.** Mitigated by the warning and the LAN/VPN scope; TLS is a
  bounded follow-up.
- **Protocol version bump** breaks a stale browser tab until reload — that is
  the designed behaviour (`version_mismatch` → "reload to update").

## 8. Review outcome

All open points were decided on 2026-10-02 and folded into §0 (R5–R7). The
plan is approved for tracking: next step is the beads epic with one child per
milestone, then M0.
