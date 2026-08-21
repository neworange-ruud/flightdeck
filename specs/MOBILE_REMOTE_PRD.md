# Product Requirements Document — FlightDeck Remote (iOS)

> **Status:** v1.0 — finalized. Extracted from `MOBILE_REMOTE_BRIEFING.md` and the
> `FlightDeck Remote.dc.html` UI design, then narrowed through a 3-round interview
> (log in §14). Remaining **[TBD]** items are explicitly-deferred, non-blocking
> details (e.g. token lifetime, multiple-phones-per-Mac).

---

## 1. Overview

**FlightDeck Remote** is an iOS companion app that puts FlightDeck's
mission-control loop in the developer's pocket. FlightDeck is a desktop terminal
application that runs several AI coding agents (Claude Code, OpenCode, Codex CLI)
in parallel, each in its own isolated Git worktree/branch. The developer's job is
**supervision, not typing**: launch agents, watch which are working / idle / stuck,
and step in when one needs attention.

The remote exists for a single recurring moment:

> **"An agent I launched is now working somewhere else; tell me the moment it
> finishes or gets stuck, let me see what it did, and let me answer it — from my
> phone, without being at my desk."**

The core loop the app must nail: **push notification → tap → read the agent's
state → type/speak a reply or approve → done.** Everything else (starting agents,
git pushes, multi-project management, shell access) is supporting cast around that
loop.

### 1.1 Platform & positioning

| Attribute | Value |
|-----------|-------|
| Platform | iOS 18, iPhone (v1). iPad follows later; Android/Watch not planned for v1. |
| Theme | "Midnight mission-control" — dark, midnight-green field + orange signal |
| Typography | Geist (UI), Geist Mono (code/terminal/git) |
| Brand system | New Orange |
| Relationship | Companion/remote to the FlightDeck desktop app; not standalone |

---

## 2. Users & jobs-to-be-done

**Primary user:** a developer running agentic coding sessions — several agents
grinding on different tasks (fix a bug, add tests, port to a platform, review the
codebase) simultaneously on the same project, while away from their desk.

Jobs-to-be-done, in priority order:

1. **Be told the moment an agent needs me** (finished, or stuck asking a question)
   and get straight to it.
2. **See the state of everything** — which projects/agents are working, idle, or
   waiting — at a glance, rolled up.
3. **Read what an agent did** and **answer it** — reply, follow-up, approve/deny a
   permission — by text or voice.
4. **Light control** — start a new agent, restart/stop one, switch context.
5. **Drop into a real shell** in a worktree when the agent can't unstick itself.
6. **Git actions** — check status; abandon a worktree, pull base, merge back.
   (Pushing / opening the PR is the agent's job, not the remote's — see §5.5.)

---

## 3. Core concepts & hierarchy

The app mirrors FlightDeck's strict three-level hierarchy and must make it
navigable and legible:

```
Projects (top)  →  Agent sessions (middle)  →  Terminals within a session (bottom)
   project tab        one worktree/branch/agent      1 agent terminal + optional shells
```

- **Project** — a repository folder. A user can have several open at once; every
  open project stays **live in the background** (agents keep running and notifying
  even when viewing another project).
- **Agent session** ("Agent Tab") — the primary unit: **1 worktree = 1 branch = 1
  primary agent process** (+ optional shell terminals). This is what the user
  thinks in.
- **Terminal** — each session has one *agent terminal* (surfaced as a cleaned
  chat) plus optionally shell terminals (surfaced as a faithful ANSI terminal).

**Mental model rule:** `Each Agent Tab = 1 Worktree = 1 Branch = 1 Primary Agent
Process (+ optional shell terminals)`.

---

## 4. Status model (the primary information)

Status is the app's most important content. FlightDeck's color language is
inherited exactly:

| State | Color | Meaning | Behavior |
|-------|-------|---------|----------|
| 🔴 **Working** | Red `#E5484D` (animated spinner) | Agent is actively running a turn | "Leave it alone, it's busy." |
| 🟢 **Idle / finished** | Green `#6FB26C` | Turn done, waiting for a prompt | "Ready for me, or done." |
| 🟠 **Needs input** | Orange `#FF6601` (glow) | Agent stopped, asking the human (permission / question) | **Most urgent — pulls the user in.** Distinct notification + sound. |
| 🔵 **Manual override** | Cyan `#4FB3C4` | User-flagged status | Minor, but exists. |

**Roll-up:** status propagates up the hierarchy. An agent's status shows on its
session row; each project summarizes its agents into **one dot + a plain-language
summary** (e.g. "1 needs input · 1 working · 3 agents", "1 done, ready to push",
"idle · 2 agents"). The project list uses this so a glance says "where is the
fire."

**Roll-up precedence (from design):** a project dot shows orange if any agent
needs input; red if an agent is working; green/dim if idle.

---

## 5. Screen inventory (from the UI design)

The design delivers the following screens across six sections. Each is a required
surface unless noted.

### 5.1 App icon & home screen
- **Chosen icon:** "Radar & blip" — midnight-green field, orange radar sweep + a
  blip (the agent that needs you). Two brand colors, no wordmark.
- Alternates explored (not chosen): "Ascend / flight deck", "Control tower".
- **Home-screen badge** = count of agents waiting for input (e.g. `3`).

### 5.2 Core loop
- **Lock screen / notifications** — typed notifications:
  - *Needs input* (orange border): `"fix-login needs your input — "Allow rm -rf
    dist/ ?" · flightdeck"`.
  - *Finished* (green border): `"add-tests finished its turn — 18 files changed ·
    ready to push · SpecAssistant"`.
  - Both **deep-link straight to the agent**.
- **Projects list** — title "Projects", subtitle roll-up ("1 project needs you · 2
  working"), one card per project with status dot + colored left accent for the
  one needing attention + status pills + agent count + chevron. Search affordance
  top-right. Background projects stay live.
- **Agent sessions list** — per project. Back-to-Projects, project name + dot +
  agent count. One card per session showing: name, agent type (Claude Code /
  OpenCode / Codex), status (idle / working-spinner / "NEEDS YOU" pill), compact
  git indicators (`~3 drift:2`, `+12 ~4`, `clean`, `no-upstream`), running time,
  and a **preview of what a waiting agent is asking**. Primary CTA: **New agent
  session**.

### 5.3 Agent chat — two directions (distinct from shell)
- **3a — Cleaned transcript (CHOSEN):** the agent conversation as a *cleaned
  transcript*, never a raw terminal. Noisy tool calls **collapse into tappable
  activity pills** ("Edited auth.ts +18 −4 · tap to expand", "Ran npm test · 42
  passed"). Prose/decisions stay readable. Includes a per-session **surface
  switcher** (`Agent · Shell`) toggling between chat and a real terminal in the
  same worktree. **Permission asks resolve inline** ("Permission needed — `$ rm -rf
  dist/` — Allow once / Deny", "or say 'approve' · hold mic below"). Compose field
  ("Reply to fix-login…") with mic.
- **3b — Focus / eyes-free:** pending question pinned large with **Read aloud** +
  big **Approve / Deny** buttons; history condenses to a timeline ("Patched
  auth.ts — await the refresh 9:38", "Ran npm test — 42 passed 9:39", "Wants to
  clean the build output — now"). "Hold to reply by voice" + "Type instead".
- **Voice compose:** hold-to-talk drops a transcript into an **editable** field
  ("Listening…" → "Yes, run it. Then rebuild…" → "Send to agent"). **Edit-before-
  send, always** — raw dictation is never auto-sent.

### 5.4 Shell terminal

> **v1 scope decision:** the full ANSI-over-real-PTY emulator is **deferred**. v1
> ships a **minimal working terminal**: send a command, stream stdout/stderr with
> basic ANSI colors and scrollback, **`Ctrl-C` to interrupt**, and **copy/paste**
> (both directions) — enough to run a test, check `git status`/`git log`, or tail a
> log. **Interactive/full-screen programs (`vim`, `top`, `less`), full cursor
> addressing, and a complete ANSI/PTY emulator are a fast-follow.** The design's
> full ANSI terminal (4a/4b) is the target end-state, not the v1 build.

- Distinct surface from chat (the two are never merged). Target end-state: a **true
  ANSI emulator over a real PTY** — live output, scrollback, colors, cursor,
  full-screen programs. v1: minimal terminal as scoped above.
- **Accessory key bar** above the keyboard is make-or-break and is **in v1**: `Esc`,
  `Tab`, `Ctrl` (sticky modifier), arrow keys, painful symbols (`|`, `/`, `-`, `~`,
  `` ` ``), plus `paste`.
- **Portrait (4a, chosen)** and **landscape (4b)** layouts; landscape gives real
  width and shows `Ctrl` as a lit sticky modifier + full symbol run + paste.
  Font-size control ("font" button).
- Scope: **one shell at a time per session**. Multi-pane / tabbed shells stay
  desktop-only.

### 5.5 Control, Git & safety
- **New agent** screen: pick agent type (Claude Code / OpenCode / Codex CLI), name
  the session (names the worktree + branch, e.g. `flightdeck/add-rate-limit`),
  choose the base branch to create the worktree from (`main`), dictate or type the
  **first task**. One screen → **Launch agent**. v1 fields = **type + name + base +
  first task only**; model/effort (design shows "Opus 4.8 · high effort") **inherit
  the desktop's defaults** and are not editable from the phone in v1.
- **Git status (read):** view a session's branch, changed files, ahead/behind,
  base, drift — read-only, frictionless.
- **Git actions (v1):** **abandon worktree** (type-to-confirm), **guarded merge-back**,
  **pull base**. All are higher-stakes and confirmation-gated.

> **Out of scope — push & PR (decided Round 2):** **pushing a branch and opening a
> PR are handled by the agent itself, not the remote.** The design's Push
> confirmation sheet and Compare-URL screen (design section 05) are **not built in
> the remote.** Notifications/roll-ups may still say "ready to push" as
> *information* about the agent's state, but the app performs no push and holds no
> GitHub token. No GitHub API integration in v1.

### 5.6 Actions, pairing & connection
- **Session actions sheet:** safe actions grouped on top (Restart agent, Open
  shell, Set manual status, Pull base, Merge back), destructive actions apart in red
  (Close session, Abandon worktree). Never mixed. *(Design shows "Push branch…" here;
  removed per Round 2 — push is agent-handled, §5.5.)*
- **Type-to-confirm** (the one truly destructive path): "Abandon this worktree?
  Deletes the `fix-login` worktree and its uncommitted changes. The branch stays.
  This cannot be undone. Type `fix-login` to confirm." → **Abandon worktree / Keep
  it.**
- **Pairing:** "Pair with your Mac — In FlightDeck on desktop, open Settings →
  Remote and scan this, or enter the code." 4-digit code (`4 7 2 9`) + **Scan QR
  instead**. "End-to-end encrypted · unlocked by Face ID." Per-device.
- **Settings & connection honesty:** connected device ("Ruud's MacBook Pro —
  Connected · low latency"); notification toggles (Agent needs input, Agent
  finished, Completion chime) that toggle **independently**; Security (Require Face
  ID to open, Unpair this device); **Reconnecting banner** — "Reconnecting to
  desktop… Commands are paused until the link is back. Nothing is sent blind."

### 5.7 Navigation
- **Bottom tab bar:** Projects · Activity · **[+ FAB, center]** · Shell · Settings.
  (Activity carries an unread dot.)
- **Activity tab:** a **chronological feed of status events** (finished / needs
  input / errors) for the paired Mac, each tappable to **deep-link** to the agent.
  Unread dot clears on view.
- No Mac switcher in v1 (single-Mac UI, §9).

### 5.8 Behaviors, states & edge cases (decided)
- **Entry flow:** **unpaired → Pairing screen; paired → Projects.**
- **Restart agent:** relaunches a **fresh agent process in the same worktree/branch**;
  the transcript is preserved (not a new worktree).
- **Set manual status:** the phone can set the **cyan manual override** with a short
  label (matches desktop); it clears on the next real state change.
- **Reply / command delivery failure:** if the link drops mid-send, the message is
  marked **"not delivered — retry"** — never silently dropped (connection honesty,
  §8).
- **Queued notifications:** if an agent needs input / finishes while the phone is
  unreachable, the relay **holds pending events and delivers them on reconnect**,
  deduplicated (best-effort). (Bounded by "Mac must be running" — §9.)
- **Latency:** **no hard SLA.** Aim sub-second on good networks; always show honest
  connection state + latency, and pause commands on a lost link.

---

## 6. Feature scope (tiers, from briefing §6)

> **v1 release decision:** **all three tiers ship in v1** (monitor+respond, light
> control, git actions). The only in-tier deferral is the shell: v1 gets a *minimal*
> terminal (§5.4), full ANSI/PTY is fast-follow. Voice: v1 gets dictation +
> edit-before-send; TTS readback and voice Approve/Deny are fast-follow (§7).

### Tier 1 — Monitor & respond (the heart)
- See all projects and agent sessions with live status, rolled up.
- Push notification on **finish** and **needs input**; tap to deep-link to the
  agent.
- Open an agent and read its recent output / cleaned transcript.
- Type or speak a reply / follow-up; approve/deny permission prompts. **This is the
  killer feature.**

### Tier 2 — Light control
- Start a new agent session (type, name, first task).
- Restart a stuck agent; stop/close a session.
- Switch between projects and sessions.
- Open **one** shell terminal in a session's worktree and run commands.

### Tier 3 — Git actions (higher stakes, confirmation-heavy)
- See git status (branch, changes, ahead/behind, base, drift) — read, frictionless.
- **Abandon** a worktree (type-to-confirm), **pull base**, **guarded merge-back** —
  all confirmation-gated.
- **Push & PR creation are NOT in the remote** — the agent handles its own push;
  no GitHub API/token in the app (see §5.5).

---

## 7. Voice (from briefing §9)

Voice is a natural fit (user is away from the keyboard), staged:

- **MVP:** native text fields (free iOS dictation) + a first-class mic button in
  compose + **strict edit-before-send**. Covers ~80%.
- **Fast-follow:** **TTS readback** of the agent's latest message/question
  (`AVSpeechSynthesizer`) with play/stop; voice-driven **Approve/Deny** on
  permission prompts (eyes-free unblock). This makes it genuinely hands-free.
- **Only if needed:** custom technical-vocabulary STT (on-device or server,
  Whisper-class) if built-in dictation proves too weak for code — with an explicit
  privacy story (prompts about private repos leaving the device).

**Design constraints:**
- Never auto-send raw transcription — always transcript → review → send.
- Push-to-talk (hold to talk, release to review) preferred — deliberate,
  interruptible.
- TTS must decide **what** to read (the question / a summary — not all output).
- The agent-chat screen must be usable **eyes-free for the common loop** (hear the
  question → speak/confirm) while degrading gracefully to full visual reading.
- Accessibility labels on all controls (also unlocks iOS Voice Control for free).

---

## 8. Safety & trust principles (non-negotiable, from briefing §6)

- **FlightDeck never rewrites Git history, never auto-commits, auto-merges, or
  auto-creates PRs.** The remote preserves this exactly.
- **Reads and monitoring are frictionless; anything that changes shared state is
  deliberate and confirmed.**
- Destructive/irreversible actions (close session, discard uncommitted changes,
  abandon worktree, merge-back) **always require explicit confirmation.** The single
  truly destructive path (abandon) requires **typing the session name**. (Push is
  not a remote action — §5.5.)
- Safe vs. destructive actions are **visually separated** (destructive in red,
  apart).
- **Connection honesty:** a shell command that silently fails to reach the desktop
  is worse than an honest error. Lost link **pauses commands loudly**; nothing is
  sent blind.

---

## 9. Connectivity, pairing & security

- **Pairing:** per-device, initiated from desktop (Settings → Remote) via QR scan
  or 4-digit code. **End-to-end encrypted.** Face-ID gated.
- Running arbitrary commands on the dev's machine raises the auth bar — pairing is
  a real trust step.
- **Connection state is always honest:** connected/latency shown; reconnecting
  banner pauses commands.

### 9.1 Architecture (decided)

- **Reach:** the phone connects to the Mac **over the internet** (cellular / any
  network), not just same-LAN.
- **Transport:** **relay-only in v1** (single code path). A **hosted relay** brokers
  the phone ↔ desktop connection. Direct-LAN fast-path is a later optimization, not
  v1.
- **Relay hosting:** operated by New Orange on **Azure Container Apps**. FlightDeck
  desktop maintains an outbound connection to the relay; the phone connects to the
  relay.
- **Encryption:** the relay is **zero-knowledge / blind pipe** — it routes ciphertext
  and cannot read agent content, transcripts, or shell traffic. Traffic is
  **end-to-end encrypted** between phone and desktop.
- **Relay auth & key model:** each device holds a **per-device identity keypair**
  registered at pairing (private key in iOS Keychain / Secure Enclave on the phone).
  The relay authenticates each end by its device key and **routes by pairing ID
  only**; it never holds decryption keys. The pairing QR/code carries the shared
  secret that bootstraps the E2E channel.
- **Pairing lifetime:** a pairing **persists until explicitly unpaired** — no forced
  periodic re-pair. Face-ID gates app-open (optional toggle in Settings).
- **Push:** **APNs**, driven by the relay/desktop when an agent finishes or needs
  input. New Orange operates the push service.
- **Mac asleep / FlightDeck closed:** **out of scope.** Agents only run while the
  desktop app is running; notifications fire only while the desktop is reachable.
  Connection honesty (§8) covers the unreachable case.
- **Multiplicity (v1):** **one phone ↔ one Mac** in the UI. No Mac-switcher in v1.
  **The relay and pairing model must be architected to support one-phone-↔-many-Macs
  later** (per-device pairing keyed by Mac), so multi-Mac is a UI addition, not a
  protocol change. Single active user per Mac. **[TBD]** multiple phones per Mac.
- **Desktop-side dependency:** FlightDeck desktop must gain (a) a relay client, (b)
  a structured agent-transcript + status feed, and (c) a pairing/remote settings
  surface. These are net-new — no remote/server/websocket/PTY-over-wire code exists
  in the desktop app today.
- **[TBD §13]** latency targets, session/token lifetime & re-pair cadence, multiple
  phones per Mac.

### 9.2 Notifications, offline & multiplicity (decided)

- **Notification controls (v1):** the three global toggles from the design — *Agent
  needs input*, *Agent finished*, *Completion chime* (toggle independently) — **plus
  per-project mute**. Quiet hours / Do-Not-Disturb is a later phase.
- **Offline behavior:** when disconnected, the app shows the **cached last-known
  transcript and status, read-only and clearly marked stale**. **No actions are
  allowed offline** — commands never send blind (reinforces §8 connection honesty).
- **Multiplicity:** **v1 UI = one phone ↔ one Mac** (no Mac switcher). Relay/pairing
  built to add one-phone-↔-many-Macs later without a protocol change. Single active
  user per Mac. Multiple phones per Mac is **[TBD]**.

---

## 10. Explicitly out of scope (desktop-only)

- Multiple side-by-side terminals, shell tabs, split views (single shell per
  session **is** in scope).
- Command palette, keyboard modes, keyboard shortcuts.
- Container config, image builds, config-manager raw-TOML editing (admin/setup).
- Forcing the agent chat into a raw terminal, or chat-ifying the shell — they are
  **two distinct surfaces**.
- **Push a branch / open a PR from the remote** — agent-handled; no GitHub API/token
  (§5.5).

### 10.1 Future phases (noted, not specified here)
- **iPad** app (likely multi-pane) — future phase; not specified in this PRD.
- Full ANSI/PTY shell emulator with full-screen program support (§5.4).
- Voice: TTS readback + voice-driven Approve/Deny (§7).
- Direct-LAN transport fast-path; multi-Mac UI; multiple phones per Mac; quiet
  hours / Do-Not-Disturb.
- Apple Watch / Android — not planned.

---

## 11. Design system reference

- **Colors:** orange `#FF6601` (signal/brand); midnight greens `#0D353B`,
  `#061417`, `#0b2429`, `#05191c`; text `#F7F8E2`; muted `#A9C0C0` / `#6E8488`;
  status red `#E5484D`, green `#6FB26C`, cyan `#4FB3C4`.
- **Type:** Geist (variable) for UI; Geist Mono for code, terminal, git
  indicators, and monospaced values.
- **Motion:** spinner (working), pulse, blink (cursor).
- **Shape:** large corner radii (cards ~16–22px, phone frame ~46px), pill badges,
  glowing status dots.

---

## 12. Success metrics

**No formal success metrics are defined for v1** (decided Round 2). The product bet
is qualitative: the monitor-and-respond loop should feel fast and trustworthy from
the phone. Metrics may be revisited post-launch; none gate v1.

---

## 13. Remaining deferred (non-blocking) items

- Session/token lifetime & re-auth cadence beyond "persist until unpaired".
- Multiple phones per Mac.
- Exact E2E cipher suite / key-rotation policy (engineering detail).
- Concrete latency numbers (no SLA committed).

All other open questions were resolved in the interview (§14) and folded into the
body.

---

## 14. Interview log

Round-by-round Q&A used to finalize this PRD (10 questions/round, max 4 rounds).

### Round 1 — answered

1. **v1 scope:** all three tiers ship at once (not phased).
2. **Shell:** no real PTY/ANSI in v1 — ship a *minimal working terminal* instead;
   full emulator is fast-follow.
3. **Voice:** MVP = dictation + edit-before-send confirmed; TTS + voice Approve/Deny
   fast-follow.
4. **Platform:** iPhone for v1, iPad later.
5. **Reach:** over the internet.
6. **Transport:** hosted relay, hosted on **Azure Container Apps**.
7. **Push/relay operator:** New Orange–operated relay + APNs.
8. **Mac asleep/closed:** out of scope.
9. **Transcript:** FlightDeck desktop will expose a structured transcript feed
   (net-new desktop dependency).
10. **Multiplicity:** one phone ↔ multiple Macs.

### Round 2 — answered

1. **Transport:** relay-only in v1.
2. **Encryption:** zero-knowledge relay, E2E between phone and desktop.
3. **Relay auth:** per-device identity keypair; relay routes by pairing ID, never
   holds decryption keys.
4. **Notifications:** three global toggles + per-project mute in v1; quiet hours
   later.
5. **Offline:** show cached last-known state, read-only, marked stale; no actions
   offline.
6. **Minimal terminal:** command → streamed output + basic colors + scrollback +
   `Ctrl-C` interrupt; **copy/paste required**; no full-screen TUIs in v1.
7. **Git actions (v1):** **abandon worktree, guarded merge-back, pull base only.**
8. **Push & PR:** **out of scope — handled by the agent itself**, not the remote.
   No GitHub API/token.
9. **New-agent fields:** type + name + base branch + first task only; model/effort
   inherit desktop defaults.
10. **Success metrics:** none defined for v1.

### Round 3 — answered

1. **Multi-Mac:** **single-Mac UI for v1**; relay architected to add multi-Mac later
   without a protocol change.
2. **Activity tab:** chronological status-event feed, deep-linking. ✔
3. **Manual status:** settable from phone (cyan override + label), clears on next
   real change. ✔
4. **Restart agent:** fresh process, same worktree/branch, transcript preserved. ✔
5. **Delivery failure:** mark "not delivered — retry", never silent. ✔
6. **Pairing lifetime:** persists until unpaired; Face-ID gates open. ✔
7. **Queued notifications:** relay holds pending events, delivers on reconnect,
   deduped. ✔
8. **Latency:** no hard SLA.
9. **Entry flow:** unpaired → Pairing; paired → Projects. ✔
10. **iPad:** noted as a future phase only. ✔

**Interview complete after 3 rounds** (Round 4 not needed). PRD finalized to v1.0.
