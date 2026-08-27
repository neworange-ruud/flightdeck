# FlightDeck Web — Requirements & Decisions

**Status:** requirements refinement (in progress)
**Epic:** `remote-control-n81` · **This document:** `remote-control-qyg`
**Design:** Claude Design project `ba5af64b-7f47-4d58-a88e-6573ee172ac0`
("Web interface design briefing"), file `FlightDeck Web.dc.html` — turn 1
delivered artboards 1a–1h.
**Design brief:** `specs/WEBAPP_DESIGN_BRIEFING.md` (governs visual/UX intent).

---

## 1. What we are building

A web server **embedded in the FlightDeck desktop binary** that serves a
full-fidelity browser control surface for the *running* FlightDeck instance. The
browser is a genuine remote control: it sees every project, session, status and
terminal the TUI sees, and drives the same code paths.

This is **not** the phone companion. `specs/MOBILE_REMOTE_BRIEFING.md` describes a
curated, read-mostly, relay-connected iOS app with cleaned transcripts. This is
the opposite: raw terminals, full command surface, keyboard-first. The two share
almost nothing but vocabulary, by deliberate decision (see D12).

### Non-goals

- Replacing the TUI. The desktop remains primary; the browser mirrors it.
- Reachability from anywhere without user setup (see D1).
- Multi-user collaboration. One operator, several surfaces (see D14).
- A light theme. The design argues dark-only and we accept it: the main pane is a
  VT100 surface rendering ANSI colors the agent chose against a dark ground.

---

## 2. Decision log

Every decision below was taken deliberately during refinement, with its cost
stated. Where a decision has a known cost, that cost is recorded — not hidden.

### D1 — Reachability: embedded server only

FlightDeck listens on a local port. The browser connects directly over
loopback or LAN. **There is no relay involvement and no E2E crypto in the
browser.**

Reaching it from outside the network is explicitly the user's problem, solved
with Tailscale, `ssh -L`, or a Cloudflare tunnel. Documentation carries that
burden.

> **Cost accepted:** "control it from the train" requires third-party setup.
> A relay-tunnelled transport is not ruled out later, but is out of scope here.

### D2 — Terminal model: raw PTY bytes, xterm.js parses

The browser receives the **actual PTY byte stream** and runs a real VT parser
(xterm.js). Full ANSI fidelity, native scrollback, native text selection and
native copy come for free, and the desktop's own `vt100` parse is untouched.

A **per-terminal replay ring buffer** on the host lets a joining or reconnecting
viewer paint history. Bounded and configurable.

> **Cost accepted:** ring-buffer memory per live terminal.

### D3 — View state: shared; the browser mirrors the desktop

One selected project / session / terminal for the whole instance. Clicking a
session in the browser moves the desktop's selection too. This reuses the
existing single-selection app state and is what "remote control" means.

> **Cost accepted:** two people cannot look at different sessions of one instance.

### D4 — PTY geometry: the desktop always owns it

The PTY stays sized to the desktop TUI's pane. The browser scales the fixed grid
to its viewport with CSS.

This **revises** an earlier preference for last-active-viewer ownership, because
of a concrete finding in the code: `sync_selected_tab_sizes` calls
`resize_if_changed` (`src/lib.rs:5389`) **every frame** for the selected tab, so
any viewer-set geometry would be reverted within one frame unless the render path
learned to draw a PTY grid that is not its own pane size. Desktop-owns keeps that
invariant exactly as it is, adds no state, and eliminates SIGWINCH churn.

**Turn 2 refines how the browser presents that fixed grid: it letterboxes, it
does not scale.** The host's grid renders at its natural size, centred on the
terminal ground, leftover margin left dark, with a `120×34 · host owns geometry`
chip in the git bar. The designer's argument is accepted: upscaling a bitmap grid
would turn crisp type — the one thing a browser is genuinely better at than a
terminal — into the one thing it does worst.

Implementation notes:
- xterm.js must be constructed with the host's `cols`/`rows` and **must not** use
  `FitAddon` — the viewport is letterboxed, not fitted, and never scaled.
- The geometry chip is not decoration. It is the honest explanation for why a
  large browser window has dark margins.

> **Cost accepted:** a large browser window under-uses its space.
> **Precedent that does not apply:** `src/lib.rs:2637` sizes a shell the *phone*
> opened to the phone's geometry — but that is a child terminal created for the
> phone, not a shared one.

### D5 — Auth: bearer token + QR, loopback by default

- A palette command starts the server, mints a random token, and shows a QR plus
  a short code, reusing the existing pairing overlay's visual language.
- **Binds `127.0.0.1` by default.** Binding `0.0.0.0` is an explicit opt-in that
  shows a warning.
- Token persists in `~/.flightdeck`, is revocable, and rotates on one command.

> **Tension to resolve (see Q1):** a QR encoding a loopback URL is useless from
> another device. The QR is only meaningful once bound to a routable address.

### D6 — Server runtime: tokio + axum, and the relay client moves onto it

A dedicated thread owns a tokio runtime running axum — the same stack
`remote/relay` already uses, so its patterns transfer directly. The TUI event
loop stays synchronous and talks to the server over channels.

Additionally, the blocking `tungstenite` relay client in `src/remote/client.rs`
is **retired** and re-implemented on `tokio-tungstenite` on the same runtime, so
both remote transports share one runtime and one set of idioms.

> **Cost accepted:** tokio/hyper/axum enter the desktop dependency graph, and a
> deliberate "no async runtime in the TUI binary" property is given up. See D7
> for the sequencing risk, which is the larger cost.

### D7 — Sequencing: migrate the relay client first, then fix the P0s on tokio

Order of work:

1. Port `src/remote/client.rs` to tokio / `tokio-tungstenite`.
2. Fix `remote-control-5qu`, `remote-control-zv3`, `remote-control-aew` in their
   new async home.
3. Build the web server on the shared runtime.

Rationale: each bug is fixed once, in its final home, rather than twice.

> **Cost accepted, and it is significant.** Three open P0 bugs — including `5qu`,
> where the iOS app silently stops receiving until re-paired — now wait on a
> transport rewrite landing first. Milestone 1 of the web interface waits behind
> that. A P0 regression introduced by the port is also harder to attribute.
>
> **Interaction to watch:** `remote-control-2jy` (wss on Windows via native-tls /
> SChannel) is in progress in exactly this code. A tokio port changes the
> per-platform TLS wiring, so 2jy and the port must be reconciled, not merged
> blindly.

### D8 — Milestone 1 scope: observation + terminal input

M1 serves the app, authenticates, shows projects / sessions / status / git bar
live, streams terminals to a real xterm.js, **and accepts keystrokes into the
focused terminal**. That last part is what makes it worth shipping: it lets you
unblock a waiting agent from a browser.

Out of M1: command palette, dialogs, git commands, config manager, split-view
toggling from the browser, destructive operations.

### D9 — Frontend: a new `webui/` Vite + TS SPA, embedded in the binary

- New top-level `webui/`: Vite, TypeScript, xterm.js. No framework beyond what
  the design needs.
- Built to static assets and baked into the binary with `rust-embed`, so a
  release stays a single file and the server never resolves paths on disk.
- The existing Next.js `web/` marketing site is untouched and unrelated.

> **Cost accepted:** an npm build step enters the Rust release pipeline, and
> release/Homebrew packaging must account for it.

### D10 — Lifecycle: palette command, plus a config opt-in to auto-start

- `Start Web Interface` / `Stop Web Interface` palette commands, showing the QR
  overlay on start — mirroring the existing `Pair Phone` flow.
- A curated config setting (`[web] enabled`, `port`) so a user who wants it
  always available gets it on every launch. It sits beside the existing
  "FlightDeck Remote" row in the configuration manager.
- The token persists, so bookmarks keep working across restarts.

### D11 — Notifications: in-app activity feed only in M1

A visible list of status transitions inside the app. No permission prompt, no
service worker, no push infrastructure — works identically in every browser on
every platform.

Web Push is not merely deferred but **structurally blocked** under D1: Web Push
requires a publicly reachable sender, and a loopback-bound server with no relay
has none. It realistically waits for a relay-tunnelled transport.

### D12 — Wire protocol: a new web-only protocol in its own module

`src/web/protocol.rs`, versioned, JSON over WebSocket:

```
ServerMsg :  Snapshot | Delta | TermBytes | Ack | Error
ClientMsg :  Attach | Input | Resize | Command
```

Deliberately **not** an extension of `flightdeck-remote-protocol` §8. Those types
are shaped by E2E enveloping and a curated phone view; the browser needs full
fidelity and a single trusted socket, so it needs none of the envelope, sequence
or ack machinery the relay protocol exists to solve. Keeping them separate means
web work cannot destabilise the phone wire format or the iOS Swift mirror.

> **Refinement needed:** D2's reconnect-and-resume still requires a **per-terminal
> monotonic byte offset** so a returning viewer can resume rather than re-replay.
> That is a byte cursor, not the relay's envelope/ack machinery — but it must be
> in the protocol from v1.
>
> **Cost accepted:** two protocols to keep conceptually aligned; `AgentStatus`
> and git-detail semantics will exist in two places and must not drift.

### D13 — Dialogs: shared, with an origin label

A dialog is app state, so it appears on both surfaces, tagged with who opened it
(`opened from browser · 192.168.2.20` on the desktop). Either surface can confirm
or cancel. No new state.

> **Cost accepted:** the desktop user gets a modal they did not ask for. The
> origin label is therefore load-bearing, not decoration.

M2 concern, but recorded now because D12's protocol must accommodate it.

### D14 — One *controlling* browser, plus read-only observers

The server accepts **one controlling browser at a time**. A second attach is
offered a takeover, and — per turn 2 — **may instead watch read-only**.

This is a revision. D14 originally said "one viewer, full stop"; turn 2's
takeover artboard (2f) makes read-only observation a first-class affordance in
two places: cancelling a takeover leaves a live read-only view, and an evicted
browser can watch rather than fight. Adopted, because **it preserves D14's actual
rationale exactly**. The reason for the restriction was to defer the
interleaved-input problem — and read-only viewers have no input, so they do not
reintroduce it. What it adds is read-only fan-out, which is strictly simpler than
multi-writer arbitration.

So M1 supports: the desktop, one controlling browser, and N observers. The
viewer chip reads `desktop + this tab` — two named seats rather than a counter
implying a crowd (2f). M3's multi-viewer list is the same panel with rows.

> **Cost accepted:** fan-out to N sockets lands in M1 rather than M3. Modest, and
> the alternative was shipping an approved design minus a feature.

### D15 — Testing: Rust integration + SPA unit + Playwright E2E

- `tests/web_server.rs` — real `TcpListener`, real WS client: token
  rejected/accepted, attach, snapshot, byte streaming, input, reconnect+resume,
  takeover.
- `webui/` — `tsc --noEmit` plus `vitest` on the state reducer.
- **Playwright E2E in CI** — launch FlightDeck, open the page, type, assert. The
  only thing that proves xterm.js renders what the PTY emitted.

> **Cost accepted:** a new browser-in-CI job, with the flakiness this repo already
> knows from the iOS suite (`remote-control-7lo`, `ba5`, `7lr`). Budget for
> quarantine/retry policy up front rather than after it bites.

### D16 — Desktop-only actions: shown with a `host only` badge

Not a refinement question — the design already answers it. Artboards 1d and 1f
render `host only` badges on `Open Worktree in File Manager` and
`e edit in $EDITOR`. Such actions remain visible and honest about where their
effect lands rather than being hidden.

`Ctrl-q` (Quit — kills FlightDeck and every agent) is the one that needs more
than a badge from a remote surface; it inherits the two-step confirmation
treatment from artboard 1g.

---

## 3. Architecture

```
┌─ flightdeck (desktop binary) ─────────────────────────────────────┐
│                                                                   │
│  TUI event loop (sync, ratatui)                                   │
│    │  owns AppState, owns PTY geometry (D4)                       │
│    │  vt100 parse for its own render                              │
│    ├── channels ──────────────────────────┐                       │
│    │                                       │                       │
│  PTY sessions ──raw bytes──> replay ring buffers (D2)             │
│                                            │                       │
│  ┌─ std::thread: tokio runtime ────────────┴────────────────────┐ │
│  │   axum:  GET /            -> rust-embed'd SPA (D9)           │ │
│  │          GET /ws          -> web protocol (D12)              │ │
│  │   tokio-tungstenite: relay client (D6, after D7 step 1)      │ │
│  └───────────────────────────────────────────────────────────────┘ │
└───────────────────────────────────────────────────────────────────┘
        │ 127.0.0.1 by default, 0.0.0.0 opt-in (D5)
        ▼
   browser: Vite/TS SPA + xterm.js  (letterboxes the fixed grid, D4)
```

---

## 4. Milestones

| Milestone | Contents |
| --- | --- |
| **M0** | Port the relay client to tokio (D6/D7 step 1); fix `5qu`, `zv3`, `aew` in async code; reconcile with `2jy`. **Prerequisite for M1.** |
| **M1** | Embedded axum server, token auth + access overlay, palette start/stop + config opt-in, web protocol v1, `webui/` SPA against artboards 1a–1c and 2a–2g, live state, raw terminal streaming, terminal input, takeover **plus read-only observers** (D14), activity feed, Rust integration tests + Playwright job. |
| **M2** | Command palette, the dialog family with origin labels, git commands, destructive operations with two-step confirmation, configuration manager, split view (1c–1g). |
| **M3** | Multi-viewer, narrow-viewport/slide-over layout, and — if pursued — a relay-tunnelled transport (which is what unblocks Web Push). |

---

## 5. Design coverage

Delivered in turn 1: 1a main/Terminal mode · 1b main/App mode · 1c split view ×3 ·
1d filtered palette · 1e new-agent dialog (both states) · 1f config manager with
the three origin tags · 1g destructive confirmation (two-step, typed session
name) · 1h stated positions.

Positions the design locked in, which this spec adopts: palette-primary with
`Ctrl-g` as the only chord claimed; `Esc Esc` (within 400ms) or click-outside to
leave terminal focus, with a single `Esc` still passing through to the agent;
dark-only; `● connected 18ms` and a viewer count in the status bar; slide-over
sidebar below 900px.

Delivered in turn 2 (brief: `specs/WEBAPP_DESIGN_BRIEFING_T2.md`, artboards
vendored at `specs/design/flightdeck-web-turn2.dc.html`): 2a access overlay in
both states · 2b the three browser-side access screens · 2c every connection
state · 2d live/asleep/stale/asleep-and-stale/catching-up · 2e activity feed ·
2f takeover trio · 2g the semantic reference sheet.

### 5.1 Requirements turn 2 introduced

Behaviour the artboards specify that no decision above covered. All of it is
required for M1.

- **Input is queued, never dropped.** While reconnecting, the status bar says
  `keystrokes are being held`; while catching up it says input queues until the
  replay lands. Keystrokes typed against a stale terminal must not be silently
  discarded, and must not be delivered out of order once the link returns.
- **Losing control drains the mode chip.** Any state that costs the user control
  renders `MODE: —`, because the mode is a lie while input is not arriving. The
  connection strip never moves position, and the bar takes the state's frame
  colour — the only chrome in the app that changes hue.
- **The stale terminal is amber, cast plus scanlines plus a frozen clock, with
  the caret removed.** Asleep desaturates cool (`--fd-term-asleep`, lifted to
  5.8:1). The scanlines are what survive both, so asleep-and-stale is legible as
  a third state. `--fd-stale #e0a34a` is a new token.
- **The activity feed is a right-edge slide-over**, opened with `a` in App mode
  or from the unread chip, never a modal. **The host retains the last 200 events
  or 24 hours**, whichever is smaller, so a fresh tab opens on history rather
  than silence. The unread chip has three tiers following the existing project-dot
  precedence — attention beats finished beats quiet — and takes the colour of the
  most urgent unread event.
- **Feed rows are honest about shared selection.** Clicking an entry selects that
  session on the desktop too (D3); the row says so on hover
  (`jump · also moves the desktop`).
- **Unknown stays unknown.** An agent with no lifecycle hooks renders `○` and
  `unknown → unknown · Codex CLI reports no lifecycle` rather than a guess,
  honouring the original brief's requirement for a credible "we don't know".
- **Read-only observation is a real mode** (D14), reachable from both the
  arriving and the evicted browser.
- **The type scale is four sizes**, not turn 1's seven: 11 meta / 12.5 body / 14
  title / 30 for the pairing code, which is the only place the app shouts.
- **The dim tier is lifted.** `--fd-text-quiet` (4.8:1) carries anything a user
  could act on wrongly, including `no-upstream` and `git: ?`.
  `--fd-text-decor` (2.9:1) is decoration only, under a rule worth quoting into
  code review: *if deleting it would lose a fact, it cannot be this colour.*

**Not yet designed — needed before the milestone that uses them.** Turn 3 covers
these (`remote-control-v4s`):

| Gap | Needed by | Brief item |
| --- | --- | --- |
| Narrow viewport main screen — the below-900px slide-over 1h asserts but does not draw | M3 | §11.9 |
| Help overlay, git-status overlay | M2 | §6.1 |

**Nothing M1 needs is undesigned.** Every gap the turn-2 brief raised was
delivered: the access screens, the connection states, the stale treatment, the
takeover trio, the activity feed and the reference sheet. M1 can be built
end-to-end against `specs/design/flightdeck-web-turn2.dc.html` without further
design input.

---

## 6. Open questions — provisional answers

**Every question below has a provisional answer, and implementation should build
it as written.** They are recorded as questions because they were not settled by
the interview, not because work should wait for them. Treat each as decided
unless you have a concrete reason to differ — in which case change it here, in
the same commit as the code, and say why.

This exists so that long unattended stretches of implementation are never blocked
on a conversation. A provisional answer that turns out wrong is cheap; a stalled
milestone is not.

### Q1 — The QR is meaningless on loopback · **answered**

Resolved by `specs/WEBAPP_DESIGN_BRIEFING_T2.md` §3. The access overlay has two
states and the transition between them is part of the flow:

- **Local only (default).** Primary action is **Open in browser** — FlightDeck
  launches the default browser already authenticated. No QR, no code. A copyable
  URL for a second browser, and a door to network access that states its
  consequence.
- **Network enabled.** The QR now earns its place, encoding
  `http://<lan-ip>:<port>/#<code>`, alongside the code in large type, an explicit
  address choice, and the warning.

**Delivered in turn 2, artboard 2a**, with three additions beyond the proposal:

1. **The QR encodes the code**, deliberately. The designer's reasoning: a
   URL-only QR trades a real and frequent cost — typing four digits on a phone
   at the moment the user is trying to walk away from their desk — against a
   hazard we cannot detect and the user can. Mitigated by `r` to hide the code
   and QR at any time, and by hiding them by default while a screen-recording
   API reports active capture (see **Q7** — that second mitigation may not be
   portable).
2. **The address is chosen, not guessed.** The overlay enumerates interfaces
   (`en0` wifi, `bridge100` vm bridge, `tailscale0` your own tunnel) with a
   one-line description each, and publishes the selected one. This is new work:
   interface enumeration must behave identically on macOS, Linux and Windows.
3. **Code entry is rate-limited** — "3 attempts left before this address is
   rate-limited for 60s" (2b). Per-address, not global.

### Q2 — Replay ring buffer size · **provisional**

**256 KiB per terminal**, configurable as `[web] replay_bytes`. Bytes, not lines,
because the buffer sits in front of a VT parser and lines are not yet a concept.
Discard oldest first. On attach, replay the whole buffer; a viewer that joins
mid-escape-sequence gets one imperfect repaint, which xterm.js recovers from on
the next full redraw.

### Q3 — Reconnect semantics · **provisional**

**Resume from a byte cursor.** Every terminal frame carries a per-terminal
monotonic byte offset, and this is in protocol v1 (see the D12 refinement). On
reconnect the viewer sends its last offset; the host resumes from there if the
offset is still inside the ring buffer, and otherwise sends a full replay with an
explicit `truncated: true` so the viewer can say it missed output rather than
pretending continuity. The viewer shows a catching-up state while draining.

### Q4 — Token transport · **provisional**

Two separate credentials, per `specs/WEBAPP_DESIGN_BRIEFING_T2.md` §3:

```
short code    ~120s bootstrap, shown on the desktop with a countdown
                   │  exchanged once
                   ▼
HttpOnly cookie    long-lived, per browser, revocable from the desktop
```

The bootstrap code arrives in the **URL fragment** on first visit so it never
reaches the server in a request line or a log, is exchanged for the cookie, and
the fragment is stripped from history. Bookmarks then work with no credential
visible anywhere.

### Q5 — The host quits while a browser is attached · **provisional**

**The server drains rather than dropping.** Before the listener closes, it sends
an explicit `Shutdown { reason }` frame, so the browser can distinguish a
deliberate quit from a network failure — which is the whole point, because
"reconnecting…" against a host that is gone is a lie that wastes the user's time.

On receiving it the browser enters a **terminal state and stops retrying**,
naming the reason. If the quit was initiated from *this* browser, it shows an
acknowledgement of its own action instead of a failure.

### Q6 — Playwright flake policy · **provisional**

Agreed before the job lands, per D15's accepted cost:

- `retries: 2` in CI, `retries: 0` locally, so flake is visible when developing.
- A test that fails twice consecutively on `main` is **quarantined** the same
  working day — marked `fixme` with a filed bug — rather than left to erode trust
  in the suite.
- The job is **non-blocking for its first two weeks**, then becomes required.
  This repo has three open iOS flake issues (`remote-control-7lo`, `ba5`, `7lr`);
  the policy exists so the browser suite does not become a fourth.

### Q7 — Is "hide the code while screen recording" implementable? · **provisional**

Turn 2 specifies that the code and QR render hidden by default "when a
screen-recording API is active". This is on the **desktop overlay**, so detection
would be a host-side OS query, not a browser one — and there is no portable way
to ask it. macOS exposes partial signals, Linux has no common answer across
compositors, and Windows differs again. FlightDeck holds a hard cross-platform
parity requirement (`.agents/skills/flightdeck-cross-platform-parity`).

**Provisional answer: do not gate the design on detection.** Ship the manual
control, which works everywhere and is what the user actually controls:

- The code and QR are **hidden behind a reveal** by default on every platform.
- `r` toggles them, as the artboard specifies.
- Where a platform *does* offer a trustworthy capture signal, use it to keep them
  hidden and say why. Never use its *absence* to imply the screen is private.

This keeps the security posture honest — never claiming a protection we cannot
deliver — and keeps behaviour identical across platforms, which parity requires.

## 7. Reference

- `specs/WEBAPP_DESIGN_BRIEFING.md` — the design brief this implements.
- `specs/WEBAPP_DESIGN_BRIEFING_T2.md` — turn 2: access/QR (resolves Q1),
  connection and staleness states, takeover, activity feed, reference sheet.
- `specs/REMOTE_PROTOCOL.md` §8–§9 — the phone protocol we are deliberately *not*
  extending (D12).
- `specs/SPECS.md` §19–§24 — terminal model, layout, git panel, interaction model,
  keyboard modes, status detection.
- `src/lib.rs:5374-5395` — `resize_sessions` / `resize_if_changed`, the per-frame
  geometry invariant behind D4.
- `src/lib.rs:2637` — the phone-opened-shell geometry precedent that does not
  generalise.
- `src/remote/client.rs` — the blocking relay client retired by D6/D7.
