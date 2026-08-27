# Design Briefing: FlightDeck Web — Turn 2

**Audience:** Claude Design, continuing the project "Web interface design
briefing" (`ba5af64b-7f47-4d58-a88e-6573ee172ac0`, file `FlightDeck Web.dc.html`).

**Turn 1 is accepted.** This document does not revisit it. It asks for the five
things turn 1 did not cover, all of which are now blocking implementation, and it
supplies the engineering decisions taken since turn 1 that constrain them.

**Read first:** `specs/WEBAPP_DESIGN_BRIEFING.md` (the original brief — still
governs visual and UX intent) and `specs/WEB_INTERFACE.md` (the decision log,
cited below as D1–D16 and Q1–Q6).

---

## 1. What turn 1 settled, and what we adopted

Turn 1 delivered artboards 1a–1h and took four positions in 1h. **All four are
adopted as-is** and are now requirements, not proposals:

- **Palette-primary.** `Ctrl-g` is the only chord the app claims; every other
  command is reachable by searchable name, with the desktop key shown beside it.
- **Leaving terminal focus:** click outside the viewport, or double-tap `Esc`
  within 400ms. A single `Esc` still passes through to the hosted agent.
- **Dark only**, with the stated argument: the main pane is a VT100 surface
  rendering ANSI colours the agent chose against a dark ground.
- **Sidebar mirrors for `agent_tab_position = right`;** below 900px it becomes a
  slide-over and the git bar folds into the status bar.

Turn 1 also established the whole visual language this turn must extend — the
1px accent frame with a titled header, keyed buttons where every button shows its
key, the `▸` selection marker with a 2px inset stripe, `host only` badges, and
the recessed `#050c16` chrome strips above and below the terminal.

---

## 2. Engineering decisions that constrain this turn

These were taken after turn 1 and change what the screens must say. The full
rationale for each is in `specs/WEB_INTERFACE.md`.

| # | Decision | Consequence for design |
| --- | --- | --- |
| **D1** | Embedded server only — no relay, no browser-side crypto. Reaching it from outside the LAN is the user's own tunnel. | There is no cloud account, no sign-in, no relay pairing. Access is a **local network question**, and the UI must be honest that leaving the network is out of scope. |
| **D4** | **The desktop always owns PTY geometry.** The browser scales the fixed grid to fit. | The browser can never resize a terminal. A large window under-uses its space, and the design should decide how that reads — letterbox, or scale up. |
| **D5** | Bearer token; **binds `127.0.0.1` by default**, `0.0.0.0` is an explicit opt-in with a warning. | This is the whole of §3 below, and the reason Q1 exists. |
| **D10** | Palette start/stop, plus a config opt-in to auto-start. **The token persists so bookmarks keep working.** | Access must survive restarts, which conflicts with a short-lived pairing code. §3 proposes the reconciliation. |
| **D11** | **In-app activity feed only.** Web Push is structurally blocked under D1 (it needs a publicly reachable sender; a loopback server has none). | The feed is the *entire* substitute for OS notifications. It carries more weight than a convenience log. §6. |
| **D14** | **One browser viewer at a time** in M1. A second attach is offered a takeover. | §5. Also: the status bar's viewer count is always "desktop + at most one browser", so it must not imply more. |
| **D16** | Desktop-only actions keep their `host only` badge and stay visible. | Already solved by turn 1; carried forward unchanged. |

**Milestone 1** is observation plus keystrokes into the focused terminal. No
palette, no dialogs, no git commands, no configuration manager from the browser.
Items 2a–2d below are all M1 blockers; 2e serves everything.

---

## 3. Deliverable 2a — Access, and the QR problem (resolves Q1)

### The problem

D5 binds loopback by default, but the turn-1 vocabulary inherits the phone
remote's QR-plus-4-digit-code overlay. **A QR encoding `http://127.0.0.1:7420` is
useless** — the only device that can open it is the one already displaying it.
Meanwhile a QR is exactly right the moment the server is reachable from a phone
on the same wifi. So the overlay has two genuinely different jobs, and the
transition between them is part of the flow rather than a settings detour.

### What we propose (please refine or reject with reasons)

**A single "Web interface" overlay with two states**, invoked by the palette
command `Start Web Interface`.

**State A — local only (the default).** The server is on `127.0.0.1`. Nothing
here is a credential worth hiding, because nothing off-machine can reach it.

- The primary action is **Open in browser** — FlightDeck launches the default
  browser, already authenticated. In the common case the user never sees a code
  at all, and that is the point.
- A copyable URL, for a different browser or a second window.
- A door to State B, stating the consequence rather than a bare toggle:
  *"Allow other devices on this network to connect"*.

**State B — network access enabled.** Now the QR earns its place.

- QR encoding `http://<lan-ip>:7420/#<code>`, in turn 1's idiom.
- The short code in large type, for someone typing it on a laptop.
- **Which address**, explicitly. Machines have several interfaces and the design
  should assume we may need to show a choice, not just a guess.
- An honest warning. Not "this may be insecure" — something closer to *"anyone on
  this network who has the code can read your repositories, type into your
  agents, and push branches."* Turn 1's destructive-dialog copy discipline (1g:
  name the consequence, never soften to "are you sure?") applies here.
- A way back to local-only, and a way to **revoke or rotate** the credential.

### The credential model to design around (proposed answer to Q4)

D10 needs bookmarks to keep working; leaving a permanent code on screen is a
poor idea. These reconcile if the two are separated:

```
short code   bootstrap credential, ~120s, shown on the desktop
                  │  browser exchanges it once
                  ▼
persistent   HttpOnly cookie in that browser — survives restarts,
             revocable from the desktop, invisible thereafter
```

So the overlay shows a **short-lived code with a countdown** (matching the phone
pairing overlay's existing countdown), while the *browser* holds long-lived
access. The token also travels in the URL fragment on first visit and is stripped
after exchange, so it stays out of history and server logs.

**Please take a position on one security tradeoff:** should the QR encode the
code (instant, but the code is then legible to anyone seeing the screen — a real
hazard while screen-sharing, which we cannot detect), or only the URL, forcing
the code to be typed? We lean toward encoding it, given the 120s window, but this
is a judgement call worth your argument.

### Browser-side screens needed

1. **Code entry** — reached by a bookmark whose cookie is gone, a typed address,
   or a rotated credential. Needs to explain where to find the code without
   assuming the user is at the desktop.
2. **Rejected** — wrong or expired code, with the recovery path (go to the
   desktop, run the palette command again).
3. **Revoked** — access was withdrawn from the desktop while the tab was open.
   This is distinct from a network failure and must not look like one.

---

## 4. Deliverable 2b — Connection and staleness

Turn 1 established the indicator: `● connected 18ms`, green dot `#5ddc9a`, label
`#8a9cb8`, latency `#4d5f79`, right-aligned in the status bar. That is the
connected case. **The other states are undesigned, and they are the ones that
protect the user from acting on a lie.**

### States to design

| State | What the user needs to know |
| --- | --- |
| **Connecting** | First attach, or a deliberate reconnect. Brief; should not flash alarmingly. |
| **Reconnecting** | It broke and we are retrying. Attempt count or next-retry timing — enough to tell "working on it" from "stuck". |
| **Disconnected** | We stopped trying, or the host is gone. Needs a retry affordance and a plain reason. |
| **Version mismatch** | The SPA is baked into the binary (D9), so a `flightdeck update` on the host while a tab is open leaves that tab running old code. Needs a "reload to update" state — and note the main screen already carries an update-available chip, so these two must not be confused. |
| **Unauthorized / revoked** | Covered in §3, but its *indicator* state lives here. |
| **Host quit** | Open question **Q5**: FlightDeck was shut down, possibly by `Ctrl-q` from this very browser. Please propose what the browser shows. |

### Terminal staleness — the constraint that matters

When the link drops, the terminal on screen is a **photograph, not a window**.
The user must not be able to mistake one for the other, because the failure mode
is typing into a terminal that stopped receiving twenty seconds ago.

**The obvious treatment is already taken.** Turn 1's App-mode "asleep" state
uses flat `#5d6f8a` with all bold and per-token colour removed, plus an
explanatory footer strip on `#050c16`:

> `terminal asleep — keystrokes go to FlightDeck ·` **`Enter`** `or click to wake it`

Stale must therefore be **distinguishable from asleep at a glance** — and note
the two **co-occur**: a disconnected browser sitting in App mode is both. A
treatment that simply reuses desaturation will collide. Worth considering: a hue
cast rather than a desaturation, a hatch or scanline veil, a frozen-timestamp
badge, or an explicit *"last update 34s ago"*. Your call; the requirement is only
that asleep, stale, and asleep-and-stale are three legible states.

Also needed: the **catching-up** state. Q3 leans toward resume-from-byte-cursor
on reconnect, so there is a moment where the terminal is live again but still
replaying. Say what that looks like.

---

## 5. Deliverable 2c — Takeover (D14)

M1 accepts one browser. A second attach is offered the incumbent's seat.

**Be aware of what this is.** Anyone holding the credential can evict anyone
else, so this is **not** a security boundary — it is clarity, so that neither
person is silently confused about why their input stopped working. Design it as
courtesy, not as a permission check, and do not imply an authority it lacks.

Three surfaces:

1. **The arriving browser.** "Another browser is controlling this instance." What
   identifying detail is fair and useful — IP, browser/OS, how long it has been
   connected? Then `Take over` / `Cancel`. If they cancel, is there a read-only
   fallback, or nothing?
2. **The evicted browser.** It loses control mid-session, possibly mid-keystroke.
   It must say so unmistakably, and say whether reclaiming is one click or a
   round trip to the desktop.
3. **The desktop.** D13 establishes origin labels for browser-initiated dialogs
   (`opened from browser · 192.168.2.20`). A takeover is arguably worth a
   transient notice in the same vocabulary. Propose whether it deserves more.

Related: the status bar's `2 viewers (this tab + desktop)` chip should be honest
under D14 — the count cannot exceed two in M1. If the chip's shape implies a
growing list, adjust it, or design the M3 multi-viewer form now and note that M1
renders a degenerate case of it.

---

## 6. Deliverable 2d — The activity feed (D11)

**This is the whole substitute for OS notifications**, not a nicety. The stated
core event of the product — *"an agent I wasn't looking at just finished, or just
got stuck"* — has no other channel in the browser. Web Push is structurally
unavailable under D1.

### The hard part: turn 1's main screen has no spare room

Every row in 1a is allocated: logo band, project tabs, terminal tabs, viewport,
git bar, status bar. The feed must therefore be an **overlay, popover or
slide-over**, or take space from something that can spare it. Choosing where it
lives, and what invokes it, is the substance of this deliverable.

### Requirements

- **Global across projects**, with project attribution. The whole point is the
  session you were *not* looking at, which is frequently in another project.
- **Entries are status transitions** in the established vocabulary and colours:
  `in progress` / `idle` / `waiting` / `error`, plus manual overrides. Reuse the
  §7 semantics from the original brief exactly; do not invent a second status
  language.
- **Unread must be visible without opening it.** This replaces a notification
  that used to arrive with a sound. It has to be noticeable while not being a
  modal — the original brief's rule that the update notice "never becomes a
  modal" is the right precedent.
- **Actionable.** Clicking an entry should select that session. Note the
  consequence under D3: shared view state means that **moves the desktop's
  selection too**. Decide whether that needs acknowledging in the interaction.
- **Backfill.** A freshly opened tab should show recent history, not an empty
  list, so the host keeps the ring buffer rather than the tab. Say how far back
  is useful and what an empty state says on a genuinely quiet instance.
- **Relationship to the desktop.** The host still posts its own OS
  notifications with sounds. The feed is an addition, not a replacement, and
  should not claim to be the only record.

---

## 7. Deliverable 2e — Colour and type reference sheet

Largely **formalisation, not invention**: turn 1 already committed to a complete
and coherent palette. Extracted from `FlightDeck Web.dc.html`, with WCAG contrast
against the two grounds it is used on:

| Token | Role in turn 1 | on `#07111f` | on `#050c16` |
| --- | --- | --- | --- |
| `#edf4ff` | primary text, selected rows | 17.1:1 | 17.7:1 |
| `#dce7f7` | setting labels (config manager) | — | — |
| `#a9b8cf` | muted prose, inactive tab labels | 9.4:1 | 9.8:1 |
| `#8a9cb8` | secondary text, connection label | 6.8:1 | 7.0:1 |
| `#6d819d` | agent display name | 4.8:1 | 4.9:1 |
| `#5f7391` | eyebrow / small caps | 3.9:1 | 4.1:1 |
| `#5d6f8a` | **dimmed terminal text (App mode)** | 3.7:1 | 3.8:1 |
| `#55647d` | inactive terminal tab | 3.2:1 | 3.3:1 |
| `#4d5f79` | **dim / unknown: `no-upstream`, `git: ?`, `·set`** | **2.9:1** | **3.0:1** |
| `#f5d76e` | yellow — selection, focus, key names, active agent tab | 13.4:1 | 13.9:1 |
| `#61dafb` | cyan — accent, interactive, upstream, manual override | 11.7:1 | 12.1:1 |
| `#5ddc9a` | green — idle, healthy, additions, connected | 11.0:1 | 11.4:1 |
| `#d68bf5` | magenta — `drift:N`, `[recovered]`, App-mode key | 8.0:1 | 8.3:1 |
| `#6aa8ff` | blue — `⎇` branch glyph, `(global)` origin tag | 7.8:1 | 8.1:1 |
| `#ff6b6b` | red — attention, error, `✕`, working spinner | 6.8:1 | 7.1:1 |
| `#24374f` | separator `│`, outer frame rules | 1.6:1 | 1.6:1 |
| `#16233a` | frame border | — | — |
| `#101d31` | internal row dividers | — | — |
| `#07111f` | frame ground (matches the marketing site) | — | — |
| `#050c16` | recessed chrome strips (tab bars, git bar, footers) | — | — |
| `#04090f` | canvas behind the frames | — | — |
| `rgb(16,38,68)` | active project tab — **carried verbatim from the TUI** | — | — |
| `#3a5a8a` | terminal selection — **`rgb(58,90,138)` from the TUI** | — | — |

Type is a single family, `JetBrains Mono`, at 10.5 / 11 / 11.5 / 12 / 12.5 / 13 /
14px.

### What we need from this deliverable

1. **Semantic names** for these values, so implementation ships CSS custom
   properties rather than 54 literals. The hues already carry consistent meaning;
   name the meaning, not the colour.
2. **Fill the gaps this turn creates:** the connection states (§4), activity
   severity (§6), the takeover states (§5), and the stale-terminal treatment —
   which needs a token that cannot be confused with `#5d6f8a`.
3. **A position on the dim tier.** Four tokens carrying real information fall
   below WCAG AA for body text (4.5:1), and `#4d5f79` at 2.9:1 misses even the
   3:1 floor for large text and non-text UI. This is not pedantry: `#4d5f79`
   renders `no-upstream` and `git: ?`, which are load-bearing git facts, and
   `#5d6f8a` renders an **entire terminal** in App mode. Either lift the tier,
   or state deliberately which of these are decorative enough to stay — but the
   choice should be made rather than inherited.
4. **A position on the type scale.** Seven sizes inside a 3.5px range is a lot.
   Consolidate, or justify each step.

---

## 8. What we would like back, in priority order

1. **The access overlay, both states** (§3) — local-only and network-enabled,
   with the transition between them and the warning copy.
2. **The three browser-side access screens** (§3) — code entry, rejected,
   revoked.
3. **The connection indicator in every state** (§4), as a strip of variants
   against the real status bar.
4. **The stale terminal**, shown beside App-mode "asleep" and in the combined
   state, so the three are provably distinguishable (§4).
5. **The activity feed** (§6), including where it lives on the main screen, its
   unread state, and its empty state.
6. **The takeover trio** (§5) — arriving, evicted, desktop notice.
7. **The reference sheet** (§7), with semantic names and a stated position on the
   dim tier.
8. **The catching-up / resume state** (§4), if it does not fall out of item 4.

Everything should sit on the same 1520×880 frame turn 1 used, so the artboards
remain comparable.

---

## 9. Reference

- `specs/WEBAPP_DESIGN_BRIEFING.md` — the original brief. §7 (status semantics
  and colour), §8 (states that must be designed), §10 (safety invariants) are
  the ones this turn leans on hardest.
- `specs/WEB_INTERFACE.md` — decisions D1–D16 with their costs, and the open
  questions Q1–Q6 this turn is asked to resolve or inform (Q1 in §3, Q3 and Q5
  in §4, Q4 in §3).
- `FlightDeck Web.dc.html` in the design project — turn 1, artboards 1a–1h.
- `specs/screenshot.png` — the desktop TUI, for status vocabulary.
