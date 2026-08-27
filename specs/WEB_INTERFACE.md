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

Implementation notes:
- xterm.js must be constructed with the host's `cols`/`rows` and **must not** use
  `FitAddon` — it is a scaled viewport, not a fitted one.
- Scaling is crisp at integer-ish ratios and soft off them; the browser should
  prefer the nearest clean ratio over filling every pixel.

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

### D14 — Concurrent input: a single browser viewer in M1

The server accepts one browser at a time. A second attach is refused with an
`already in use — take over?` prompt. The design's viewer count stays honest at
2 (the desktop plus at most one browser), and the whole class of interleaved-input
questions is deferred cheaply.

> **Cost accepted:** the design presents viewers as first-class; M1 under-delivers
> on that until multi-viewer arrives.

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
   browser: Vite/TS SPA + xterm.js  (scales the fixed grid, D4)
```

---

## 4. Milestones

| Milestone | Contents |
| --- | --- |
| **M0** | Port the relay client to tokio (D6/D7 step 1); fix `5qu`, `zv3`, `aew` in async code; reconcile with `2jy`. **Prerequisite for M1.** |
| **M1** | Embedded axum server, token auth + QR overlay, palette start/stop + config opt-in, web protocol v1, `webui/` SPA against artboards 1a/1b, live state, raw terminal streaming, terminal input, single-viewer takeover, activity feed, Rust integration tests + Playwright job. |
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

**Not yet designed — needed before the milestone that uses them.** The turn-2
brief covering the M1 gaps is `specs/WEBAPP_DESIGN_BRIEFING_T2.md`
(`remote-control-l83`); turn 3 covers the rest (`remote-control-v4s`):

| Gap | Needed by | Brief item |
| --- | --- | --- |
| Pairing / token-entry screen | M1 | §11.8 |
| Disconnected / reconnecting / stale-terminal states | M1 | §8, §11.8 |
| Single-viewer takeover prompt | M1 | new (D14) |
| Activity feed | M1 | new (D11) |
| Narrow viewport main screen | M3 | §11.9 |
| Help overlay, git-status overlay | M2 | §6.1 |
| Colour/type reference sheet | M1 | §11.10 |

---

## 6. Open questions

- **Q1 — The QR is useless on loopback.** D5 binds `127.0.0.1` by default but
  shows a QR. Either the QR appears only once bound to a routable address, or the
  overlay offers "enable LAN access" inline as part of the flow. Needs a decision
  and a design.
- **Q2 — Replay ring buffer size.** Bytes per terminal, and whether it is
  configurable. Proposal: 256 KiB per terminal, `[web] replay_bytes`.
- **Q3 — Reconnect semantics.** Confirm resume-from-byte-cursor (D12 refinement)
  versus full re-replay, and what the viewer shows while catching up.
- **Q4 — Token transport.** Bookmarkability (D10) implies the token survives in
  the URL or a cookie. Proposal: token arrives in the URL fragment on first
  visit, is exchanged for an `HttpOnly` cookie, and the fragment is stripped.
- **Q5 — Behaviour when the desktop quits** while a browser is attached: what the
  browser shows, and whether the server drains or drops.
- **Q6 — Playwright flake policy** — retry/quarantine rules agreed before the job
  lands, per D15's stated cost.

---

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
