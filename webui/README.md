# FlightDeck Web — `webui/`

The browser control surface for FlightDeck (`specs/WEB_INTERFACE.md`, decision
**D9**). Vite + TypeScript + xterm.js, no framework. Built to static assets and
baked into the desktop binary with `rust-embed` (`src/web/assets.rs`), so a
release stays a single file and the server never resolves paths on disk.

The main screen (artboards 1a Terminal mode, 1b App mode, 1c split view) is
built: seven regions rendered from a **fixture snapshot**, driven entirely
through the reducer. So are the turn-2 screens — the four access screens (2b),
every connection state (2c), the five terminal treatments (2d), the activity
feed (2e) and the takeover trio (2f). One task still plugs in without touching
a component: `remote-control-hgqy` swaps the fixture for the live websocket
(see "Where the socket goes" below).

## The turn-2 screens

Three overlay layers on the same frame, plus one rewritten status bar. All four
are state-driven — nothing keeps its own visibility in the DOM — so every rule
below is a unit test rather than a screenshot.

| Layer | Artboard | Opened by |
| --- | --- | --- |
| `ui/accessScreen.ts` | 2b, four screens | `state.access !== null` |
| `ui/takeover.ts` | 2f, arriving + evicted | `state.takeover !== null` |
| `ui/activityFeed.ts` | 2e, right-edge slide-over | `a` in App mode, or the unread chip |
| `ui/statusBar.ts` | 2c, nine rows | every state |

The vocabulary lives in pure modules, one per artboard, so the *rules* are
testable without a DOM: `state/connection.ts` (2c's strip and 2d's pane tone),
`state/access.ts` (2b's copy), `state/activity.ts` (2e's tier precedence),
`state/seats.ts` (the viewer chip). `access/bootstrap.ts` and `access/client.ts`
are the two things that touch the outside world — the URL fragment and the two
auth routes.

### Five rules that are easy to break by accident

1. **`Shutdown` never shows "reconnecting" (Q5).** Enforced in the *reducer*,
   not in the transport: once `state.shutdown` holds a non-retryable reason,
   `connection/changed → reconnecting` is refused. A transport that keeps
   dialling still cannot paint a lie. Only `ShutdownReason::Restarting` retries,
   and an unknown reason does **not**.
2. **Input is queued, in order, exactly once (§5.1).** `pendingInput` carries
   the keystrokes and `inputSeq` carries the seq of the last one — one number,
   not two that can drift. On a reattach, `input/acked { throughSeq }` drops the
   prefix `Snapshot { last_input_seq }` says the host already applied, and
   re-sends the rest in order. `input/esc` takes a seq like any other keystroke;
   forgetting that was a real bug the test caught.
3. **The connection strip never moves.** The mechanism is structural:
   `.fd-spacer` is always the element immediately before `.fd-conn`. Do not add
   a second spacer after it, and do not give the connection group a margin.
4. **The desktop's seat row is always `Seat::Controlling`.** A "find the
   controller" that only looks at `seat` finds two rows and names the desktop as
   the browser that evicted you. Ask `webController()` — a viewer *and*
   controlling — and nothing else.
5. **The feed is never a modal.** `role="complementary"`, no `aria-modal`, no
   focus trap, no click-swallowing scrim. D11 makes it the only notification
   channel there is, so a version that blocked the screen would interrupt the
   terminal you are reading to tell you about one you are not.

### One deliberate deviation from the plan

The access screens were specced as "a screen chooser above `createApp`". They
shipped as a **layer inside the frame** instead, because 2b draws all three
panels that way: logo band above, footer strip below, and a running agent
visible behind the revoked one — which 2b describes in words as *"a photograph
from the moment access ended"*. A sibling screen could not show that, and would
have had to reproduce the band and the footer to look right. `data-access` on
the frame hides the git bar and status bar, which have nothing honest to say
with no session.

## Gate commands

This is the third build surface in the repo, alongside the root Rust crate
and the `remote/` cargo workspace — see
`.agents/skills/shipping-flightdeck-changes`. From a clean checkout:

```bash
npm install
npm run build        # -> dist/, which the Rust build embeds
npx tsc --noEmit      # strict typecheck, no emit
npm run test          # vitest (D15): the reducer (both halves), the status and
                      # connection vocabularies, Esc-Esc timing, the tier
                      # precedence, the fragment/auth path, the seven regions,
                      # the turn-2 screens, and the palette guard
```

`npm run dev` starts a Vite dev server for iteration; it is not part of the
gate. `npm run test:watch` runs vitest in watch mode.

The Rust side does **not** require `npm run build` to have run — `webui/dist/`
ships a tracked `.gitkeep` so `rust-embed` has a folder to compile against even
on a fresh checkout with no npm build. Without a real build, `assets::lookup`
returns an honest "webui was not built" response instead of a blank page or a
panic. See `src/web/assets.rs`.

## The palette rule

`src/style/tokens.css` ships the **named** semantic palette from
`specs/design/flightdeck-web-turn2.dc.html` artboard `2g — REFERENCE SHEET`,
as CSS custom properties (`--fd-text`, `--fd-stale`, `--fd-term-asleep`, …),
each annotated with the meaning 2g documents. That artboard supersedes the
extracted table in `specs/WEBAPP_DESIGN_BRIEFING_T2.md` §7 — read the artboard
if the two ever disagree.

**Never ship a colour literal for anything the palette already names.** The
rule worth quoting into code review, straight from 2g: *if deleting it would
lose a fact, it cannot be `--fd-text-decor`.* `--fd-text-decor` and
`--fd-text-quiet` look similar on screen and are not interchangeable —
`--fd-text-quiet` is the lifted floor for anything a user could act on
wrongly (`no-upstream`, `git: ?`, an agent's display name); `--fd-text-decor`
is decoration only (key-hint letters, counts, the `│` separator).

## Where the socket goes

Nothing in `src/` opens a connection. The whole screen renders from
`src/state/fixture.ts`, which is typed as the *exact* payload the
`snapshot/received` action carries, so the live socket is a change of source,
not of shape:

| Today | With `remote-control-hgqy` |
| --- | --- |
| `fixtureSnapshot()` → `snapshot/received` | `ServerMsg::Snapshot` → `snapshot/received` |
| `fixtureTerminalBytes()` → `term.write` | `ServerMsg::Delta` → `term.write` |
| `createApp({ onDispatch })` logs a selection | `onDispatch` emits `ClientMsg::Command` (D3) |
| `state.pendingInput` accumulates | flushed as ordered `ClientMsg::Input` (§5.1) |
| `connection: "connected"` set at boot | `connection/changed` from the transport |
| — | `ServerMsg::Shutdown` → `connection/shutdown` (Q5) |
| — | `Delta::Seats` → `seats/changed` / `takeover/evicted` |
| — | `ErrorCode::SeatHeld` → `takeover/held` |
| — | `Delta::Activity` → `activity/received` |
| — | `Ack` / `Snapshot { last_input_seq }` → `input/acked` |
| — | `check_version()` mismatch → `version/mismatch` |

The access half is **already live**, not a fixture: `src/main.ts` consumes the
bootstrap code from the URL fragment, strips it from history, exchanges it at
`POST /auth/exchange`, and asks `GET /auth/session` whether an existing cookie
still works before anything opens a socket.

`src/ui/app.ts` carries the same table as a doc comment, next to the one
behaviour that has to move when input goes live: the `Esc` handler must become
xterm's `attachCustomKeyEventHandler` so it stays the single authority.

## Font

Single family, **JetBrains Mono**, vendored rather than linked from a CDN.
Three static weights (400/500/700, Latin subset, WOFF2 only) are committed
under `public/fonts/`, sourced from the upstream
`@fontsource/jetbrains-mono` npm package (glyphs unmodified, just subsetted;
SIL OFL 1.1, license copied to `public/fonts/JETBRAINS-MONO-LICENSE.txt`). See
`src/style/fonts.css` for the full rationale — in short: this app ships inside
a binary that may be running on a loopback-only or LAN-only host (D1/D5), so a
CDN `@font-face` or a runtime font fetch would silently degrade exactly the
deployment this server exists for. `--fd-font-mono` in `tokens.css` is the one
place code should reach for the family; it falls back to the platform
monospace stack if the vendored font somehow fails to load.

## The no-`FitAddon` invariant

`specs/WEB_INTERFACE.md` D4, as revised by turn 2: **the desktop always owns
PTY geometry**, and the browser *letterboxes* the host's fixed grid — it does
not scale or refit it. `sync_selected_tab_sizes` calls `resize_if_changed`
(`src/lib.rs:5389`) every frame for the selected tab, so any size the browser
claimed for itself would be reverted within one frame.

Concretely: **`@xterm/addon-fit`'s `FitAddon` must never be imported anywhere
in this app.** `src/term/terminal.ts` constructs xterm.js with `cols`/`rows`
taken verbatim from the host and never calls `terminal.resize()` in response
to a container/window resize — only a new geometry value from the host may
resize it. If you find yourself reaching for `FitAddon` to make a terminal
"fill its container", that is exactly the regression D4 turn 2 exists to
prevent; letterboxing (dark margin, not a scaled bitmap) is the intended
behaviour, not a bug to fix.

## Layout

```
webui/
├── index.html          entry point
├── public/fonts/        vendored JetBrains Mono (WOFF2, Latin subset)
├── src/
│   ├── main.ts          entry point: fixture in, main screen out
│   ├── state/           model.ts, status.ts, fixture.ts + the reducer seam
│   ├── input/           escape.ts — the 400 ms `Esc Esc` window (§5)
│   ├── access/          bootstrap.ts (Q4's fragment), client.ts (the two routes)
│   ├── ui/              the seven regions + three overlay layers, one file each
│   ├── term/            xterm.js construction (terminal.ts) — no FitAddon
│   └── style/           tokens.css (palette + type scale), main.css,
│                         states.css (2b–2f), fonts.css
├── dist/.gitkeep         tracked placeholder; dist/* itself is gitignored
├── vite.config.ts        also the vitest config (env: node; DOM per-file)
└── tsconfig.json         strict
```
