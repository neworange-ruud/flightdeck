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
of a concrete finding in the code: `sync_terminal_sizes` calls
`resize_if_changed` (`src/lib.rs:5387`) **every frame** for the selected tab, so
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

**Implemented in `remote-control-ll5.3`** — see R8 in §6.5 for how it reuses the
TUI's own prompt state, what happens when a second dialog arrives while one is
open, and the two rows that still refuse.

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

### Q6 — Playwright flake policy · **implemented, see R5/R6**

Agreed before the job lands, per D15's accepted cost:

- `retries: 2` in CI, `retries: 0` locally, so flake is visible when developing.
- A test that fails twice consecutively on `main` is **quarantined** the same
  working day — marked `fixme` with a filed bug — rather than left to erode trust
  in the suite.
- The job is **non-blocking for its first two weeks**, then becomes required.
  This repo has three open iOS flake issues (`remote-control-7lo`, `ba5`, `7lr`);
  the policy exists so the browser suite does not become a fourth.

Landed as written on 2026-08-27: non-blocking from that date, **required from
2026-09-10**. §6.5 R6 records where each half lives.

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

## 6.5 Refinements made during implementation

Kept here because §6's rule cuts both ways: a provisional answer that turns out
wrong is cheap, but a change made in code and *not* written down is how a spec
starts lying. Each entry below was decided while building M1, in the commit that
made it.

### R1 — `ActivityEvent` carries a `reason` string

Turn 2 requires feed rows to read `asked a question`, `agent exited (code 1)`,
`finished, 18 files touched` (artboard 2e, §5.1). Protocol v1 as first written
carried `from`, `to`, `manual` and `tier` but no reason, and a browser cannot
reconstruct one — `finished, 18 files touched` is not derivable from a status
pair at all. `reason` is therefore a wire field, `#[serde(default)]` to the empty
string.

**It is never padded with a guess.** An empty reason renders as no reason.
§5.1's "unknown stays unknown" governs the reason exactly as it governs the
statuses, and the host-side store enforces it by taking the reason from the
caller rather than deriving one.

### R2 — the browser's status/git model is wider than the wire's · **resolved: mapped in the adapter**

`webui/src/state/model.ts` models per-session git as a three-way union —
`known` / `no_upstream` / `unknown` — and carries a session-level
`lifecycleNote`. Protocol v1 encodes the same information as a `has_upstream`
bool plus a `collected` bool, which admits an impossible fourth state, and it
has nowhere to say "Codex CLI reports no lifecycle" as data.

`no-upstream` and `git: ?` are load-bearing facts (2g names both, and §5.1's
lifted dim tier exists for them), so the browser must not infer either.

**Resolved when the live socket was wired: mapped host-side, in
`src/web/stream/host_state.rs`.** The wire is unchanged; `git_bar()` is the one
function that emits a `GitBar`, and the module doc there carries the reasoning
in full. In short:

1. **The lifecycle half needed nothing.** Protocol v1 already carries
   `SessionView::lifecycle_reporting` alongside `agent_display_name` — a fact
   plus a name, from which the browser writes `unknown → unknown · Codex CLI
   reports no lifecycle`. The host sends the fact, the design owns the wording.
   R2's note was written before that field existed. The host derives the flag
   from `agents::setup::status_backend`, i.e. from the *same* function that
   decides whether to attach a lifecycle integration at launch, so the flag
   cannot drift from the behaviour it describes.
2. **The two git bools are a faithful encoding of the three-way union**, because
   both are derived from one `Option<&WorktreeStatus>`: `has_upstream: true` is
   unreachable without `collected: true`. Read as a union —
   `!collected` → `unknown` (`git: ?`); `collected && !has_upstream` →
   `no_upstream`; `collected && has_upstream` → `known`. Nothing is inferred
   from a missing field, from zeroed counts, or from an empty branch:
   `collected: false` is sent deliberately, and `is_git_unknown()` is the single
   predicate that reads it.

**Why not widen the wire.** The encoding has two peers and the decoder lives in
`webui/`, which protocol v1 is already built against. A wire change the SPA is
not changed to match in the same commit does not make the model narrower — it
makes the two halves disagree, which is a worse failure than a bool pair with a
documented reading and a test.

The test that keeps this honest is
`git_bar_never_claims_an_upstream_it_has_not_looked_for`
(`src/web/stream/tests.rs`), which asserts the impossible fourth state never
escapes the adapter. If a later turn does widen `GitBar` to a tagged union —
changing the SPA in the same change — `git_bar()` is the only host-side place
that moves, and that test is rewritten rather than deleted.

### R3 — the letterbox and palette invariants are enforced by tests

D4's "no `FitAddon`, never scale" and 2g's four type sizes and named tokens are
guarded by `webui/src/ui/tokens.guard.test.ts`, which fails the suite on a hex
or `rgb()` literal outside `tokens.css`, on a `font-size` that is not a
`var(--fd-t-*)`, on an inline style write, and on `FitAddon` / `addon-fit` /
`transform: scale` appearing anywhere. Both guards were verified to fail when
deliberately violated.

This is recorded as a decision because it constrains future work: a value the
artboards use that 2g never named is expressed as a `color-mix()` of named
tokens and reported as a token-turn candidate, rather than reintroduced as a
literal. Current candidates: `--fd-wash-select`, `--fd-glow-focus`,
`--fd-glow-elsewhere`, `--fd-sidebar-ground`, `--fd-chip-shell`.

### R4 — D4's per-frame invariant is `sync_terminal_sizes`

D4 and §7 previously cited `sync_selected_tab_sizes` and `resize_sessions`.
Neither exists. The function is `sync_terminal_sizes` (`src/lib.rs:5405`),
calling `resize_if_changed` (`src/lib.rs:5387`). Corrected in both places; the
invariant is unchanged and still load-bearing.

### R5 — the browser's live socket, and the one test-only seam it needed

The Playwright job (D15) is the last M1 task, and writing it surfaced two gaps
that had to be closed before it could assert anything true.

**1. The SPA had no transport.** `remote-control-hgqy` landed the host half —
the tee, the ring, the fan-out, the input path — and `webui/src/main.ts` was
still painting `state/fixture.ts`. A browser-in-CI test against a fixture proves
nothing, so the socket landed here: `webui/src/wire/frames.ts` (protocol v1 as
TypeScript), `wire/adapt.ts` (wire → `state/model.ts`, per R2's decision, on the
browser side of the pair), `wire/socket.ts` (attach, snapshot, `term_bytes`,
`input`, acks, seats, shutdown, reconnect with backoff). Three properties are
load-bearing and stated where they live: **bytes never enter the store** (they go
from the frame to xterm.js, so a re-render cannot repaint or lose terminal
output); **bytes stay bytes** (base64 → `Uint8Array` → `term.write`, so a UTF-8
sequence split across two frames is xterm's decoder's problem, not a corrupted
string); and **keystrokes come from xterm's own `onData`** rather than a
hand-written key→bytes encoder, which is the part that always gets Home, Alt and
the arrow keys wrong.

Deltas that change facts the store only accepts wholesale (status, git, a new
session) are answered with the host's own `request_snapshot` command, coalesced.
That is a deliberate simplification of M1, not a permanent shape: it is honest
(the browser never invents the new state) and it is one command away from being
replaced by per-delta reducer actions when M2 needs them.

**2. Authentication had no way in for a machine.** The persistent token is
stored as a SHA-256 hash, so no usable credential can be read off disk, and the
bootstrap code is random and rendered only into a TUI overlay on a PTY. The seam
is `CredentialStore::mint_fixed_bootstrap_code`, driven by
`FLIGHTDECK_WEB_TEST_CODE` — the same shape as the `FLIGHTDECK_REMOTE_AUTOPAIR`
hook the phone harness uses, and bounded the same way:

- It mints a code with **known digits**. It does not mint a token, accept one,
  or skip a check. The browser still exchanges it at the real
  `POST /auth/exchange`, and the code still expires after 120s, is still single
  use, and is still subject to both rate limiters (asserted by three tests in
  `src/web/credentials/tests.rs`).
- The method and its caller are **`#[cfg(debug_assertions)]`**, so
  `cargo build --release` — how every shipped binary is produced — does not
  compile them at all. There is no runtime flag, no config key and no cargo
  feature that could turn it on in a release build.

**What the job found on its first run**, which is the argument for its
existence: `.fd-access` and `.fd-takeover` are toggled with the `hidden`
property, and their `display: flex` beat the UA stylesheet's
`[hidden] { display: none }`. Every authenticated tab therefore kept a
full-frame scrim over the app that dimmed it and swallowed every click,
including into the terminal. No unit test in either language could have seen it;
the browser found it in one click.

### R6 — Q6's flake policy, as registered

Implemented exactly as Q6 fixed it, and recorded here because the dates matter:

- `retries: 2` in CI, `retries: 0` locally (`webui/playwright.config.ts`, from
  `$CI`). `workers: 1` and `fullyParallel: false` are a *correctness*
  requirement, not tuning: D14 gives out one controlling seat, and two workers
  would fight over it.
- Quarantine is `test.fixme` naming a filed `bd` issue and an owner, within the
  same working day as the second consecutive failure on `main`. The exact form
  is at the bottom of `webui/e2e/chain.spec.ts`. There are no quarantined tests
  today.
- **The job is registered non-blocking on 2026-08-27 and becomes required on
  2026-09-10** — `continue-on-error: true` on the `e2e` job in
  `.github/workflows/webui.yml`, with that date in the job's own comment and the
  removal step spelled out. Raising `retries` to quiet the job is ruled out by
  Q6; the repository's three open iOS flake issues are why.

The suite is deliberately three tests, not ten. `tests/web_server.rs` already
drives 35 tests over real sockets, so the browser suite asserts only what
nothing else can reach: that the page came from the embedded assets, that
authentication went through the real exchange endpoint, that **text the PTY
really emitted is in the DOM xterm.js rendered**, that a keystroke typed in the
browser reached the PTY (proved twice — the agent's echo renders, and the agent's
own on-disk reply log records the line), and that the rendered grid is the host's
`cols`×`rows` with the geometry chip saying so. The keystroke test was checked
for vacuity by disabling the input path and watching it fail.

### R7 — the command inventory is host state on the wire, and one table owns it

M1 shipped six command names and the SPA had to invent two more
(`open_worktree_in_file_manager`, `edit_in_editor`) so D16's badges could be real
UI. Both halves of that are now fixed by one decision: **the host owns the
command inventory and sends it**, in `Snapshot::commands`.

`src/web/commands.rs` holds a single `INVENTORY` table — wire name, label, group,
`host_only`, annotation, route — and it is the only source for all three
consumers: `src/web/server.rs` refuses any name not in it, `src/lib.rs` routes
what is, and the snapshot field is built from it. One row on the wire is

```
{ id, label, group, run: { name, args? }, host_only?, annotation?, target?, refusal? }
```

matching `webui/src/state/commands.ts`'s `PaletteCommand` field for field (modulo
the snake_case → camelCase rename the adapter already does for every wire type),
plus two additions: `target` (`project` / `session` / `terminal` /
`unread_activity`) marks a *template* row the browser expands into one row per
target, filling `run.args`; `refusal`, when present, is the sentence the host
will answer with if the row is sent, so a row can be shown honestly rather than
hidden. It rides on the snapshot rather than on `HostState` because it is static
for the life of the build — there is no change for a `Delta` to describe.

**Dispatch goes through the TUI's own palette path, not a copy of it.** A row's
route carries the very `PaletteAction` the desktop's palette hands to
`run_palette_action` on Enter, and `run_web_command` passes it into that same
function; the ack is derived from what the dispatch reported (`Ui::web_outcome`),
so a guard's refusal reaches the browser in the guard's own words. §1's "drives
the same code paths" is therefore structural: there is no arm anywhere that
performs a command's effect a second time.

**Three consequences worth stating, because they are constraints on later tasks:**

1. *Nothing can become silently unreachable.* `exposure_of` is an exhaustive
   match over `PaletteAction` (and through it `Command`) with no wildcard arm, so
   a new palette action fails to compile until it is classified, and a test walks
   `palette::all_entries()` to prove the name it claims is really in the table —
   labels and groups included, so the two palettes cannot drift apart.
2. *The git-ownership boundary holds by construction (SPECS §5).* A forwarding
   row ignores the frame's `args` entirely — the action and every `confirm` flag
   inside it come from the table — so no frame can smuggle a confirmed
   destructive operation, and the two history-rewriting commands
   (`rebase_worktree`, `pull_base`) are not on a forwarding route at all.
   *(The second clause is superseded by R11: `rebase_worktree` now forwards its
   **unconfirmed** value, which is what SPECS §5.1 sanctions. The first clause is
   what makes that safe, and it is unchanged.)*
3. *Refusals are typed, and the type says who owns the follow-up.* A command
   whose effect lands on the host's machine (D16's two) or must not land
   unconfirmed (`quit`, `stop_web_interface`) is answered `Ack{Rejected}` with the
   reason and **never forwarded**, which is what makes "a bare frame naming quit
   cannot kill the process" a property of the code rather than of a check.
   Everything whose browser-side surface belongs to another M2 task — destructive
   confirmations (ll5.4), git (ll5.5), the configuration manager (ll5.6),
   help/about/git-status (ll5.8); the dialog family (ll5.3) has since landed,
   see R8, and the git family (ll5.5) has since landed, see R11 — is answered
   `ErrorCode::NotSupported` with a reason naming what is missing, rather than
   dispatched into a modal on a screen the browser cannot see. Each of those
   tasks flips its rows' route; nothing else has to move.

### R8 — D13's shared dialog reuses the TUI's prompt state, and two rows still refuse

D13 says "no new state", and the implementation takes that literally: the dialog
on the wire **is** the TUI's own `Ui::prompt` read out.

1. **One dialog store.** `PromptState` (`src/lib.rs`) gained a `DialogId` and a
   `DialogOrigin`; `web_dialog_view` serialises it into `HostState::dialog`. There
   is no browser-side dialog kind, no second store, and no arm that performs a
   dialog's action a second time — a browser's `dialog_confirm` is turned into the
   very `KeyEvent`s `handle_prompt_key` already handles, so `New Agent Session
   Tab` confirmed from a browser runs `begin_new_agent_tab_ex` because a synthetic
   `Enter` reached the same arm a real one does.
2. **The origin is on the render model.** `crate::tui::render::Dialog` gained
   `origin: Option<String>`, drawn between the title and the buttons in
   `--fd-elsewhere`'s TUI equivalent (magenta) — so it is read *before* anything
   is decided. `None` for a desktop-opened dialog is deliberate: the reader is the
   asker, and a line saying so would be the decoration D13 says this is not.
   Covered by `draw_dialog_renders_the_browser_origin_above_the_buttons`.
3. **The origin comes from one place.** `run_web_command` sets
   `Ui::web_dialog_origin` for the duration of one browser dispatch — the same
   idiom as `Ui::web_outcome` — and `start_prompt` reads it. So none of the two
   dozen prompt-opening call sites knows a browser exists, and a dialog cannot be
   published without an origin because there is no other way to open one.

**Two new wire names**, `dialog_confirm` and `dialog_cancel`, both listed in
`INVENTORY` (which is what makes the server's "refuse any name not in the table"
a complete check) and both `requires_control()`, so an observer is told
`read_only` rather than being allowed to answer a question that is not theirs
(D14). `DialogView::body` carries `protocol::DialogBody` — the *shell* artboard
1d describes (input, list, buttons, `confirmable`, `refusal`), not one struct per
dialog kind, because the desktop already renders every prompt from one model and
giving the browser the same model is what stops the two becoming two dialog
systems. Artboard 1e's form is that shell with a list, an input and three keys.

**A browser can only press a key the dialog is showing.** `choice` names a
button by its key label; `text` is ignored by a dialog with no field; `toggle`
needs a `Tab` button to exist. Anything else is refused with a sentence naming
what the dialog *does* offer.

**The `Superseded` policy.** `crate::web::stream::deltas` compares two published
states, so all it can honestly say about a dialog that is gone is
`DialogOutcome::Superseded` — it did not witness a decision. `handle_prompt_key`
does witness one and records it in `Ui::dialog_decisions`; the event loop's
`resolve_dialog_outcomes` upgrades the diff's frame with the real outcome before
sending it. So a browser learns `Confirmed` when the desktop pressed `y`,
`Cancelled` when either surface dismissed it, and `Superseded` only when a dialog
really was replaced without an answer — never a silent swap. A `dialog_confirm`
naming a dialog that has since been replaced is refused, not applied to whatever
is on screen now, which is the mechanism behind "nobody confirms something they
never read".

**Ten of the twelve refused rows now open a dialog:** `open_project`,
`close_project`, `new_agent_session_tab`, `rename_agent_session_tab`,
`close_agent_session_tab`, `close_child_terminal`, `new_agent`, `close_agent`,
`set_manual_status`, `unpair_phone`. Two still refuse, each naming its owner:

* **`abandon_worktree`** — `remote-control-ll5.4`. The dialog is shared once the
  desktop opens it and **cancellable** from a browser; confirming it needs
  artboard 1g's two-step typed-name confirmation. Same shape for the git
  confirmations (`push` / `merge` / `rebase`), which refuse a browser's confirm
  with `GIT_DIALOG_REFUSAL` and remain cancellable (`remote-control-ll5.5`).
  *(The git half is superseded by R11: those three dialogs **are** SPECS §5's
  confirmation, so `remote-control-ll5.5` lifted the gate and
  `GIT_DIALOG_REFUSAL` is gone. `abandon_worktree` is unchanged.)*
  Cancelling is never gated: dismissing a confirmation cannot destroy anything,
  and a shared dialog a remote surface can see but not dismiss would be worse
  than not sharing it.
* **`show_git_status`** — it is **not** one of D13's dialogs. Nothing is being
  asked, so there is nothing to confirm or cancel; it opens a read-only overlay
  and design turn 3 owns what the browser shows instead. It was grouped with the
  dialog family before this task only because it opens a desktop overlay.

**A dialog rides on the snapshot as well as on deltas**, because a dialog is
state: a tab that attaches while one is open paints it from `Snapshot::dialog`
and never saw the `DialogOpened`. On the browser side the local *draft* (1e's
typed branch, the radio position, the `Tab` toggle) survives a re-announcement of
the same dialog, so a coalesced resync mid-typing does not empty the field — and
it is never rendered as accepted state: the confirm is settled by the host's own
`Ack`, and an `applied` `Ack` does **not** close the panel. Only
`Delta::DialogClosed` does, which is what makes "either surface can confirm or
cancel and the other reflects it" one mechanism rather than two.

### R9 — the reload chip's `Enter` is focus-scoped, and the rate-limited screen is written to 2b's rules

`remote-control-l7ya` reported two open questions from artboard `2c —
CONNECTION STATES` and `2b — BROWSER-SIDE ACCESS SCREENS`. `remote-control-ll5.10`
settles both.

**1. Enter on the version-mismatch reload chip is scoped to the chip's own
focus — it is not bound globally.** 2c deliberately draws a version mismatch
as `● connected 21ms` with the mode chip *intact*: nothing about the
connection or about control is wrong, the tab is merely old (`ConnectionStatus`'s
doc comment in `state/types.ts`, and `modeChip` in `ui/statusBar.ts`, both say
so). Suppressing terminal input the way `disconnected` does — which is the
only way to make `Enter` genuinely free — would contradict that position by
turning "nothing is wrong" into "input is not arriving," so it was rejected.
`Enter` therefore keeps its ordinary jobs (a newline in Terminal mode,
focus-the-terminal in App mode — `app.ts`'s keydown handler), and the chip's
printed `Enter` means "Enter while this chip has focus," exactly as a real
`<button>` behaves. Contrast: `r Retry now` (`disconnected`) and `Enter Enter a
code` (`revoked`) *are* bound globally — both in `app.ts`'s keydown handler —
because those two states deliver input nowhere else and the key is genuinely
free.

The chip (`actionButton` in `ui/statusBar.ts`) carries this two ways: a
`keydown` listener on the button itself fires only when the button is the
event's target, so an `Enter` that bubbles up from anywhere else in the frame
never reaches it; and its `title` says "Enter reloads only while this button
is focused" for anyone who tabs to it without already knowing. The visible
text is unchanged — still `Enter` plus 2c's own label — because the artboard
does not license new copy; the scoping lives in the accessible name instead.
Enforced by `reload: Enter is scoped to the chip, not global (ll5.10, §6.5
R9)` in `ui/turn2Screens.test.ts`, which asserts an `Enter` dispatched at the
frame (i.e. not aimed at the chip) fires nothing, and the same key dispatched
at the chip does.

**2. The rate-limited screen's amber tone and missing primary button are
confirmed, not a guess.** 2b draws no rate-limited *panel* — it puts the limit
in the rejected screen's footer instead — so `state/access.ts`'s `rate_limited`
case was always written to 2b's *rules* rather than copied from an artboard:
**amber** (`tone: "stale"`), by the same reasoning 2b gives `revoked` — a
limiter doing its job is not a failure, so red would misreport it as one — and
**no primary button** (`primary: null`), because the host would refuse a
retry it knows cannot succeed, and offering one anyway is exactly the false
claim Q7 rules out elsewhere. Enforced by `rate-limited: amber, no button, and
the host's countdown` in `ui/turn2Screens.test.ts`, already in place before
this task; ll5.10 confirms the ruling rather than changing the code.

### R10 — `finished, N files touched` is bought at the finish edge, not from the cache

R1 above records that the reason string is never padded with a guess, and until
`remote-control-ll5.11` that left artboard 2e's `finished, 18 files touched`
rendering with no clause at all. The count was not knowable at the call site:
the git-status file count lives in `GitStatusCache`, refreshed every
`GIT_REFRESH_EVERY` ticks **for the active project only**, so a number read at
transition time would be stale for the project on screen and simply absent for a
background one.

**The fix is a scoped, one-shot refresh on the finish edge, not a wider periodic
one.** Widening the cache to every open project would run git for every project
every N ticks, forever, to serve a row that appears when a session finishes.
Instead `record_web_transitions` (`src/lib.rs`) — which the event loop already
calls for *every* open project in the same pass that drains PTYs and fires
notifications — returns one `FinishCountRequest` per finished session, and
`spawn_finish_count` runs a single `git status --porcelain` on that one tab's
worktree, off the UI thread (SPECS §21). Nothing periodic changed: the
`GIT_REFRESH_EVERY` block still refreshes `workspace.projects[active]` alone, and
a project whose sessions are not moving asks git nothing.

**Which edges earn a count** is `activity::wants_file_count`: a `finished`-tier
move — derived through the same `StatusBucket`/`ActivityTier` pair the recorded
event's own tier comes from — whose reason is still empty. A manual override, an
exit code and a no-lifecycle agent all already have a better explanation and keep
it.

**The row waits, and is never lost or padded.** `activity::PendingFinishes` holds
the whole transition while git is asked, rather than amending an event already in
the store: `stream::deltas` matches feed entries by id and emits one only for an
id the browser has not seen, so a row published empty and corrected later would
reach an open tab as nothing at all. The row therefore enters the store exactly
once, complete — stamped with the *edge's* `at_ms` (`ActivityStore::record_at`),
not with the moment git answered. Git that fails, and git that has not answered
within `FINISH_COUNT_DEADLINE_MS` (2s), both record the row with the **empty**
reason it had before this existed: slow and broken degrade to the identical
honest row, which is what lets the deadline be short.

### R11 — git runs from the browser, and §5's boundary is restated rather than relaxed

`remote-control-ll5.5`. R7 left four rows on `Route::NotSupported(GIT_REFUSAL)`
— `rebase_worktree`, `push_branch`, `finish_local_merge`, `pull_base` — and R8
left the three git confirmations refusing a browser's confirm with
`GIT_DIALOG_REFUSAL`. Three of the four rows now dispatch, both refusals are
gone, and the fourth is a decision rather than a placeholder.

**What the browser can now do.** `Push Branch` (SPECS §14), `Finish / Local
Merge` (§15) and `Rebase Worktree` (§5.1) are palette rows a browser runs, and
they run through the same `run_palette_action` → `AppState::dispatch` path the
desktop's own palette drives — there is still no arm anywhere that performs a git
effect a second time. The confirmations those commands raise are D13 dialogs, so
a browser now *confirms* as well as cancels them: `browser_may_confirm` has no
git arm left, and `web_dialog_view` publishes `confirmable: true` with no
`refusal`. Nothing on the browser side changed to allow that. `state/commands.ts`
renders whatever the host sends, `state/dialog.ts` has never had a list of dialog
kinds, and the git rows appeared the moment the Rust table flipped.

**Why the confirm had to be lifted along with the row.** Refusing it would have
left the worst of both: a browser able to raise a question — *Rebase
flightdeck/x onto main (base moved 7 commits)? Rewrites history; aborts on
conflict* — that only the other surface could answer. D13 exists so a dialog is
one thing on two screens; a family where one surface can ask and not answer is
that decision half-taken.

**The boundary invariant, restated.** The old test asserted that *no*
browser-reachable dispatching route rewrites history. That was true while every
git row refused, and it would now be false — so it was restated rather than
loosened into a rubber stamp:

> No browser-reachable route may rewrite history **except** through a route
> whose dispatched command is unconfirmed and therefore lands on §5.1's
> confirmation prompt; and no browser-reachable route may create a pull request,
> **ever, with no exception**.

Both halves are enforced by construction, not by a check at the door:

1. **`web::commands::confirmation_of` classifies a command *value*, not a
   command kind** — `Pending` / `Given` / `None` — exhaustively and with no
   wildcard arm, so a new confirmation flag has to say where it stands before
   the crate compiles. `INVENTORY` carries `RebaseWorktree { confirm: false }`,
   and R7's forwarding rule is what makes that unforgeable: a `Command` frame's
   `args` are *never read* by a forwarding row, so the payload reaching
   `AppState::dispatch` is the table's whatever the frame said. §5.1's "the first
   dispatch always returns a confirmation prompt before anything is rewritten" is
   therefore a property of the payload, and
   `a_frame_cannot_smuggle_a_confirmed_rebase` pins it.
2. **The exception cannot quietly grow.** The boundary test collects every
   history-rewriting row it finds and asserts the set is exactly
   `[rebase_worktree]`, so a second one is a spec argument rather than a test
   update. `creates_pull_request` stays asserted absolutely, with no clause.
   Both functions remain exhaustive with no wildcard arm.

**`pull_base` is deliberately not exposed (SPECS §5.2).** It was the judgment
call, and the reasoning is on the record because either answer is defensible and
an unexamined one is not.

*For exposing it:* §5.2 sanctions it as a palette command with a keybinding, it
never touches an Agent Tab's worktree, and it is guarded — base branch must be
checked out, conflicts abort, the folder is left exactly as it was.

*Against, and decisive:* those preconditions bound the **damage**, not the
**surprise**. What makes a browser-reachable rebase acceptable is not that it is
safe but that nobody reaches it without reading a question, and §5.2 says in as
many words that pull-base "is not confirmation-gated". So it has no unconfirmed
variant for the table to carry — `confirmation_of(&PullBase)` is `None` — and it
fails the invariant's exception clause *structurally* rather than by being named
in it. The implementation also does more than §5.2's summary: a dirty base folder
is stashed, pulled over and re-applied, so one frame would move the user's own
uncommitted work through the stash with nothing shown to either surface first.
Inventing a browser-only confirmation was rejected for the reason the command
module exists at all: it would be a second flow the desktop does not have.

The row is therefore **offered and refused**, carrying `PULL_BASE_REFUSAL` —
which names the asymmetry rather than pleading not-implemented, because it is not
a missing surface. If §5.2 ever grows a confirmation step, the decision is worth
revisiting; the test says so where it asserts `Confirmation::None`.

**Refusals reach the browser in the guard's own words, and the phase decides
what "applied" means.** R7 already routes a dispatch's outcome through
`Ui::web_outcome`, so §13's dirty base, §15's precondition refusals, §5.1's
rebase preconditions and git's own errors arrive verbatim rather than as a
generic failure — `web_git_guards` in `src/lib.rs` asserts each against a
`FakeGit` (SPECS §26 asks for the refusal paths, not only the happy ones). One
correction was needed: `Effect::Warning` is two different facts. From a
*confirmed* dispatch it means the operation landed and the cleanup after it did
not — applied-with-caveat. From an **unconfirmed** one it means a guard stopped
the flow before it asked, which is how §13's dirty base arrives, and reporting
that as `Applied` would be the browser claiming something the host did not say.
`dispatch_command` separates the two by the command's phase, once for every
two-phase command — the same line the phone's `dispatch_remote_merge_back`
already drew ("no merge happened, so it is a rejection").

**`show_git_status` still refuses**, unchanged and for R8's reason: it is not one
of D13's dialogs, nothing is being asked, and design turn 3 owns what the browser
shows instead (`remote-control-ll5.8`).

### R12 — the seat's facts, and the two policy numbers, are split on the wire

`remote-control-ll5.9`. Three defects with one shape: the browser was being made
to reconstruct something the host already knew.

**Artboard 2f's arriving-viewer panel lists three facts — address / browser /
connected — and `SeatInfo` carried the first two merged into one free-text
`label`.** The webui task rendered that merged label verbatim in the address slot
and left the browser slot empty, which was **the right call**: the browser half
of the label is a user-agent string, which is attacker-supplied free text and can
contain the ` · ` separator, so a browser-side split is a parse the parsed string
gets to steer. The fix therefore belongs on the wire, not in a parser.
`SeatInfo` now carries `address` and `user_agent_label` as additive
`#[serde(default)]` fields beside `label`, which stays because the compact viewer
chip (`desktop + this tab`) genuinely wants one line. `ViewerIdentity` in
`src/web/server.rs` is the single place the two are joined, and joining is the
only direction that is safe.

**The address stays host-observed.** `ClientInfo`'s rule — *the host owns the
address it observed and must not trust a client-supplied one for anything but
display* — survives the split unchanged and is now enforced where it is easiest
to get wrong: `ViewerIdentity::with_claim` lets an `Attach` frame replace what we
say the *browser* is and can never move the address, which came off the socket.
A claim of `9.9.9.9 · Chrome on macOS` lands in `user_agent_label`, verbatim and
unparsed, and the address row still reads the real peer.

The desktop row has `address: null` and `user_agent_label: null`, because it
arrived over no socket; 2f drops a row it was told nothing about rather than
printing a placeholder. So does a host from before the split — the merged label
is still a true answer for the address slot, which it starts with, and the
browser row goes undrawn.

**The third fact needed a clock, so `Delta::Seats` now carries one.** Splitting
the first two facts was not enough: `since_ms` is an instant on the *host's*
clock, and a `Delta::Seats` carried no reference to date it against. The browser
could not use it honestly — `Date.now()` measures a host instant with a local
clock that may be wrong — so it left the row undated, and 2f drew three facts
when the seat list arrived in a `Snapshot` and two when the same list arrived in
a delta. A viewer panel that silently drops *"connected 12s ago"* depending on
which frame carried the news is exactly the inconsistency this refinement exists
to remove, so `Delta::Seats` gained `server_time_ms` as a fourth additive
`#[serde(default)]` field, the same pairing `Snapshot` has always had. The
reducer stays clock-free: dating happens in `wire/adapt.ts`'s `seatOf`, which is
now **exported and shared by both paths** — two mapping functions is how the
inconsistency arose in the first place. A delta with no clock (`0` is serde's
default and is not a time) still renders the row without its `connected` line,
never with a fabricated or negative duration.

`WireError::seat_held` is not a seat list and carries no clock either, so an
arriving takeover panel still opens with `connected` blank. It is completed by
the first dated seat list to arrive — the snapshot on the observe-attach, then
the delta — through `refreshArrivingIncumbent` in `state/reducer.ts`. The panel
is closed only by the user's own answer, never by a seat list, and a list with
nobody in the seat does not blank out the name of the browser that just refused
us.

**`RATE_LIMIT_LOCKOUT_MS` and `BOOTSTRAP_CODE_TTL_MS` were mirrored in
TypeScript, and are host-sent now.** 2b prints *"3 attempts left before this
address is rate-limited for 60s"* while the address is still allowed to try, so
the lockout length is needed **before** the limiter fires — which `retry_after_ms`
cannot supply, because it only exists once it has. `refusal_body()` carries both
as `lockout_seconds` and `code_ttl_seconds`. `GET /auth/session` was the other
candidate carrier and turned out to be unnecessary rather than wrong: the SPA's
first act is that call, a browser with no live cookie is *refused* by it, and
that refusal is built by the same function — while the `authenticated: true` body
is only ever followed by the app, which draws none of 2b's copy. One carrier, no
field nobody reads. The precedent is `attempts_remaining`, which was already
host-sent and never guessed, and it is followed exactly: absent means *we were
not told*, and every sentence has an honest shape one clause shorter.

**`revoked_at_ms` completes 2b's revoked sentence.** `AuthFailure::TokenRevoked`
now carries the tombstone's `revoked_at_unix_secs`, the way `RateLimited` already
carried `retry_after_ms`, so the refusal that reports the fact also reports when.
It is sent **paired with the host's own `server_time_ms`**, exactly as `Snapshot`
pairs `server_time_ms` with every `since_ms`: the browser subtracts two host
timestamps rather than measuring a host instant against a local clock that may be
wrong, which on a security screen would print a confident wrong duration. Either
value missing — an older host, or a tombstone with no time — renders *"withdrew
this browser's access."* and stops. A zero is never sent, because zero is not a
missing time, it is 1970.

---

## 7. Reference

- `specs/WEBAPP_DESIGN_BRIEFING.md` — the design brief this implements.
- `specs/WEBAPP_DESIGN_BRIEFING_T2.md` — turn 2: access/QR (resolves Q1),
  connection and staleness states, takeover, activity feed, reference sheet.
- `specs/REMOTE_PROTOCOL.md` §8–§9 — the phone protocol we are deliberately *not*
  extending (D12).
- `specs/SPECS.md` §19–§24 — terminal model, layout, git panel, interaction model,
  keyboard modes, status detection.
- `src/lib.rs:5387-5405` — `sync_terminal_sizes` / `resize_if_changed`, the per-frame
  geometry invariant behind D4.
- `src/lib.rs:2637` — the phone-opened-shell geometry precedent that does not
  generalise.
- `src/web/commands.rs` — the one table behind the browser's command surface
  (R7): wire name to palette action, `host only` badges, refusals.
- `src/remote/client.rs` — the blocking relay client retired by D6/D7.
