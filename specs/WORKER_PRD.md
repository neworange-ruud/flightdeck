# FlightDeck Worker — product requirements

> Status: **requirements agreed, not started** (2026-10-23). Epic
> `remote-control-qlyd`. Decisions W1–W30 were taken with Ruud in a three-round
> interview; open decisions O1–O4 are still to be made and are filed as
> children of the epic.
> Scope: a UI-less FlightDeck host that runs on a remote machine and is
> controlled from FlightDeck Desktop over Tailscale.

## 1. What it is

`flightdeck worker` is a subcommand of the existing `flightdeck` binary that
runs the application host with no front-end. It orchestrates agents exactly as
the TUI does — projects, worktrees, agent sessions, status hooks, git — but
nobody sits at the machine. You put it on a spare Mac or a Linux box, pair
your Desktop with it once, and from then on the Desktop's remote window is the
only screen it has.

Nearly all of this already exists. `AppHost` (`src/host/mod.rs`) is the
front-end-neutral host the TUI and the Desktop both drive, and FlightDeck Web
(`src/web/`) is the protocol a Desktop remote window already speaks to a TUI on
another machine (`specs/DESKTOP_REMOTE_CONTROL_PLAN.md`). The worker is a third
driver loop around `AppHost`, plus the things a host with no screen and no
keyboard cannot do today: show a pairing code, size its PTYs, open projects,
give you a shell, and tell you something went wrong.

```
 worker machine (Linux / macOS)                     your machine
 ┌───────────────────────────────────────┐          ┌──────────────────────────────┐
 │ flightdeck worker supervise           │          │ FlightDeck Desktop           │
 │  └ flightdeck worker run              │          │  ├ Workers list (launcher)   │
 │     AppHost (no front-end)            │          │  │   background link × N     │
 │      ├ projects / agents / git        │ tailnet  │  │   OS notifications        │
 │      ├ worker shell PTY               │  plain   │  └ remote window (1 worker)  │
 │      └ WebSurface ── /ws ─────────────┼── WS ────┼──── web::client             │
 │   control socket (0600)               │          └──────────────────────────────┘
 │    ▲                                  │
 │    └─ worker status|pair|devices|…    │   (over SSH, on the box)
 └───────────────────────────────────────┘
```

## 2. Goals and non-goals

**Goals (v1)**

- Run agents on a remote machine you control entirely from the Desktop.
- Reuse the web protocol and the Desktop's remote window. No second protocol.
- Expose nothing outside the tailnet.
- Survive crashes (supervisor) and restarts (agents resume as in the TUI).
- Be diagnosable without a screen: status, logs, messages surfaced in the Desktop.

**Non-goals (v1)** — each one was explicitly decided against or deferred:

- Windows workers (W2).
- Init-system integration and start on boot (W3, W11). After a reboot you run
  `flightdeck worker start` yourself.
- The phone. No relay pairing, no direct link, no pushes from a worker (W6, W18).
- More than one person. One user, one attached window at a time (W10).
- FlightDeck managing agent or git credentials (W12, W13).
- Updating a worker from the Desktop (W20 → deferred by W25; follow-up issue).
- The new host capabilities on TUI or Desktop hosts (W23).
- A TUI that attaches to a running worker (W22).
- Listening on anything but Tailscale addresses, and so TLS (W8).

## 3. Decisions

| # | Topic | Decision |
| --- | --- | --- |
| W1 | Packaging | A **subcommand** of the existing binary: `flightdeck worker …`. Shares `AppHost`, config, and self-update. "Lightweight" means no rendering, not a smaller binary. |
| W2 | Platforms | **Linux and macOS.** Windows is out of scope. |
| W3 | Lifecycle | **Self-daemonizing** with `start`/`stop`/`status`, a pidfile, and no init-system integration. |
| W4 | Restart | Agents **resume like the TUI's**: worktrees and tabs persist, agents relaunch with their resume command, an in-flight turn is lost. |
| W5 | PTY geometry | The **controlling Desktop's pane size** drives the PTY. This relaxes D4 (host owns geometry) **for worker hosts only**. |
| W6 | Protocol | The **web protocol**, as a Desktop remote window uses against a TUI today. |
| W7 | Pairing | **CLI-issued code**: `flightdeck worker pair` on the box prints the address and a one-time code; the Desktop's pair flow is unchanged and saves a token. |
| W8 | Binding | **Tailscale addresses only**, as the phone direct link does. Plain WebSocket; WireGuard encrypts. |
| W9 | Projects | The Desktop can **open a path**, **clone a URL**, and pick from a **scanned root directory**. |
| W10 | Users | **One user, one device at a time.** |
| W11 | Crash recovery | A **supervisor** restarts the worker if it dies. No start on boot. |
| W12 | Agent auth | You log in to Claude Code, Codex, OpenCode and Cursor **in a worker shell**. FlightDeck stays out of credentials. |
| W13 | Git credentials | The **worker account's own** SSH keys / `gh auth`. Failures are shown clearly in the Desktop. |
| W14 | Isolation | **Same as the TUI**: project and global config decide (`isolated`, containers). No worker-specific default. |
| W15 | Shell | A **worker-hosted shell**: the worker spawns a login shell PTY and streams it over the web protocol. No SSH needed. |
| W16 | Filesystem reach | Opening and browsing reach the **whole home directory** of the worker's account. |
| W17 | Second window | **The new one takes over.** The old window is told who took over and goes to a reconnect screen. |
| W18 | Notifications | A **Desktop background link** raises OS notifications when no window is open on that worker. |
| W19 | Background links | They **do not count as attached.** Any number of paired Desktops may hold one; only an open window triggers takeover. |
| W20 | Updates (intent) | Update from the Desktop, with the supervisor restarting the worker. **Deferred to after v1 by W25.** |
| W21 | Desktop UX | A **Workers** section in the launcher with live status; opening one gives a remote window (one remote per window, R4). |
| W22 | Local TUI | Running a local host on the worker machine while the worker runs **refuses with a hint**. |
| W23 | Parity | **Worker-only.** None of the new host capabilities change TUI or Desktop hosting. |
| W24 | Name | `flightdeck worker`. |
| W25 | v1 scope | In v1: the worker shell, clone from URL, background link + notifications. Not in v1: update from the Desktop. |
| W26 | Config | The worker's **configuration manager is reachable from the Desktop** in v1. |
| W27 | No tailnet | **Keep running and retry the bind**; rebind when the address changes. `worker status` says why it isn't listening. |
| W28 | Architectures | **x86_64 and aarch64** on both Linux and macOS. |
| W29 | Observability | A rotating log + `worker logs`; `worker status` (text and JSON); health in the Desktop; host messages forwarded to the Desktop. |
| W30 | Done | Headless integration tests in CI, a manual pass on a real Mac, and a docs page. |

## 4. Requirements

The decisions in §3 come from the interview. The concrete numbers below
(backoff, log rotation, default PTY size, scan depth, intervals, buffer sizes)
and the default paths are proposed defaults, to be confirmed in implementation.

### 4.1 Command surface

| Command | What it does |
| --- | --- |
| `flightdeck worker start` | Starts the supervisor in the background and returns once the worker answers on the control socket (or prints why it didn't). Refuses if a worker is already running for this account. |
| `flightdeck worker stop` | Stops the supervisor and the worker gracefully: agents are terminated as on a TUI quit, state is persisted, so the next `start` resumes them. |
| `flightdeck worker restart` | Stop, then start. This is how a `flightdeck update` takes effect, and it re-executes the supervisor too, so both run the new binary. |
| `flightdeck worker status [--json]` | Running or stopped; pid; uptime; version; listen addresses, or why it isn't listening (W27); paired devices; the attached device, if any; background links; projects; agents and their status. When stopped: the supervisor's last exit reason. |
| `flightdeck worker pair` | Mints a one-time code in the running worker and prints it with the address to type in the Desktop (the MagicDNS name when Tailscale has one, else the `100.x` address, with the port). |
| `flightdeck worker devices` | Lists paired Desktops: label, paired at, last seen, attached / background / offline. |
| `flightdeck worker revoke <device>` | Revokes a Desktop's token and closes its connections at once. |
| `flightdeck worker logs [-f] [-n N]` | Prints or follows the worker log. Reads the file directly, so it works while the worker is down. |
| `flightdeck worker run` | The worker in the foreground, with logs on stderr. What the supervisor runs; also for debugging. |

`flightdeck worker` with no subcommand prints this table. All of them are
Linux and macOS only; on Windows they exit with a clear "not supported" error.

### 4.2 Process model (W3, W4, W11)

- `start` spawns `flightdeck worker supervise` as a **new process** (via
  `std::process::Command`, `setsid` in `pre_exec`, stdio to the log), never by
  forking the running process. The supervisor spawns `flightdeck worker run` the
  same way. Neither ever forks after a runtime or a system framework is up, which
  sidesteps the macOS fork-safety problem entirely.
- The supervisor is small: no tokio, no `AppHost`. It restarts a worker that
  exits abnormally, with exponential backoff (1 s doubling to 60 s), and gives up
  after 5 crashes within 10 minutes, recording the reason for `worker status`.
  A clean exit after `stop` is not restarted.
- Pidfile and control socket live in `~/.flightdeck/worker/`, the directory mode
  0700 and the socket 0600.
- On start, the worker opens its workspace (`~/.flightdeck/worker/workspace.json`
  or the existing workspace state, decided in implementation) and resumes agents
  exactly as `AppHost::resume_launch_project` / `resume_agents` do for the TUI.
- **Local host lock (W22).** While a worker runs for an account, starting the TUI
  or opening a *local* Desktop window on that machine refuses with: "A FlightDeck
  worker is running on this machine and serving its projects. Control it from
  FlightDeck Desktop, or run `flightdeck worker stop`." Desktop *remote* windows
  on the worker machine are unaffected.

### 4.3 Network and pairing (W6–W8, W17, W19, W27)

- The worker serves the web protocol's `/ws` and `/auth/exchange` on port 7420
  by default (`[worker] port`). It binds **only** the addresses Tailscale reports,
  reusing `src/remote/direct/net.rs` (`tailscale_addresses`,
  `select_bind_addresses`), never `0.0.0.0` and never loopback.
- When there is no Tailscale address the worker keeps running: agents work, the
  status says "not listening: no Tailscale address", and it polls (every 5 s) and
  binds when an address appears. When the address set changes it rebinds.
  Attached clients reconnect through the client's existing backoff and resume.
- `worker pair` reaches the running worker over the control socket, which mints a
  bootstrap code with the same credential store FlightDeck Web uses
  (`src/web/credentials.rs`: single use, `BOOTSTRAP_CODE_TTL_MS`). The Desktop's
  "Connect to remote…" pair flow is unchanged. Whether that code is strong enough
  when pairing grants a shell is O4.
- **One attached window (W10, W17).** A worker allows one remote window at a time.
  When a second paired Desktop opens a window, the first receives a "taken over
  by <device label>" close and shows a reconnect screen; its token stays valid.
  This replaces the multi-writer seat model on worker hosts only.
- **Background link (W18, W19).** A new, status-only connection kind: it receives
  the workspace's projects, sessions, statuses, health and host messages, and
  **no terminal bytes and no input**. Any number of paired Desktops may hold one.
  It never takes over and is never taken over.
- A revoked token closes every connection that used it, window and background link.

### 4.4 Host behaviour on a worker

- **PTY geometry (W5).** The attached window's pane size sets each PTY's size,
  using a new resize message that names a terminal. With no window attached, PTYs
  keep their last size; a new agent with no size yet starts at a configured default
  (`[worker] default_cols`/`default_rows`, 200×50). TUI and Desktop hosts keep D4
  unchanged and ignore the new message.
- **Worker shell (W15).** Every worker window has a **Worker shell** tab, first,
  where the SSH shell tab sits today in a remote window (the SSH tab is not shown
  for workers). It is a login shell (`$SHELL -l`, falling back to `/bin/sh`) in
  `$HOME`, running as the worker's account, streamed and resized like an agent
  terminal. It outlives a detach and is restarted on demand after it exits. Host
  commands are refused on that tab, as on the SSH tab.
- **Projects (W9, W16).**
  - *Open path*: the Desktop browses the worker's filesystem from `$HOME` down and
    opens any git repository there as a project. Paths are canonicalised; anything
    that resolves outside `$HOME` (including through a symlink) is refused.
  - *Scan root*: `[worker] projects_root` (default `~/flightdeck`) is scanned for git
    repositories, two levels deep, and offered in the open-project dialog.
  - *Clone*: the Desktop sends a git URL; the worker clones into `projects_root/<name>`
    with progress reported to the window, then opens it. Failures (authentication,
    host key, network, name in use) are reported with git's own message.
- **Credentials (W12, W13).** The worker never stores, forwards or injects agent or
  git credentials. It runs agents and git with its account's environment. The docs
  say how to log in to each agent CLI from the worker shell.
- **Isolation (W14).** Unchanged: config decides.
- **Configuration (W26).** A remote window on a worker opens the worker's
  configuration manager (global and project config) and saves through it. This is
  `remote-control-96y4`'s feature; whether it stays worker-only is O2.

### 4.5 Desktop (W18, W21, W29)

- The launcher gets a **Workers** section listing every saved remote whose host
  says it is a worker: name, online / offline / not listening, agents needing you,
  version and health.
- The Desktop keeps a background link to every saved worker while it runs, with
  reconnect backoff, starting at launch.
- A status edge that would notify on a local host (needs input, error, done) raises
  an OS notification through the Desktop's existing notifier, naming the worker.
  While that worker's window is open, the window notifies instead, so nothing is
  doubled.
- Opening a worker opens a remote window as today (R4), with the worker shell tab
  and the project actions above.
- **Health**: uptime, version, CPU and memory use, free disk in `$HOME` and
  `projects_root`, and the agent count, sent every 10 s on both connection kinds.
- **Host messages**: whatever the TUI would put in its status line (merge refused,
  spawn failed, clone failed, update available) goes to the attached window. While
  nothing is attached the worker keeps the last 100 and replays them on the next
  attach, and the Workers list shows that there are unread messages.
- **Version skew**: a protocol mismatch already fails with `version_mismatch`; the
  Desktop also shows a soft warning when the worker's FlightDeck version differs from
  the core version the Desktop was built with. The fix in v1 is `flightdeck update`
  then `flightdeck worker restart` in the worker shell.

### 4.6 Observability (W29)

- Log: `~/.flightdeck/logs/worker.log`, rotated at 10 MB, 5 files kept. The
  supervisor's own lines (spawns, exits, backoff, give-up) go to the same log.
- `worker status --json` is a stable, documented shape for scripts.
- Health and host messages as in §4.5.

### 4.7 Builds (W2, W28)

The `flightdeck` release must ship `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`, `x86_64-apple-darwin` and `aarch64-apple-darwin`.
The first, third and fourth exist in `dist-workspace.toml` today;
`aarch64-unknown-linux-gnu` is new.

## 5. Protocol changes

All additive to the web protocol, as **v7**. A v7 Desktop must keep working
against v6 TUI hosts (it simply sees no worker), and the worker refuses clients
older than v7. Per `web-protocol-v6-release-coupling`, the CLI and Desktop
releases must both ship v7.

- `Snapshot.host_kind`: `tui | desktop | worker`.
- A connection-kind field on the WebSocket hello: `window` (today's behaviour) or
  `background` (§4.3).
- A `TakenOver { by }` close sent to the displaced window.
- `ResizeTerminal { terminal_id, cols, rows }`, honoured only by worker hosts.
- The worker shell as a terminal id outside any project.
- Project operations: `BrowseDir`, `OpenProject`, `ScanProjects`, `CloneProject`
  with progress and result frames.
- A `Health` delta, and host messages with replay on attach.

## 6. Security model

- **Pairing a Desktop with a worker gives it the worker account's full power**: a
  shell (W15) and the whole home directory (W16). The docs must say so plainly.
- The outer boundary is the tailnet. The worker listens nowhere else (W8), so
  Tailscale ACLs decide who can even reach the port.
- Codes are minted only on demand, over a control socket that only the account
  itself can reach, so getting a code already requires a shell on the box.
- Tokens are long-lived, one per Desktop, listed by `worker devices` and revoked by
  `worker revoke`, which closes live connections at once.
- No credentials cross the wire; no TLS is needed because nothing leaves WireGuard.

## 7. Open decisions

| # | Question | Notes |
| --- | --- | --- |
| O1 | Add a manual pass on a real **Linux** VM to v1's definition of done? | W2 ships Linux, but W30 only verifies macOS by hand. Daemonizing, PTYs and Tailscale binding on Linux would ship untested by hand. Recommended: yes. |
| O2 | Is the configuration manager from the Desktop **worker-only**, or does `remote-control-96y4` ship for every host? | W23 says worker-only, but the feature already exists as an open issue aimed at TUI hosts; gating it off them is extra work for no gain. |
| O3 | **Claude Code login on a macOS worker**: does an agent started by a daemon launched from an SSH session read the credentials `claude login` stored? | Claude Code keeps its login in the macOS Keychain, and a process outside the GUI login session often cannot reach the login keychain. A spike must answer this before the macOS acceptance pass; if it fails, the docs or the launch path must change (e.g. an API key in the environment). |
| O4 | Is a **4-digit, 120 s** bootstrap code strong enough when pairing grants a shell? | Mitigated by the tailnet boundary and by codes needing a shell on the box to mint. Options: keep it; longer codes for workers; rate-limit `/auth/exchange`. |

## 8. Definition of done (W30)

- **Headless integration tests in CI**, driving a worker host through the
  web-client harness (`tests/web_server.rs`): pair with a CLI-minted code, open and
  clone a project, spawn an agent, resize from the client, a second window taking
  over, a background link receiving a status edge and no terminal bytes, the worker
  shell, revoke closing live connections, and resume after a restart.
- **A manual pass on a real Mac**: install, `worker start`, pair the Desktop over
  Tailscale, log in to Claude Code in the worker shell, clone a repo, run two
  agents, get a notification with the window closed, take over from a second
  Desktop, kill the worker and see it come back, `worker restart` and see the agents
  resume.
- **A docs page**, "Run a FlightDeck worker": install, the Tailscale prerequisite,
  pairing, logging in to the agent CLIs, the security model (§6), status and logs.
- O1 may add a Linux pass.

## 9. After v1

- Update from the Desktop (W20): one click, self-update, supervisor restart, agents resume.
- Phone pushes from a worker through the push-only relay.
- Start on boot (`worker enable` writing a systemd user unit / launchd agent).
- A TUI attaching to a running worker as a client.

## 10. Issue map

Children of epic `remote-control-qlyd`. "After" lists blocking dependencies.

| Issue | Work | After |
| --- | --- | --- |
| `.1` | Decide O1: Linux manual pass | — |
| `.2` | Decide O2: config manager worker-only or all hosts | — |
| `.3` | Spike O3: Claude Code keychain on a macOS worker | — |
| `.4` | Decide O4: bootstrap code strength | — |
| `.5` | Headless `AppHost` loop, `worker run`, local host lock | — |
| `.6` | Supervisor, daemonizing, start/stop/restart/status | `.5` |
| `.7` | Tailscale-only bind, retry and rebind | `.5` |
| `.8` | `worker pair` / `devices` / `revoke` | `.6`, `.7` |
| `.9` | Web protocol v7 | `.5` |
| `.10` | One attached window, takeover, background links | `.9` |
| `.11` | PTY geometry from the Desktop | `.9` |
| `.12` | Worker shell tab | `.9` |
| `.13` | Open path, scan `projects_root` | `.9` |
| `.14` | Clone from URL | `.13` |
| `.15` | Configuration manager from the Desktop | `.2`, `.9` |
| `.16` | Desktop Workers section, background links, notifications | `.10`, `.17` |
| `.17` | Logs, `worker status --json`, health, host messages | `.6`, `.9` |
| `.18` | aarch64 Linux release target | — |
| `.19` | Headless integration tests | `.6`, `.8`, `.10`–`.14` |
| `.20` | Manual pass on a real Mac | `.3`, `.7`, `.15`, `.16`, `.19` |
| `.21` | Docs page | `.3`, `.8` |
| `.22` | Follow-up: update from the Desktop | `.6`, `.16` |
