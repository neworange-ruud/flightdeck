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
- Multi-user collaboration. One operator, several surfaces (see D14) — several
  of which may now type, arbitrated rather than merged.
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
> **D17 settles what this left open:** not pursued for the current architecture,
> which makes this cost permanent rather than provisional.

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
- **And when the viewport is smaller than the grid, the stage scrolls** — see
  §6.5 **R17**, which closes the direction this decision was never written for.
  Not clipped, not scaled, not refitted: the grid keeps its natural size, both
  edges stay reachable, and the browser still never measures itself.

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

### D11 — Notifications: in-app activity feed only, and no Web Push

A visible list of status transitions inside the app. No permission prompt, no
service worker, no push infrastructure — works identically in every browser on
every platform.

Web Push is not merely deferred but **structurally blocked** under D1, and D17
makes that **permanent for the current architecture** rather than an M1
limitation. There is no milestone in which it arrives.

The blocking mechanism is the secure-context rule. A service worker — the thing
that receives a push when no tab is open — registers only in a secure context.
`http://127.0.0.1` qualifies, but loopback is the one place push is pointless:
the desktop is on that machine and already notifies natively. The address a
*remote* browser actually uses is the LAN one from D5/Q1
(`http://192.168.2.20:<port>`), which is **not** a secure context, so no service
worker registers and no subscription is ever created. Serving that address over
HTTPS would need a certificate for a private IP, which no public CA issues.

**What would have to change first, in order: reachability, then browser key
custody, then push.** A relay transport is the only thing that supplies a public
HTTPS origin without third-party setup, and D17 declines to build one — for
reasons about key custody and PTY volume, not about notifications. So the
activity feed (§5.1) is the whole notification story, and a browser with no
FlightDeck tab open is silent by design.

> **One honest exception, recorded so nobody finds it and concludes the spec was
> wrong.** A user who has already paid D1's cost with an **HTTPS-terminating**
> tunnel (Cloudflare, Tailscale Funnel) is in a secure context and could
> subscribe. That does not make Web Push a FlightDeck feature: it would need
> VAPID keys and a push sender in the binary, and it would work for that subset
> of users while doing nothing at all for everyone else. Whether that narrow
> version earns its keep is a separate and much smaller question. D17 does not
> decide it and no milestone carries it.

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

### D14 — N writers and N observers, arbitrated by a soft input lock

Any number of browsers may seat themselves as **writers**; any number may watch
read-only. Among the writers — the desktop is always one of them — exactly one
holds the **input lock** at a time, and only that one's keystrokes reach the PTY.

**This has now been revised twice, in the same direction: towards saying what is
actually scarce.**

*First revision (turn 2).* D14 originally said "one viewer, full stop"; the
takeover artboard (2f) makes read-only observation a first-class affordance in
two places — cancelling a takeover leaves a live read-only view, and a browser
that has lost the seat can watch rather than fight. Adopted, because it preserved
D14's actual rationale exactly: the reason for the restriction was to defer the
interleaved-input problem, and read-only viewers have no input. What it added was
read-only fan-out, strictly simpler than multi-writer arbitration.

*Second revision (`remote-control-eek.2`).* The deferred problem itself, now
solved, so the single-controller restriction is lifted. **The read-only fan-out
above is untouched** — an observer still costs nothing in arbitration, and every
sentence of the first revision still holds.

#### Why a lock, and not the alternatives

A terminal has one cursor. Deliver two keystroke streams and the agent reads
`helwolrold`, which looks like a bug in the agent rather than in FlightDeck. The
three candidates:

| Model | Why not / why |
| --- | --- |
| Per-keystroke last-writer-wins | This *is* the corruption. Rejected outright. |
| Line-level batching | Not available to us: D2 requires the raw PTY byte stream and agents read raw keys, so there is no line to batch. |
| **A soft lock with a visible holder** | Adopted. |

#### The rule, in full

- A seat is a **writer** or an **observer** (`Seat::Writing` / `Seat::Observing`).
  `SeatRequest::Write` is never refused: several writers at once is the normal
  case, and that is what "lift the restriction" means.
- Among writers, exactly one holds the input lock. It is **claimed implicitly by
  typing**: a writer that types while the lock is free, or while its holder has
  been idle for `INPUT_LOCK_IDLE_MS`, takes it. Nobody presses a button to start
  typing, and nobody has to remember to let go.
- A writer that types **into another holder's live burst is refused** — never
  interleaved, and never silently dropped (§5.1). The refusal is an
  `Ack { rejected }` for the queue's bookkeeping plus an
  `Error { seat_held, incumbent }` that **names the holder**, because "that did
  not work" is indistinguishable from a broken host, and 2f exists precisely so
  that neither person wonders why the keys stopped working.
- **Explicit preemption reuses the vocabulary that already exists.**
  `SeatRequest::TakeOver` already meant "evict the incumbent and take the seat"
  and was already gated behind a confirmation in 2f; it now means *take the input
  lock now*. The desktop reaches the same act through `Take Input Lock` in the
  palette. There is **no hard-coded precedence for any surface** — a surface that
  could always cut in is exactly the corruption this removes, and an asymmetric
  rule is one more thing for a reader to get wrong. Symmetric rule, explicit
  override.
- Preemption demotes nobody. The interrupted writer keeps its seat and gets the
  lock back the moment the interrupter goes quiet, which is what lets 2f offer
  `Watch read-only` as a choice rather than as a consolation.
- **The holder is host state, published to both surfaces.** `SeatInfo` carries
  `seat` (the role) and `holds_input` (the turn) as two fields, because one
  merged `controlling` flag cannot express "three writers, one of them
  mid-burst". The browser's viewer chip reads `desktop + this tab ✎`; the
  desktop's status bar carries an `INPUT: <holder>` chip built from the same
  rows. Neither surface derives it, so the two cannot disagree.

#### `INPUT_LOCK_IDLE_MS = 400`

The number is the floor on how fast two people can alternate, so it wants to be
short — a few hundred milliseconds, not conversational. 400 ms is the smallest
value that satisfies all three constraints:

1. **Longer than a typist's gap between keystrokes**, or an ordinary burst would
   be broken mid-word and the other surface would splice into it — the exact
   corruption this exists to prevent. Fast typing runs 50–150 ms between keys and
   held-key autorepeat is 30–50 ms.
2. **Longer than the host's own drain latency.** A browser's keystrokes cross a
   channel and are written on the TUI's next render tick (`POLL_TIMEOUT`, 50 ms).
   If the lock could move while the previous holder's bytes were still queued,
   the queue itself would interleave them. 400 ms is eight ticks.
3. **Short enough that a hand-off is not a negotiation** — about one relaxed
   inter-word pause.

> **Cost accepted, and it is a real one:** during genuine simultaneous typing one
> side's keystrokes are **refused, not delivered**. They are not queued for later
> either, because a keystroke replayed once the other writer stopped would land
> in the middle of what they had typed — which is the thing being prevented. The
> loss is visible (an ack, a named holder, a panel) rather than silent, and that
> is the whole of the mitigation. Two people alternating faster than 400 ms hit
> the floor and are refused until it elapses.
>
> **Also accepted:** protocol v1 → **v2**. `Seat` and `SeatRequest` are closed
> vocabularies and both grew a member the peer must understand, which the wire
> protocol's own forward-compatibility policy makes a bump by definition. A tab
> left open across a host update is told to reload (D9), which is the failure
> mode that policy was written for. *(The wire is at **v3** as of R16, which
> applied this same standard to `ServerMsg` growing `GitStatus`. `PROTOCOL_VERSION`
> in `src/web/protocol.rs` is the live number; this paragraph records why v2
> happened, not where the wire is.)*
>
> **Out of scope, and named so it is not mistaken for an oversight:** FlightDeck
> Remote's phone reply path (`write_primary_pty`) does not claim the lock. It
> delivers one complete message as a single atomic write rather than a keystroke
> stream, and it is not a FlightDeck Web surface; bringing it under the same
> arbiter is a `area:relay` question, not this one.

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

### D17 — Relay-tunnelled transport: not pursued · **provisional**

D1 left one thread hanging — *"a relay-tunnelled transport is not ruled out
later"* — and §4 carried it into M3 as "if pursued", which is not a plan. This
settles it. **FlightDeck Web does not get a relay-tunnelled transport in the
current architecture.** Reaching the embedded server from outside the network
stays exactly what D1 made it: the user's own Tailscale, `ssh -L`, or Cloudflare
tunnel. Those work today, on every platform, and cost us no code.

Three reasons, heaviest first.

**1. The browser cannot hold the key the phone holds.** A relay transport
re-introduces precisely what D1 removed — relay involvement *and* E2E crypto in
the browser — and the phone's model does not transfer. `REMOTE_PROTOCOL.md` §7.1
derives the channel from a **static-static P-256 ECDH** between long-lived
key-agreement keypairs with **no forward secrecy in v1**, which is only
defensible because of where those keys live and who writes the code that uses
them: on iOS the identity key is Secure-Enclave-resident and non-exportable, the
software KA key sits in the app's Keychain behind the sandbox, and the code that
applies it is App Store–delivered and OS-verified.

A browser can hold *something*. WebCrypto generates a non-extractable
`CryptoKey`, IndexedDB stores it, and the private scalar is then unreadable from
JavaScript. That solves exfiltration, which is not the problem. The key is bound
to an origin, and **whoever serves the origin writes the code that uses the
key** — so a relay that both routes ciphertext and serves the SPA can ship one
build of the JavaScript that seals to a key of its own, and no user can detect
it. "Zero-knowledge relay" and "the relay serves the client" cannot both be true.
Fixing that means the page must come from somewhere the relay does not control,
which is the host you cannot reach, which is the problem we started with.
WebAuthn does not rescue it — passkeys sign, they do not do ECDH. And
non-extractable keys are erased by "clear site data", by private windows and by
storage eviction, so pairing becomes something the user re-does at intervals
nobody can predict.

That is an unsolved security design question, not plumbing waiting on a sprint.
It is why this decision is a "no" rather than a "later".

**2. It puts raw PTY bytes through a relay built for curated envelopes.** D2
requires the *actual* byte stream so xterm.js can parse it, which means the relay
may not summarise, coalesce or drop. `remote/relay` is not shaped for that: it
routes discrete `ciphertext` envelopes by `pairing_id`, sequences them, persists
per-pairing queues, prunes on cumulative ack, and bounds each queue by **envelope
count** with drop-oldest on overflow (`remote/relay/src/queue.rs`). Every one of
those properties assumes a message that is individually meaningful and
individually droppable. For a VT byte stream, drop-oldest is not a lost
notification — it is a corrupted parser state.

The volume differs in kind, not degree. What dominates it is bulk program
output: `cargo build`, a test run scrolling, an agent streaming tokens, a
full-screen TUI repainting, one stream per live terminal. The phone protocol
never carries any of that, because `specs/MOBILE_REMOTE_BRIEFING.md` exists
precisely to curate it away — the phone's traffic is bounded by how much a human
reads, the browser's by how fast a compiler writes. And the relay is New
Orange–operated infrastructure, so that egress is billed to us, per user, per
unbounded session, with an authenticated pairing becoming a general-purpose byte
pipe for anyone who wants one.

**3. Nothing else in M3 needs it.** Multi-viewer and the narrow-viewport layout
are local-transport features. Removing the transport question from M3 is what
lets M3 ship.

> **Cost accepted, and it is user-facing.** Three things are foreclosed, plainly:
>
> 1. **Web Push is permanently unavailable** for this architecture, not deferred
>    — see D11. FlightDeck Web will never raise an OS notification or wake a
>    phone; the in-app activity feed is the entire story, and it is silent when
>    no tab is open.
> 2. **"Control it from the train" keeps its third-party setup cost**, which D1
>    already accepted — now permanently rather than for one milestone. A user
>    with no Tailscale and no tunnel cannot reach FlightDeck from anywhere but
>    their own network, and documentation carries that burden indefinitely.
> 3. **The project keeps two reachability models on purpose** — relay for the
>    phone, the user's own network for the browser. They will not converge, and
>    anyone asking "how do I reach FlightDeck" must answer "from which surface"
>    first.
>
> **Provisional, pending the repository owner's ratification** — marked in §6's
> sense: build against it, do not wait on it. Two things reopen it: a concrete
> demand for Web Push that the activity feed demonstrably cannot answer, or a
> browser key-custody design that survives reason 1 — specifically one where the
> code that uses the key is not delivered by the party the crypto defends
> against. Either reopens this as a design question, not as an epic. **No
> implementation epic is filed, deliberately**: filing one would assert the
> "if pursued" branch this decision declines.

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
| **M1** | Embedded axum server, token auth + access overlay, palette start/stop + config opt-in, web protocol v1, `webui/` SPA against artboards 1a–1c and 2a–2g, live state, raw terminal streaming, terminal input, takeover **plus read-only observers** (D14 as first revised), activity feed, Rust integration tests + Playwright job. |
| **M2** | Command palette, the dialog family with origin labels, git commands, destructive operations with two-step confirmation, configuration manager, split view (1c–1g). |
| **M3** | Multi-viewer (the viewer panel as rows, over D14's existing fan-out) and the narrow-viewport / slide-over layout, with the turn-3 designs those need (§5). Also D14's second revision: the input lock, protocol v2, and the seat vocabulary the panel's rows render. **No relay transport, and therefore no Web Push** — see D17 and D11. |

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

**Nothing is undesigned any more, and that table is gone.** What replaced it is
the list below: the two screen families turn 3 would have drawn, both **built
without a turn**, each with its derivation recorded in full and individually
overrulable.

| Screen family | Built by | Derivation |
| --- | --- | --- |
| Help overlay, git-status overlay, About | `remote-control-ll5.8` (M2) | §6.5 **R16** |
| The below-900px layout — the slide-over sidebar, the session chip, the git bar folded into the status bar, and what D4 does when the host's grid does not fit | `remote-control-eek.4` (M3) | §6.5 **R17** |

**Turn 3 was not run**, by the repository owner's decision, recorded on
`remote-control-v4s`'s close: the instruction was to design and build these
screens directly from the rules turns 1 and 2 already establish rather than to
wait for a human-gated design session. That lifted the block; it did not lower
the bar.

**R16 and R17 are not substitutes for a turn.** Each is a record of one screen
family derived under the standard a turn would have been held to, written so
that a later turn inherits reasoning rather than a fait accompli — every call
the artboards did not cover is enumerated, and a turn may overrule any single
row of either without reconstructing why the row is there. A future turn 3 is
still the right way to *draw* these screens; what it is no longer is a
prerequisite for having them.

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
  requirement, not tuning: D14 gives out one input lock at a time, and two workers
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
  `GIT_DIALOG_REFUSAL` is gone. The abandon half is superseded by R13:
  `remote-control-ll5.4` built 1g's two steps, so `abandon_worktree` is a
  dispatching row and `DESTRUCTIVE_DIALOG_REFUSAL` is gone too — the refusal
  became a gate on the confirm, and R11's rebase was pulled behind it.)*
  Cancelling is never gated: dismissing a confirmation cannot destroy anything,
  and a shared dialog a remote surface can see but not dismiss would be worse
  than not sharing it.
* **`show_git_status`** — it is **not** one of D13's dialogs. Nothing is being
  asked, so there is nothing to confirm or cancel; it opens a read-only overlay
  and design turn 3 owns what the browser shows instead. It was grouped with the
  dialog family before this task only because it opens a desktop overlay.
  *(Superseded by R16: turn 3 was not run, `remote-control-ll5.8` derived the
  browser's panel from turns 1 and 2, and the row now dispatches — the host
  collects SPECS §21's facts and answers the asking browser with
  `ServerMsg::GitStatus`. R8's classification is unchanged and is the reason it
  is a per-viewer reply rather than a shared dialog or a broadcast delta;
  `UNDESIGNED_OVERLAY_REFUSAL` is gone.)*

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
shows instead (`remote-control-ll5.8`). *(Superseded by R16: it dispatches now,
and its answer goes to the browser that asked rather than onto the desktop's
screen. It is still not a dialog — that is exactly why the answer is a
per-viewer frame.)*

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

### R13 — 1g's second step is a remote-surface gate, and it is checked before a key is fed

`remote-control-ll5.4`. Artboard 1g draws a destructive confirmation twice —
`step 1 of 2` with the consequences and the keyed buttons, then `step 2 of 2 —
confirm` with a field where the session's own name is typed back. Everything
below follows from reading step 2's own copy.

**The contradiction in 1g, and the ruling.** The caption says *"Abandon and Quit
are the only two that reach step 2."* The artboard **draws Rebase Worktree** as
its two-step example, and step 2's copy reads *"This browser is remote. Type the
session name to run the rebase on the host."* The drawn artboard wins, and the
copy says why: **the trigger is the surface being remote, not the command being
destructive.** Read that way the caption is not wrong, it is counting a different
world — the desktop's, where *nothing* reaches step 2, because the person
answering is at the machine the effect lands on. That is what lets an enumeration
of two stand beside a picture of a third.

So the ruling is: **step 2 is a browser-only gate, and from a browser it covers
the answers that destroy work or rewrite history** — §5/§15's **Abandon
Worktree**, §5.1's **Rebase Worktree** (the one 1g actually draws) and D16's
**Quit**. `Push Branch` and `Finish / Local Merge` stay one-step, explicitly:
neither rewrites history nor discards anything — a push is undone by a push, a
merge-back is a commit on the base branch — so 1g's friction there would be
ceremony, and ceremony is what teaches people to type the name without reading
it. The desktop's dialogs are untouched in every case.

**This tightens R11 rather than extending it.** `remote-control-ll5.5` made the
git confirmations answerable from a browser with one press; rebase's browser
confirm now has to pass step 2 as well. Nothing about R11's boundary invariant
moved: the row still carries `RebaseWorktree { confirm: false }`, the exception
clause still names exactly one history-rewriting row, and
`a_frame_cannot_smuggle_a_confirmed_rebase` still holds. The gate is strictly
additional, and it is on the *answer*, not on the row.

**The mechanism, and what is enforced by construction versus by check.**

1. **Both destructive rows dispatch their unconfirmed value** —
   `abandon_worktree` joins `rebase_worktree` on a palette route, and `quit`
   joins them by gaining `Command::Quit { confirm }`. *By construction:* R7's
   forwarding rule means the payload comes from `INVENTORY`, never from the
   frame, so the first dispatch can only open D13's shared question.
   `Confirmation::Given` remains unreachable from any row.
2. **The desktop's quit is unchanged.** SPECS §23 asks the person at the keyboard
   nothing, and this did not add a question: the desktop's palette row and
   `Ctrl-q` carry `Quit { confirm: true }`. The confirmation is not a
   browser-only flow either — it is a D13 dialog, shared, origin-tagged, and
   answerable with one `y` from the desktop. Only an unconfirmed dispatch opens
   it, and only the browser's row carries one.
3. **The gate is host state, published with the dialog.** `DialogBody` gained
   `confirm_gate: Option<ConfirmGate>` — the button key it guards, the exact
   name to type, and the sentence saying why. All three are host-worded, so the
   browser authors nothing about which dialogs are dangerous (R7 as amended by
   ll5.12). `expected` is published rather than kept secret because 1g draws it
   as the field's own hint: the gate buys deliberateness, not secrecy.
4. **One function resolves the name, for both readers.** `gate_expectation`
   answers what the browser is shown *and* what the confirm is checked against.
   A second spelling of "which name is that" is how a gate becomes unpassable
   or, worse, passable with the wrong name.
5. **The gate guards one button, not the dialog.** The sidebar's close menu
   (`a` Abandon / `c` Close) is deliberately **not** gated: `a` dispatches the
   *unconfirmed* abandon, which asks — so the browser lands on the abandon
   confirmation and takes step 2 there, once, in front of the button that
   really does it.
6. **The check runs before a single key is fed.** *By check, in exactly one
   place:* `apply_web_dialog` compares `confirm_name` against `expected` and
   returns before the synthetic keypresses begin. So "the effect provably does
   not occur" is a property of the control flow — there is no rollback, because
   nothing started. A frame with no name at all (an older browser, a replay) is
   refused the same way, and cancelling never reaches the check at all.
7. **The comparison is exact: no trimming, no case folding, no normalisation.**
   A name that needs correcting before it matches is a name that was not read,
   and git branch names are case-sensitive — a fold would accept a name the host
   does not have. Both surfaces compare the same two strings the same way, so
   the browser never enables a button the host is about to refuse.
8. **A gate the host cannot resolve refuses.** If the session the question was
   about is gone there is no name to check, so `confirmable: false` and
   `GATE_UNRESOLVED_REFUSAL` — confirming past an unresolvable gate would
   destroy something nobody named. Cancelling still works.

**A confirmation raced by a takeover cannot slip through, and does not need a
comparison to prove it.** The typed name rides on the deciding frame itself, so
the host keeps no "this viewer is armed" state for a second browser to inherit —
the seat that typed the name *is* the seat that confirms, structurally. What is
left is D14's ordinary seat check, which runs before a command's route is even
considered, so a read-only browser's confirm — correct name and all — is answered
`read_only` and never reaches the host
(`a_confirm_from_a_browser_watching_read_only_never_reaches_the_host`).
*(D14's second revision narrows what puts a browser on the wrong side of that
check: a takeover no longer demotes anyone, so it is 2f's `Watch read-only` —
which a browser chooses — rather than eviction. The check itself is unchanged.)*

**Cancelling is never gated**, at either step, on either surface. R8's reason
stands unchanged: a shared dialog a remote surface can see but not dismiss would
be worse than not sharing it. The browser's step-2 panel keeps `Esc Cancel`
enabled with a half-typed name in the field, and the host's `dialog_cancel` path
returns before the gate is ever consulted.

**What the browser contributes is a reading position, never a claim.**
`draft.step` and `draft.confirmName` are local — 1g's step 1 sends nothing at
all, so pressing `y` on a destructive dialog from a browser commits to nothing.
`gateSatisfied` disables the confirm until the name matches, which is an
affordance in front of the host's own check, not a substitute for it.

**Tested on both sides, on the refusal paths first** (SPECS §26). Rust:
`quitting_from_a_browser_takes_the_typed_project_name` (`ui.should_quit` is a
boolean, so "it did not happen" is asserted directly),
`a_destructive_confirmation_needs_the_exact_session_name` (wrong name, wrong
case, trailing space, step 1 alone — the tab survives all of them and no decision
is witnessed), `the_rewrite_is_gated_and_the_other_git_confirmations_are_not`
(nothing reaches `GitExecutor::rebase_onto`),
`a_gate_with_nothing_to_name_refuses_but_still_cancels`,
`exactly_three_answers_are_behind_step_two_from_a_browser` (the ruling as a
table, plus the check that a gate's key is a button the dialog really shows),
and `the_desktop_answers_the_quit_dialog_with_one_key` (the ruling's other half).
webui: the gate's exactness, the local advance that sends nothing, and every way
of pressing `Enter` on a wrong name producing no frame at all.

### R14 — the input lock is a third thing, beside the seat and the watermark

`remote-control-eek.2`. D14's second revision names the model; this records where
it lives and the three seams that shaped it.

**One arbiter, one mutex, and it is not the seat registry.**
`src/web/arbiter.rs` owns `InputArbiter`, shared as an `Arc<Mutex<_>>` by the
tokio task that owns a browser's socket and the TUI thread that owns `AppState`.
The two were kept apart deliberately: the seat roster changes when a tab opens or
closes, the lock changes several times a minute, and one mutex for both would put
every keystroke behind the same lock as every fan-out. They meet in exactly one
place — `SeatRegistry::seat_rows`, which is handed the current holder and marks
the row that has it — so `holds_input` has a single source.

**The claim happens before the channel, not at the PTY.** A browser's `Input` is
arbitrated in `handle_client_msg` and only then forwarded as
`WebInbound::Input`; the desktop's claims in `desktop_may_type` before
`write_active_pty`. That ordering is what makes the drain safe: everything queued
for the TUI is from one holder until the lock moves, so applying it in order
cannot splice two writers together. Arbitrating at the write instead would put
both writers' bytes in the same queue and lose the property entirely.

**Nobody causes an expiry, so the tick announces it.** Every other movement of
the lock is announced by whoever caused it. Idleness is the passage of time, so
`WebServerHandle::sync_input_lock` runs once per render tick, retires a holder
that has gone quiet, and fans out a `Delta::Seats` only when the holder actually
changed — otherwise a per-tick call would be a per-tick fan-out. It is also why
both surfaces can show *free* rather than naming somebody who stopped typing a
minute ago.

**A refused keystroke leaves the browser's queue.** §5.1's held-queue releases a
frame on any ack, `rejected` included, which is correct here and deliberate: the
alternative is retrying it once the lock frees, which delivers it into the middle
of what the other writer typed. The queue's other invariants are untouched — a
refused `seq` is **not** recorded as forwarded, so `Snapshot::last_input_seq`
never claims a keystroke the host declined.

**What the tests pin.** `two_writers_typing_at_once_never_interleave_at_the_pty`
(`tests/web_server.rs`) is the criterion: two real sockets, both seated as
writers, each typing a five-byte token one frame per byte at 40 ms intervals so
the bursts genuinely overlap in time, with a 20 ms stagger deciding the round's
winner and alternating so both reach the terminal. The assertion is on the fake
PTY's transcript — every five-byte chunk must be one writer's whole token.
Removing the claim makes it read `12121212..21212121..`, which is the failure it
exists to catch. `two_threads_typing_at_once_never_split_a_burst`
(`src/web/arbiter/tests.rs`) asserts the same property on two real threads
without a socket in the way, and
`the_desktop_has_no_precedence_over_a_browser` pins the symmetry in both
directions so a future "the desktop is special" cannot pass.

**What this leaves for `remote-control-eek.3`.** The multi-viewer panel renders
rows, and it now has two fields per row to render rather than one: `seat` (may
this surface type) and `holds_input` (is it typing now), with `writers()`,
`observers()` and `inputHolder()` in `webui/src/state/seats.ts` as the only
correct questions. `TakeoverState`'s `evicted` direction is modelled, styled and
tested but still **not dispatched by `socket.ts`** — it was not wired under v1
either, and firing it on every ordinary hand-off would be a modal every time the
other person starts typing. Wiring it to explicit preemption only needs the host
to say *which* movements were deliberate, which is a per-recipient field on
`Delta::Seats` that this task did not add. *(Added by R15 below.)*

### R15 — the panel is the roster, and the host says which movements were meant

`remote-control-eek.3`. R14 left the multi-viewer list and one loose end; both
are the same panel, which is why they are one entry.

**The list is 2f's panel with rows, not a new screen.** D14's caption says so
literally, and the implementation takes it literally: `webui/src/ui/takeover.ts`
replaces the single incumbent's `address / browser / connected` fact list with
one row per seat, on both of the panel's directions. The rows are read from
`AppState.seats` rather than from `TakeoverState`, which is the whole of what
makes them **live** — a `Delta::Seats` arriving while the panel is open repaints
it, so a reader watching the lock move sees it move.

Each row carries the two facts R14 insisted on keeping apart, as two marks: the
role (`can type` / `read-only`, from `seatRoleLabel`, shared with the chip's
tooltip so the two cannot drift) and the turn (`typing now`, plus the chip's `✎`
glyph, on at most one row). *Three writers, one of them mid-burst* renders as
three rows. The reader's own row is marked `this tab` from `SeatInfo::is_you`,
which the browser had been dropping on the floor: two tabs on one machine are
two rows with the same address and the same browser, so matching on either marks
both, and the host is the only party that knows. The fact list survives as the
fallback for the one case that genuinely has no roster — `WireError::seat_held`
naming a holder before any dated seat list has arrived on a fresh socket.

**The evicted panel now fires, and only on a confirmed override.** The reason it
could not simply be fired on every seat delta is R14's: under the revision the
lock moves on every ordinary hand-off, so "the lock left me" is true several
times a minute and a modal on it is an obstruction rather than a notice. The
distinguishing fact is *intent*, and intent exists only at the host at the
moment of the act — so the host carries it, as
**`Delta::Seats::you_were_preempted`**, per recipient beside `you`. One
preemption interrupts exactly one writer; a list-shaped `preempted:
Option<ViewerId>` would broadcast the same fact to everybody and invite each
browser to compare it against its own id, and a browser that gets that
comparison wrong shows a modal to the wrong person. It names no interrupter,
because the rows already do (`holds_input`) and a second field naming the same
surface is a second thing that can disagree.

`SeatRegistry::seat_frames` takes the interrupted writer alongside the holder,
and `Shared::announce_seats` takes it as a parameter rather than deriving it from
`announced_holder`: that field records *what* moved, and only the caller knows
*why*. `Some` at exactly three sites — a browser's `Attach { seat: TakeOver }`,
a browser's `take_input_lock`, and `preempt_input_for_desktop` — all of which go
through `preempt_for_viewer`, which reads the holder *before* the preemption and
returns `None` when the claimant already held it, so confirming `Take over`
twice does not show the panel to the person who pressed the button. An
interrupted *desktop* flags nobody: 2f gives the person at the machine a
transient strip, because their keyboard was never revoked.

**Not a version bump, and the reasoning is the policy's own.** §3's
forward-compatibility rules make a bump necessary when a field's meaning
changes, a required field appears, or a **closed vocabulary grows a member**.
This is an additive `#[serde(default)]` `bool`, and `false` is the honest reading
on a host that never sends it — *this host reports no preemptions*, which leaves
the browser exactly where it was before the field existed rather than guessing.
So `PROTOCOL_VERSION` stayed at 2 for *this* change, and
`webui/e2e/chain.spec.ts` needed nothing (it takes the number from
`wire/frames`, and the number did not move). *(It has since moved: R16 took the
wire to v3 for a frame **kind**, which is a different clause of the same policy.
The distinction this paragraph draws — an additive field is not a bump — is
unchanged, and is exactly the distinction R16 turns on.)*

**One copy change, and it is 2f's own sentence.** The evicted panel's *"the last
one that landed was 3s ago"* clause is now dropped entirely when this tab has no
keystroke that landed — `socket.ts` dates it from the most recent **applied**
ack, never from a send or a refusal, so a tab preempted before it ever typed
gets the sentence without the clause rather than a time invented for an event
that did not happen. The panel's remaining two-seat wording needed nothing: it
describes *the other writer*, which is still exactly one surface however many
rows the roster has.

**The chip and the panel agree by construction.** D14 as revised says outright
that "the browser's viewer chip reads `desktop + this tab ✎`", and the chip did
not: `is_you` had never been mapped into the browser's model at all, so
`viewerChipText` rendered the reader's own row by the host's label and a real
session read `desktop + 192.168.2.20 · Chrome on macOS ✎`. It only *looked*
right because `fixtureSeats` set that row's label to the literal string
`this tab` — a fixture that had pre-baked the answer.

Both surfaces now read the one field: `seatChipName` in `state/seats.ts` for the
chip's line, `SeatInfo.isYou` for the panel's row, and neither derives it from
anything else. **Matching on the address or the label is not a shortcut, it is a
wrong answer**, and it is wrong in the ordinary case rather than an exotic one:
two tabs on one laptop send the same `User-Agent` from the same address, so
their rows are identical in every field a browser could match on, and a label
match names *both* of them `this tab`. Only the host can tell them apart,
because it is the one building a frame per recipient. The fixture now carries
the host's real label so that a chip which stopped deriving the name cannot keep
passing — `npx vitest run` fails eight tests if `seatChipName` is reverted to
returning the label, four of them the pre-existing artboard tests for 2c and 1a.

The chip's compact line has room for one of the two, so it shows `this tab` in
place of the label; its **tooltip** has room for both and shows
`192.168.2.20 · Chrome on macOS · this tab — can type · 14 minutes`, in the seat
panel's own column order. Nothing is invented when the host marks nobody: every
row is then named by its label, which is what a host that does not send `is_you`
is actually saying.

### R16 — the read-only overlays were derived, not designed, and here is the derivation

`remote-control-ll5.8`. The help overlay, the git-status overlay and About were
the last of §5's M2 gaps, and they were the one part of M2 that §5 listed as
*"not yet designed"*. **Design turn 3 (`remote-control-v4s`) was not run, by the
repository owner's decision**, with the instruction to design and build these
three from the rules turns 1 and 2 already establish rather than to wait. That
lifts the block; it does not lower the bar. This entry is the derivation, in the
form a design turn's hand-off would have taken — and it is written so that a
later turn can overrule any single row of it without having to reconstruct why
the row is there.

**The block was on the screens, not on the plumbing.** Two of the three needed
no design input at all to be *correct*, only to be *drawn*: help and About are
the host's own words, and the reason they had no browser surface was that
nobody had said where the words come from. That question — R7's question — has
one answer in this codebase, and answering it is most of the work below.

#### 1. The host owns the words, for the third time

R7 put the command inventory on the wire because *"the host is the only thing
that knows what it implements"*. R2 put the git union host-side for the same
reason. Help is the third instance and the most obvious one: a browser that
compiled in its own keybinding list would be right until somebody changed a
binding, and then it would be a browser confidently documenting a FlightDeck it
is not attached to — with no test anywhere able to see it.

So `src/tui/help.rs` now owns the help and About content as data, the desktop's
`draw_help_overlay` / `draw_about_overlay` render it, and `Snapshot::help` /
`Snapshot::about` carry the same values to the browser. The types are
re-exported by `web::protocol` rather than restated, which is that module's own
stated practice for `InterpretedStatus` and `TabId`. Two facts vary at runtime
and both are read from the live `AppState` when the snapshot is built:
`[ui] use_f2_to_leave_terminal_focus` (which of three keys leaves terminal
focus) and SPECS §32's `--isolated`. `HELP_KEYS` is now one constant behind the
status bar *and* the panel's own `F1 / Alt-h` row, so those cannot drift either.

They ride on the snapshot for R7's reason exactly — static for the life of the
build, so there is no change for a `Delta` to describe.

#### 2. Git status dispatches; help and About do not

The three overlays split, and the line is **whether the browser already has the
facts**.

* **Help and About** are on the snapshot, so their palette rows are intercepted
  in the browser and never sent — precisely as `open_configuration` already is
  (`remote-control-ll5.6`). The host's rows stay in `INVENTORY` and stay
  refusing, with the refusal reworded from *"this task owns it"* to what is
  actually true: **forwarding would open a panel on the desktop that the person
  who asked cannot read** (`HELP_REFUSAL`, `ABOUT_REFUSAL`, and
  `OPEN_CONFIGURATION_REFUSAL`, now a named constant so the three read as one
  rule). The row is still the host's, so a host that stops offering the name
  stops offering the panel.
* **`show_git_status` dispatches.** SPECS §21 wants the upstream's *name*, the
  worktree path and §14's compare URL; none is on the snapshot, and the compare
  URL needs a `git remote` lookup nobody would want on the status poll. So the
  row now carries `Route::Palette(Dispatch(ShowGitStatus))` — it reads, it
  rewrites nothing, it creates no pull request, and R7's forwarding rule means
  the frame's `args` are never read. `UNDESIGNED_OVERLAY_REFUSAL` is gone.

**A browser's read does not open the desktop's overlay.** This is the ruling
R8 implies and did not have to make: git status is *not* one of D13's dialogs,
nothing is being asked, so there is nothing for the person at the machine to
answer — and a read-only panel they never requested is an obstruction, which is
the same judgement 2f makes when it gives an interrupted *desktop* a transient
strip instead of a modal. `apply_effect` therefore routes one collected
`Effect::GitStatus` two ways by origin: the desktop's own run opens the
desktop's overlay unchanged, and a browser's run is answered to that browser
alone with a new per-viewer `ServerMsg::GitStatus`. There is still exactly one
`collect_status` call and one dispatch path; only the rendering differs, which
is what §1 has always permitted.

**Also accepted: protocol v2 → v3.** `ServerMsg` is a closed vocabulary and it
grew a member the peer must understand, which the wire protocol's own
forward-compatibility policy makes a bump by definition — the same standard D14
applied when `Seat` and `SeatRequest` took v1 → v2. This entry originally
claimed the opposite, reading `ServerMsg`'s `#[serde(other)]` catch-all as a
licence to add frames freely; that is wrong, and the correction is worth keeping
on the record because the reasoning is the interesting part.

The catch-all is a **crash guard, not a compatibility guarantee**. Dropping an
unrecognised frame is the right outcome only when the frame carried news the
peer can live without — `Delta::Seats`'s `you_were_preempted` is that shape, and
a host that never sends it is honestly saying "this host reports no
preemptions". A frame that *answers something the peer asked for* is not that
shape, and here the palette makes the failure concrete rather than theoretical:
the inventory is host-driven (R7), so a stale **v2** tab attached to this build
would pass the version check (both sides say 2), be handed a `show_git_status`
row the host lists as runnable, send it, have it accepted — and then drop the
answer on its `default` branch. The user clicks **Show Git Status** and gets
silence, with nothing anywhere reporting a fault. That is precisely the failure
"reload to update" exists for, and only the constant can produce it.

`MIN_SUPPORTED_VERSION` and `MAX_SUPPORTED_VERSION` move with it, as always:
server and SPA ship in one binary (D9), so a difference is a stale tab and not a
negotiation. `webui/e2e/chain.spec.ts` needed nothing — it imports
`PROTOCOL_VERSION` from `wire/frames` and passes it into its observer socket
precisely so a bump is not a twenty-second timeout, which was checked rather
than assumed.

**What did *not* contribute to the bump**, stated because the two changes landed
together: `Snapshot::help` and `Snapshot::about` are additive
`#[serde(default)]` `Option` fields under rule 4. A v3 browser attached to a
host that sends neither reads `None` and renders the "this FlightDeck did not
send its keybindings" path — a lesser panel, not a wrong one — which is asserted
by `help_and_about_are_additive_and_absent_parses_as_absent`. Nothing else this
task added is a required field or changes an existing field's meaning.

The bump is also now defended by tests rather than by this paragraph.
`the_newest_frame_kind_is_covered_by_the_advertised_version` pins the newest
frame kind against the version that was current when it arrived, so the next
person to add one has to say where it stands before the crate's tests pass —
verified to fail against an un-bumped constant, which is to say it would have
caught this mistake. And the forward-compatibility policy's rule 1 now states
the carve-out in the module doc itself, so the clause I misread cannot be
misread the same way twice.

(Separately: adding the two snapshot fields pushed `ServerMsg` past clippy's
`large_enum_variant` threshold, so `ServerMsg::Snapshot` is now boxed. The wire
format is unchanged — `Box` is transparent to serde — and every other frame,
including one per keystroke, stops being sized for the snapshot.)

#### 3. What each screen was derived from

| Element | Derived from |
| --- | --- |
| The panel shell — titled 1px frame, head with the title, body, keyed footer | 1d/1f/1g: *"same shell for every dialog: titled 1px accent frame, keyed buttons"* |
| **Blue** frame (`--fd-info`) on all three | 1g's own legend: *"Cyan frame = confirm/select, blue = notification, red = destructive."* Cyan is taken (1d, 1e, 1f — all of them ask you to choose), red is 1g's, magenta is 2f's. Blue is the one the legend named and nothing had claimed, and *notification* is exactly what these are |
| Centred panel over a dimmed frame, not a slide-over | 1d and 1f, both of which are this. §5.1's *"never a modal"* is 2e's rule and is specific to it: the feed is D11's entire substitute for OS notifications, so blocking the screen would interrupt the terminal you are reading to tell you about one you are not. None of these three is a notification; each is on screen because the reader just named it in the palette |
| `role="dialog"`, no `aria-modal`, no focus trap | every existing browser-owned overlay (`accessScreen`, `takeover`, `commandPalette`, `configManager`) |
| Four type sizes, named tokens only | 2g, enforced by `tokens.guard.test.ts` |
| Fact rows as `quiet label / full-contrast value` | 2f's `address / browser / connected` list |
| Group eyebrows in `--fd-focus`, uppercase, `--fd-t-meta` | 1d's palette group headings |
| `clean` in `--fd-ok`; drift in `--fd-elsewhere`; upstream name in `--fd-accent` | 2g names each of these meanings against those tokens (`--fd-elsewhere` = "drift"; `--fd-accent` = "interactive, **upstream**") |
| `no-upstream` in `--fd-text-quiet` | 2g's own worked example of the lifted dim tier |
| `host only` badge on the worktree path and on the host's key group | D16 |
| Drift and dirty wording (`4 commits ahead since creation`, `clean`) | the desktop's own `draw_git_status_overlay`, word for word, so the two surfaces say the same sentence |
| The compare URL as a link that says FlightDeck did not open a PR | SPECS §5 (*"must not: create GitHub PRs"*) and §14 (the compare URL is the whole of what it does) |
| Ahead/behind absent with no upstream; no compare row with no URL | SPECS §21's *"if known"*, and §5.1's "unknown stays unknown". `WorktreeStatus` literally holds `0`/`0` when it never looked, so the wire's `GitUpstream` puts the counts **inside** the optional upstream and the impossible pair cannot be built |

#### 4. The calls the artboards do not cover

Each of these is a decision this task had to make. A later design turn may
overrule any of them.

1. **The help overlay has two halves.** Rendering the host's thirty chords
   unqualified in a browser tab would be the app's first outright lie: `Ctrl-q`
   typed here goes to the agent, because §5 gives the SPA one chord and it passes
   the rest to the PTY. So the panel states *This browser* (authored in
   `webui/src/state/help.ts`, every row implemented in `ui/app.ts`) and *On the
   host* (from `Snapshot::help`), with D16's badge on the host's heading and one
   sentence between them saying where those keys act and that the palette reaches
   all of them. **Derived from** D16's "honest about where the effect lands" and
   §5's palette-primary position; **not drawn by any artboard.** A turn could
   reasonably reject the two-column framing, or badge rows instead of the group.
2. **The badge is on the group heading, not on every row.** A badge exists to be
   noticed and thirty of them are wallpaper; the fact is uniform across the
   group. A turn could overrule this cheaply.
3. **The browser binds `?` in App mode, and nothing else.** The desktop's `F1` /
   `Alt-h` are deliberately not taken: `F1` is the browser's own help in Chrome
   and Firefox and `Alt-h` opens a menu on Windows, and §5 gives the app exactly
   one chord. A *plain key in App mode* is the affordance the artboards do
   license — 2e claims `a` on exactly this reasoning ("not in Terminal mode,
   where `a` is a letter the agent is waiting for") — and `?` is free there. The
   palette row is the other door and needs no key at all. **A turn is free to
   pick a different letter, or none.** The status bar was deliberately *not*
   changed to advertise it, because 1a/1b/2e draw that bar and adding a hint is
   an artboard change.
4. **`Esc` closes, and the panel takes the keyboard while it is up** — the
   posture the palette, the configuration manager and the access screens
   already have. The panel sits over a scrim that covers the frame to the
   pointer, so letting `a` open the activity feed *behind* it would be the app
   doing something else while the reader is reading. Nothing is lost by
   swallowing: since R5, terminal input comes from xterm's own `onData`, so a
   key eaten here was only ever one of the frame's app-level shortcuts, and
   §5.1's "queued, never dropped" is about keystrokes bound for the PTY.
   `Tab` is the exception, exactly as it is on 2b: the panel has a link and a
   close button, and a keyboard-only reader has to be able to reach them.
   Clicking outside closes it, which is the pointer half of `Esc` and matches
   1a's advertised `click outside release keys` gesture.
5. **One overlay at a time, structurally.** `AppState.readOnly` is a union
   holding one panel, and opening one closes the palette — the handoff
   `config/open` already makes informally, made a property of the type.
6. **Help is 900×620 (1d's size); About and git status are 760 wide and
   content-height.** The artboards give no size for a short panel, and 1d's box
   around twelve lines would be mostly empty.
7. **The compare URL is `--fd-accent`, where the desktop's overlay uses green.**
   2g assigns `--fd-accent` to "interactive", and a link is the most interactive
   thing on a panel that otherwise only states facts. The desktop's green is
   ratatui's palette, not 2g's.
8. **`changed_files` is on the wire and rendered as `dirty · 6 files
   uncommitted`.** SPECS §21 asks only for "dirty/clean"; the count is the one
   number a reader immediately wants next to "dirty", and it is the host's, from
   the same porcelain read that set the flag. A turn could drop it.
9. **What is *not* shown.** SPECS §21 also lists "last push status, if known".
   The host does not track it — `WorktreeStatus` has no such field and the
   desktop's overlay does not show one either — so the browser shows nothing
   rather than inferring one from `ahead`. The two surfaces agree, and the gap
   is §21's to close on the host first.
10. **About renders the host's version, never the tab's.** D9 bakes the SPA into
    the binary, so a tab left open across an update is last version's JavaScript
    against this version's server; `Snapshot::about.version` is the host's
    `CARGO_PKG_VERSION`. A host that sends no About gets a sentence saying so.

#### 5. What is enforced rather than asserted

* `tokens.guard.test.ts` gained **rule 4 — the SPA's `PROTOCOL_VERSION` matches
  the host's.** The two constants are a hand-maintained mirror across two
  languages, and a stale mirror makes every version check pass while the wires
  differ — defeating the one thing the number is for. `webui/e2e/chain.spec.ts`
  would catch it, but that is the Playwright job, which R6 registered
  **non-blocking until 2026-09-10**; this is a file read and a regex that fails
  in `npm run test`. It also asserts `MIN`/`MAX` have not drifted open around
  `PROTOCOL_VERSION`, since a range would let a stale tab attach and *then* fail
  on the first frame it cannot parse. Verified to fail when the two are set
  apart.
* `tokens.guard.test.ts` gained **rule 3 — the state and view layers never read
  a clock.** Three module docs already said this (`reduce` is pure,
  `state/model.ts`'s "the same impurity one layer down", `wire/adapt.ts`'s "a
  confident guess"), and it was three prose rules with no check — the same shape
  rules 1 and 2 were in before that file existed. `wire/socket.ts` and
  `access/client.ts` are the asserted exemptions, both transport, both dating
  host instants against the host's own `server_time_ms`. Verified to fail when
  violated, as were the absence tests for the ahead/behind row, the host's help
  list and `?`'s App-mode gate.
* The panel's absences are tests, not conventions: no upstream renders
  `no-upstream` **and no ahead/behind row**; no compare URL renders **no compare
  row and no link**; a host that sent no help renders a sentence saying so and
  none of the host's rows; a refused `show_git_status` sends no panel at all.
* `a_browsers_git_status_answers_the_browser_and_never_the_desktop`
  (`src/lib.rs`) pins the routing decision in both directions in one test,
  because it is one rule and a test per direction would let half of it rot.

---

### R17 — the below-900px layout was derived, not designed, and here is the derivation

`remote-control-eek.4`. The narrow viewport was the last of §5's gaps and the
last of the epic. Artboard **1h** asserts it in one sentence and draws nothing:

> *"Below 900px the sidebar becomes a slide-over invoked from a session chip in
> the project row, and the git bar folds into the status bar."*

Design turn 3 (`remote-control-v4s`) was to draw it and **was not run, by the
repository owner's decision** — the same decision R16 records, applied to the
same instruction: derive these screens from the rules turns 1 and 2 already
establish rather than wait for a human-gated session. This entry is the
derivation, in the form a turn's hand-off would have taken, and like R16 it is
written so a later turn can overrule any single row without reconstructing why
the row is there.

**1h gave more than it looks like.** Its sentence names four things — a
breakpoint, a slide-over, where the slide-over is invoked from, and a fold —
and each of those had a precedent in the existing artboards to be built out of.
What it does not name is what happens to the *terminal*, and that is the part
the issue called the interesting constraint.

#### 1. D4, in the direction it was never written for

D4 says what a browser does when its viewport is **bigger** than the host's
grid: it letterboxes, it does not scale, and the `120×34 · host owns geometry`
chip is the honest explanation for the dark margins. It says nothing about
smaller, and until this task the stage answered smaller with `overflow: hidden`
plus `justify-content: center` — which **clips the host's grid at both edges,
silently**. On a 768px tablet against a 120-column grid, roughly the first ten
and last ten columns of the terminal did not exist, with nothing anywhere
admitting it. That is the one thing the letterbox is not allowed to be.

**The ruling: the stage scrolls.** Nothing is scaled, refitted or squeezed; the
grid keeps its natural pixel size and the viewport moves over it. Three things
follow, and all three are load-bearing:

* **The centring moved from the stage to the letterbox.** `margin: auto` on the
  flex item, not `align-items`/`justify-content` on the container. A *centred*
  flex item that overflows its scroll container overflows past the scroll
  origin, so its leading edge can never be scrolled to — the fix would have
  swapped clipping on both edges for clipping on one. Auto margins centre
  exactly as well while it fits and collapse to zero when it does not.
* **It is not a narrow-viewport rule.** A 200-column host grid overflows a 27"
  monitor, so the change lives in `main.css` beside the rest of D4 and applies
  at every width. `narrow.css` says nothing about the terminal at all.
* **The browser still does not measure itself.** No `ResizeObserver`, no
  `getBoundingClientRect`, no conditional "it does not fit" state. D4's position
  is that the browser never negotiates or requests a size, it only receives one,
  and a component that measured its own overflow would be one refactor from
  asking the host to match it. `tokens.guard.test.ts` rule 5 now forbids the
  measurement outright in `state/` and `ui/`.

**So how is the overflow *stated*?** By the scrollbar, and by the chip. The
stage asks for a non-overlay scrollbar (`scrollbar-width: thin`,
`scrollbar-color`), which Firefox and Chromium on Windows/Linux honour — but
macOS overlay scrollbars do not, and that is a real hole. It is closed in words
rather than by measurement: below 900px the geometry chip reads
`120×34 · host owns geometry · scroll, never scale`, and the chip's `title`
carries the full sentence at both widths. The clause is a statement of *policy*,
true whether or not this particular grid overflows this particular viewport,
which is exactly why it can be rendered without asking the DOM anything.

R4 is untouched: `sync_terminal_sizes` still calls `resize_if_changed` every
frame, the host still owns cols/rows, and `narrowScreen.test.ts` asserts that
crossing the breakpoint three times mounts the terminal **once**, at the host's
numbers.

#### 2. The breakpoint is a pure function, not a media query

Every other width-dependent thing in this app would have been a
`@media (max-width: 899px)`. It is not, and the reason is the same one rule 4 of
`tokens.guard.test.ts` was written for: **`vitest` runs in jsdom, which parses
media queries and never evaluates them.** A layout inside one would be checked
by nothing in `npm run test`, leaving it to `webui/e2e/narrow.spec.ts` — the
Playwright job R6 registered **non-blocking until 2026-09-10**. A rule nothing
checks is a rule that drifts, and this whole entry is a derivation nobody drew.

So: `main.ts` reads `window.innerWidth`, dispatches the **pixels**,
`state/viewport.ts`'s `widthClass` turns pixels into `wide | narrow`, and
`ui/app.ts` writes the answer to `data-width` on `.fd-frame`. That is the
existing idiom, not a new one — the frame already carries `data-mode`,
`data-layout`, `data-access`, `data-takeover`, `data-feed`, `data-dialog` and
`data-readonly`, and the narrow layout is the eighth member of the family. The
impurity arriving on the action and the decision living in the reducer is
`input/esc`'s split exactly (`at: number`, not "this was a double-tap").

`900` is 1h's number and **900 itself is wide**: "below" excludes the boundary,
and 900px still fits 1a's 300px column beside a terminal.

#### 3. What each piece was derived from

| Element | Derived from |
| --- | --- |
| The 900px breakpoint | 1h, verbatim |
| The sidebar as a slide-over, `hidden`-toggled, absolutely positioned, no `aria-modal`, no focus trap, no scrim | 2e, whose mechanism this reuses rather than inventing a second one. The sidebar is already an `<aside aria-label="Agents">`, which is 2e's `role="complementary"` by another route |
| `Esc` closes it; clicking outside closes it | 2e's feed (`Esc`) and 1a's advertised `click outside release keys` |
| Selecting a session closes it | 2e's own `jumpTo`, which closes the feed on a jump: the errand it was opened for is done |
| A chip in the project row opens it | 1h, verbatim |
| The chip shows the selected session's status glyph and name | 1a's sidebar rows, whose glyph and tone functions it shares — at this width the row is the only place the current session is named |
| `s` in App mode, and only there | 2e's rule for `a`, quoted in full: *"not in Terminal mode, where `a` is a letter the agent is waiting for"*. R16 took the same licence for `?` |
| The git bar and status bar as one bar | 1h, verbatim |
| The status line above the git line | 2c rule 3: the status bar's `border-top` **is** the state's frame colour, so it has to remain the top edge of the fold |
| The hints on a line of their own | 2c rule 1 (the connection strip never moves) plus 1h's own *"the status bar states both routes permanently — no discovery required"*: something must wrap, it may not be the strip, and it may not be dropped |
| Panels keep their designed widths; only the gutter shrinks, 80px → 24px | 2e's `min(470px, 100%)` — when the viewport runs out, the panel takes the width rather than the gutter keeping it |
| 1d's two palette columns stacked | the columns are still two in the DOM, so `Tab next column` still means what 1d's footer says |
| 1f's four cells restacked to two lines | 1f's own grid needs ~360px before the label gets a pixel |
| Tokens, four type sizes, no new colour | 2g, enforced by `tokens.guard.test.ts` |

#### 4. The calls the artboards do not cover

Each of these is a decision this task had to make. A later design turn may
overrule any of them.

1. **The slide-over comes from the left.** 2e is a right-edge slide-over, and
   the sidebar could have copied that literally. It does not, for two reasons:
   the left is where 1a's column is, so the panel arrives from where the thing
   it replaces used to be; and both panels can be open at once, so sharing an
   edge would mean one covering the other. **Not drawn by any artboard.** A turn
   could reasonably put both on the right and make them exclusive.
2. **`min(300px, 86%)` wide.** 300px is 1a's own sidebar width, kept rather than
   redesigned. The 14% left uncovered is 2e's trade — you can see the terminal
   is still behind it — at a width where 2e's own `100%` would have covered
   everything.
3. **`s` is the key, and it is bound only below 900px.** At wide the sidebar is
   always on screen, so a key that toggled nothing would be a key that lies.
   `s` is free in App mode and is the first letter of what it opens. **A turn is
   free to pick a different letter, or none** — the chip is a real `<button>`
   and is the only door a phone has anyway.
4. **The status bar was not changed to advertise `s`.** Same call R16 made for
   `?` and for the same reason: 1a/1b/2e draw that hint row, and adding a hint
   is an artboard change. The chip carries the key instead, the way 2e's feed
   header carries `a close`.
5. **The fold is `column-reverse`, not a DOM reorder.** The DOM order stays
   git-then-status at both widths, so a screen reader hears one order; the git
   bar holds nothing focusable, so no tab order is inverted; and the phone's
   bottom line — the one under the browser chrome and the home indicator — gets
   the git facts rather than "nothing you type is arriving".
6. **Nothing is dropped from either strip.** Every git fact and every hint the
   wide layout carries is still carried; the fold saves a border, a background
   and a rule, never a number. That is a deliberate refusal of the obvious
   alternative (elide `base: main`, elide a hint) and it costs one line of
   height at 768px. A turn could decide differently, and if it does the git
   status panel (R16) is where the elided facts already live.
7. **The project row scrolls horizontally, with the chip pinned.** Three project
   tabs, their separators and a badged `+ project` do not fit on a phone. A
   scrolling tab strip is the ordinary answer; `position: sticky` on the chip is
   what keeps the one control that opens the sidebar from scrolling out of reach.
8. **A click on the project row does not close the slide-over.** It holds the
   chip that opens it, so a click there is the panel's own control and not a
   click outside — and switching project with the list open is somebody asking
   to see *that* project's sessions, so the list stays up and repopulates.
9. **A click that dismisses the panel also focuses the terminal.** The read-only
   panels swallow that click (R16 §4.4); this one does not. On a touch screen,
   two taps for one intention is the bug.
10. **1c's split stacks into a column.** Three side-by-side terminals on a phone
    are three unreadable slivers. Each column still letterboxes and still
    scrolls its own grid. **Not drawn**, and cheap for a turn to overrule.
11. **2f's seat list wraps its `connected …` column onto a second line**, and
    the read-only panels' two fact grids (`190px 1fr`, `130px 1fr`) stack label
    over value. Both are the same judgement: the half carrying the meaning was
    being left with a hundred pixels.
12. **Only one breakpoint.** 1h names one and this task refused to invent a
    second, so a 380px phone and an 880px window get the same layout. If that is
    wrong it is a turn's to say, and the shape is ready for it: `widthClass`
    returns a union, and adding a third value is one function and one stylesheet
    section.

#### 5. `hidden` did not mean hidden, and the pixel test is what found it

This section is here because the first version of this work shipped a layout
that Playwright rejected, and the rejection was correct.

`e2e/narrow.spec.ts` could not click the terminal at 768px:

> `locator.click on '.fd-mount' timed out` …
> `<aside hidden class="fd-feed" data-open="false"> subtree intercepts pointer events`

**The closed activity feed was covering the right 470px of the terminal.** On a
tablet that is not a cosmetic defect: a user could not focus the terminal, could
not dismiss the sidebar by tapping past it, could not do anything with the thing
the app exists to show them — and nothing on screen said why, because the panel
was invisible.

**The cause was general and it was five bugs, not one.** `[hidden]` is a UA
stylesheet rule of the lowest possible specificity, and every overlay in this
app sets `display: flex` on a class selector, which beats it. So `.hidden =
true` closed nothing: the element stayed laid out, stayed painted and stayed
hit-testable. That had been defended **nine times**, one selector at a time, in
comments that literally read *"same trap as the one above"* — and five elements
never got the rule at all. An audit of every element any component toggles:

| Element | Toggled by | Consequence of the miss |
| --- | --- | --- |
| `.fd-feed` | `activityFeed.ts` | the live bug: a closed feed over the right 470px of the terminal |
| `.fd-pane__banner` | `terminalPane.ts` | an absolutely-positioned, 92%-opaque strip across the bottom of **every live terminal**, at every width |
| `.fd-tabs` | `app.ts`, in split layout | 1c drew the single pane's tab strip above the split |
| `.fd-pane` | `app.ts`, in split layout | 1c drew the single pane *and* the split, stacked |
| `.fd-split` | `app.ts`, on the way back to single | 1a drew the split as well |

Only the first was caused by this task; the other four are older, and three of
them are visible at any width. They are listed because they are the same bug,
and fixing them one at a time is what produced the situation in the first place.

**The fix is one rule, at the document level**, in `webui/src/style/app.css`:

```css
[hidden] {
  display: none !important;
}
```

and the nine per-component guards are **deleted**, so nobody can believe those
are what does the work and no future component has to remember. `!important` is
the mechanism here rather than a smell: `hidden` is the platform's own "this is
not relevant", and the whole failure was a component-level `display` outranking
it. Nothing in `src/style/` may carry a second one.

This is deliberately *not* a `pointer-events: none` patch. That would leave a
closed panel laid out and painted, fixing the symptom the trace named while
leaving the four rendering bugs above untouched — and it would have to be
remembered per component, which is the property that failed.

**What it changes about the entries above.** Nothing. No call in §4 is revised,
no token, size or word moves, and the narrow layout is what it was. `narrow.css`
lost its `.fd-sidebar[hidden]` rule to the general one, and `states.css` lost
eight; the only new fact is that "closed" is now true by construction.

#### 5b. And the keyboard was not reaching the app at all

Writing the test above turned up a second defect of exactly the same shape:
general, invisible to every unit test in the repository, and found only because
something drove a real browser.

The new test presses `a` to open the feed after clicking a chrome control, and
it did nothing. Measured rather than guessed — a throwaway spec that printed
`document.activeElement` at each step:

```
initial activeElement: BODY
after clicking the session chip: BODY
after Escape: sidebar still open
after 'a': feed still closed
after clicking .fd-mount: TEXTAREA.xterm-helper-textarea
```

**A keydown is delivered to listeners on the ancestors of the focused element.**
`ui/app.ts` attached its handler to `frame`, and `document.body` is an
*ancestor* of `.fd-frame`, not a descendant — so with focus on the body, every
key the app claims went nowhere.

That is the default state, not an edge case. `activeElement` is `BODY` on a
fresh load, and it returns to `BODY` whenever the focused control is removed
from the DOM — which every control in this app is, because each region rebuilds
its children on every render. So: **on a freshly loaded tab, no app-level key
worked at all** — not `Ctrl-g`, the one chord §5 gives the app; not `Esc Esc`;
not 2e's `a`; not R16's `?`; not 1h's `s` — until the user happened to click the
terminal, and clicking any chrome control silently took them away again. On a
tablet, where 1h's whole story is that the sidebar is reachable from a chip and
a key, that is the difference between a keyboard and no keyboard.

Nothing caught it because every keyboard test in the repository dispatches its
event on `app.el`, which is inside the frame by construction — the tests were
pressing keys somewhere the browser never does.

**The fix: the keyboard listens on the document; the pointer stays on the
frame.** Keys have no position, and their target is wherever focus happens to
be, which is not a component's business. Clicks do have a position and their
target is a real element, so the click handler is unchanged. One guard comes
with it — a frame that is no longer `isConnected` declines — so a torn-down app
stops answering, which matters in `vitest` where one file renders a dozen apps
into one document.

This one **jsdom can prove**, as long as the event is dispatched where a browser
would really put it, so `narrowScreen.test.ts` gained three tests that press
keys on `document.body`: §5's chord, the three App-mode plain keys, and the
`isConnected` guard. Verified to fail against the old handler.

**And what it says about the two test layers.** `ui/narrowScreen.test.ts`
asserted `.fd-feed` is `hidden` and passed — jsdom does no hit-testing, so to it
`hidden` is simply an attribute, and the entire class of bug is invisible from
`npm run test`. The keyboard one was worse: it was invisible because the tests
were pressing keys in a place a browser never focuses. That is the honest limit recorded at the end of this entry,
found the hard way rather than argued for. Both defences were added:
`tokens.guard.test.ts` rule 6 makes the *rule's existence* checkable in unit
tests, and `narrow.spec.ts`'s last test makes the *behaviour* checkable in a
browser — walking every overlay open and closed, asserting after each close
that no `[hidden]` element has a client rect, that `elementFromPoint` over a
grid of nine points on the terminal never lands inside one, and that the
terminal still takes a click.

#### 6. What is enforced rather than asserted

* `tokens.guard.test.ts` gained **rule 5 — the width decision is one pure
  function, and nothing measures itself.** No `min-width`/`max-width` media
  query anywhere in `src/style/` (comments stripped first, so a file may explain
  why it has none), and no `innerWidth`, `matchMedia`, `getBoundingClientRect`,
  `ResizeObserver`, `offsetWidth` or `clientWidth` under `state/` or `ui/`.
  `main.ts` is the one place that measures and it is under neither, so the rule
  needs no exemption list — which is why the measurement was put there.
* The **D4 letterbox block gained a positive assertion**, not just the existing
  negative ones: `.fd-stage` must be `overflow: auto` with no `justify-content`
  or `align-items`, and `.fd-letterbox` must be `margin: auto`. Putting either
  half back restores the silent clipping, and each half looks harmless on its
  own in a diff. `transform: scale` is now forbidden in `narrow.css` too — the
  one stylesheet whose entire subject is "the viewport is too small" is exactly
  where somebody would reach for it.
* `tokens.guard.test.ts` gained **rule 6 — a closed overlay is closed.** Three
  assertions, each verified to fail against the regression it names: `app.css`
  declares a bare `[hidden]` rule with `display: none !important`; no selector
  anywhere re-enables display on a `[hidden]` selector; and `!important` appears
  in exactly one file, so nothing can outrank it.
* `ui/narrowScreen.test.ts` presses three of the app's keys on
  `document.body` — the place a real browser leaves focus, and the place no
  other keyboard test in this repository has ever pressed one.
* `state/viewport.test.ts` pins 1h's boundary in both directions, including that
  900 is wide, and that crossing it in *either* direction closes the slide-over.
* `ui/narrowScreen.test.ts` (jsdom) drives the whole layout by dispatching a
  number, which is the payoff for not using a media query: the breakpoint, the
  slide-over's whole lifecycle, the fold's structure, every git fact and every
  hint surviving, and every overlay still opening and operating at 768px.

**What the unit tests do not prove, stated because it matters.** jsdom computes
no boxes. Nothing under `src/` can tell a `column-reverse` from a `column`,
notice that a panel overflows, or see that the sidebar is *over* the terminal
rather than beside it. `webui/e2e/narrow.spec.ts` does, at 768×1024, in a real
browser — including the assertion this task turns on: **the terminal is the same
number of pixels wide at 768 as it is at 1600**, with the same row count, and
the stage scrolls to both edges. A `FitAddon`, a `transform: scale` or a
`width: 100%` on the mount each fail that and none of them fails a jsdom test.
That job is still R6's non-blocking one until 2026-09-10, which is the honest
status of the pixel-level verification.

---

## 7. Reference

- `specs/WEBAPP_DESIGN_BRIEFING.md` — the design brief this implements.
- `specs/WEBAPP_DESIGN_BRIEFING_T2.md` — turn 2: access/QR (resolves Q1),
  connection and staleness states, takeover, activity feed, reference sheet.
- `specs/REMOTE_PROTOCOL.md` §8–§9 — the phone protocol we are deliberately *not*
  extending (D12).
- `specs/REMOTE_PROTOCOL.md` §7 — the phone's static-static P-256 ECDH and the key
  custody it rests on; the model D17 finds does not transfer to a browser.
- `remote/relay/src/queue.rs` — the per-pairing, envelope-counted, drop-oldest
  queue behind D17's second reason.
- `specs/SPECS.md` §19–§24 — terminal model, layout, git panel, interaction model,
  keyboard modes, status detection.
- `src/lib.rs:5387-5405` — `sync_terminal_sizes` / `resize_if_changed`, the per-frame
  geometry invariant behind D4.
- `src/lib.rs:2637` — the phone-opened-shell geometry precedent that does not
  generalise.
- `src/web/commands.rs` — the one table behind the browser's command surface
  (R7): wire name to palette action, `host only` badges, refusals.
- `src/tui/help.rs` — SPECS §23's help and the About screen as data, rendered by
  the desktop and sent to the browser (R16), so neither surface documents a
  keyboard the other does not have.
- `webui/src/state/viewport.ts` — 1h's 900px boundary as a pure function of a
  measured pixel width, and why it is not a media query (R17).
- `webui/src/style/narrow.css` — everything below 900px, keyed off `data-width`
  on `.fd-frame`: the slide-over sidebar, the fold, and the restacked panels.
- `src/web/arbiter.rs` — the input lock behind D14's second revision (R14): who
  may type, why 400 ms, and why no surface has precedence.
- `src/remote/client.rs` — the blocking relay client retired by D6/D7.
