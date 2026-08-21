# Design Briefing: FlightDeck & a Mobile Remote-Control App

This briefing describes what FlightDeck is, how its user interface looks and
works, and how a user interacts with it — so a designer can decide how a mobile
app for **remote control** of FlightDeck should look. Screenshots are included
separately.

## 1. What FlightDeck is (the one-paragraph version)

FlightDeck is a **desktop terminal application** (macOS, Linux, Windows) that lets
a developer **run several AI coding agents in parallel** on the same codebase.
Think of it as an air-traffic control tower: the developer isn't writing code by
hand — they're **launching, monitoring, and steering multiple autonomous agents**
(Claude Code, OpenCode, Codex CLI) that each work in their own isolated copy of a
Git project. FlightDeck's whole reason for existing is to make it manageable to
keep **many long-running agents busy at once** without them stepping on each
other, and to know at a glance which ones are working, which are done, and which
are stuck waiting for the human.

The core mental model, stated in FlightDeck's own docs:

> **Each Agent Tab = 1 Worktree = 1 Branch = 1 Primary Agent Process (+ optional shell terminals)**

Everything in the UI is organized around that unit: an isolated workspace,
containing one agent, that the developer supervises.

## 2. Who uses it and what job it does for them

The user is a developer running **agentic coding sessions**. Instead of one AI
assistant in one chat, they might have four or five agents each grinding on a
different task — "fix the login bug," "add tests," "port to Windows," "review the
codebase" — all at the same time, on the same project. Each agent can run for many
minutes autonomously.

The developer's actual moment-to-moment job becomes **supervision, not typing**:

- Kick off new agents and give them a task.
- Watch which agents are **working** vs **idle/done** vs **waiting for input**.
- Jump into whichever agent needs attention, read what it did, and answer its
  questions or type a follow-up.
- When an agent finishes good work, **push its branch** and open a pull request.

The single most important recurring event in their day is: **"an agent I wasn't
looking at just finished, or just got stuck and needs me."** FlightDeck already
surfaces this with OS notifications and a completion chime. **This is the
emotional and functional center of gravity for the mobile app** (see §8).

## 3. The structure the design must reflect

There is a strict three-level hierarchy. The mobile design should make this
navigable and legible:

1. **Projects** — top level. A developer can have several project folders
   (repositories) open at once, each shown as a **project tab** across the top.
   Every open project stays *live in the background*: its agents keep running and
   keep notifying even when the developer is looking at a different project.
2. **Agent sessions** (called "Agent Tabs" / "Agent Session Tabs") — the middle
   level, shown as a **left sidebar list** within a project. Each is one agent, in
   one isolated Git worktree, on one branch. This is the primary unit the user
   thinks in.
3. **Terminals within a session** — the bottom level. Each session has one *agent
   terminal* plus optionally several *shell terminals* (for running commands in
   that same worktree), shown as a row of tabs (`agent | shell 1 | shell 2`).

So: **many projects → each has many agent sessions → each has one agent terminal +
some shells.**

## 4. How the desktop UI is laid out (reference for translation)

From top to bottom (see screenshot):

- **Logo header** — a branded "FLIGHTDECK" title bar. Decorative; the mobile app
  can treat this loosely.
- **Project tab row** — one tab per open project, each with a **colored status
  dot** and a close (`✕`) button, plus a `+ project` button. The active project is
  highlighted.
- **Agents sidebar (left column)** — the list of agent sessions for the current
  project. Each row shows: the **session name** (e.g. `fix-login`), the **agent
  type** (Claude Code / OpenCode), a **status** (`[idle]`, working spinner), and
  compact **git indicators** (`~dirty`, `no-upstream`, `drift:13`). The selected
  session is marked with a `▸`.
- **Main pane (center/right)** — the **live terminal output** of the selected
  terminal. This is where the developer reads what the agent is doing and types
  into it. It's a real, scrollable terminal.
- **Git info bar** — a one-line summary for the selected session's worktree:
  branch name, changed-file counts (`+3 ~2 -1 (6 files)` or `clean`), ahead/behind
  vs remote, base branch, and drift from base.
- **Status/mode bar** — shows the current input mode and key hints, plus
  occasional notices (e.g. "new version available").

**Interaction model on desktop:** keyboard-first with two modes — *App mode* (keys
navigate FlightDeck) and *Terminal mode* (keys go into the agent). There's a
**command palette** (`Ctrl-g`) that exposes every action. This keyboard/palette
model is a desktop constraint; **the mobile app should not try to replicate modes
or shortcuts** — it should offer direct, touch-native equivalents of the *actions*
(see §6).

## 5. Status is the primary information

FlightDeck spends enormous effort on **accurately knowing each agent's state**,
because that's what the user is monitoring. There are three meaningful states,
each with a consistent color language the mobile app should inherit:

- 🔴 **Working** — the agent is actively running a turn (shown as a red animated
  spinner). "Leave it alone, it's busy."
- 🟢 **Idle / finished** — the agent is waiting for a prompt; its turn is done.
  "Ready for me, or done."
- 🟠 **Waiting for input / needs attention** — the agent has stopped and is asking
  the human something (a permission prompt, a question). **This is the state that
  most urgently pulls the user in.** It gets its own distinct OS notification and a
  distinct three-pulse sound, separate from the finish chime.
- 🔵 **Manual override** — the user can manually flag a status (cyan); minor, but
  exists.

Status is **rolled up the hierarchy**: an agent's status shows on its sidebar row;
each **project tab's dot summarizes** its agents (red = something needs attention,
cyan/red spinner = an agent is working, dim/green = idle). The mobile app will need
the same roll-up so a user can see, from a project list, "which project has
something demanding me right now."

## 6. The actions a user takes (candidate features for the remote)

These are the concrete things a user does in FlightDeck. For a *remote control*
app, they split into three tiers — this is the most important part of the briefing
for scoping:

**Tier 1 — Monitor & respond (the heart of a remote):**

- See all projects and all agent sessions with their live status at a glance.
- Get pushed a notification when an agent **finishes** or **needs input**, and tap
  straight to that agent.
- Open an agent and **read its recent output / what it's doing**.
- **Type a reply or a follow-up prompt** into an agent (answer its question,
  approve/deny a permission, give the next instruction). This is the killer
  feature — steering a running agent from your phone.

**Tier 2 — Light control:**

- **Start a new agent session**: pick an agent type (Claude/OpenCode/Codex), name
  it, give it a task.
- **Restart** a stuck agent; **stop/close** a session.
- Switch between projects and sessions.
- **Open a shell terminal** in a session's worktree and run commands (see the
  dedicated subsection below — this is in scope but carries real UI requirements).

**Tier 3 — Git actions (higher stakes, confirmation-heavy):**

- See the git status of a session (branch, changes, ahead/behind).
- **Push** a branch (always confirmed) and get the **GitHub compare URL** to open
  a PR.
- Abandon a worktree, pull base, guarded merge-back.

**A crucial safety principle to carry into the design:** FlightDeck **never
rewrites Git history** and never auto-commits, auto-merges, or auto-creates PRs —
by deliberate design. Destructive or irreversible actions (closing a session,
discarding uncommitted changes, pushing) **always require explicit confirmation**.
The mobile app must preserve this: reads and monitoring should be frictionless;
anything that changes shared state should be deliberate and confirmed. Don't make
it easy to fat-finger "abandon worktree" on a phone.

### Terminal / shell access (in scope)

Each desktop session can open shell terminals inside its worktree (the same folder
the agent works in). This is **in scope for the mobile app** — quick, real reasons
to want it from a phone include running a test, checking `git log`/`git status`,
tailing a log, or unsticking something the agent can't. Two things must be true
for it to be usable:

- **It needs a real terminal, not the chat metaphor.** The agent conversation can
  be a cleaned-up message thread, but a shell is interactive and must render as a
  faithful ANSI terminal emulator: live output, scrollback, cursor, colors, and
  support for full-screen programs (`vim`, `top`, `less`) via a proper PTY. Treat
  the agent-chat view and the shell view as **two distinct surfaces**, not one.
- **The mobile keyboard is the hard problem.** A phone keyboard has none of the
  keys a shell needs. A shell view must add an **accessory key bar** above the
  keyboard with at least: `Esc`, `Tab`, `Ctrl` (as a sticky modifier), arrow keys,
  and the symbols that are painful to reach (`|`, `/`, `-`, `~`, `` ` ``). This is
  the single biggest determinant of whether mobile shell feels usable or
  miserable — established mobile terminals (Blink, Termius, iSH) all live or die on
  this bar. Also account for: paste, pinch/font-size control, text selection, and a
  landscape layout.

Scope guidance: **one shell at a time per session is enough** on mobile. Multiple
side-by-side shells, tabbed shell rows, and split panes stay a desktop feature
(see §7) — don't try to reproduce that layout on a phone. The value is
*occasional, single-terminal access when away from the desk*, not doing real
terminal work on a 6-inch screen.

Security note: running arbitrary commands on the developer's machine from a phone
is powerful. It raises the bar on the app's auth/pairing and on connection-state
honesty (see §6 safety principle and the connection requirements) — a shell
command that silently fails to reach the desktop is worse than a chat reply that
does.

## 7. Things that are conceptually desktop-only

Help avoid dead ends:

- **Multiple side-by-side terminals, shell tabs, split views** — these layout
  features assume a big screen and a keyboard. *Single* shell access per session is
  in scope (see §6, "Terminal / shell access"), but the multi-pane/tabbed-shell
  layout stays desktop-only.
- **Command palette, keyboard modes, keyboard shortcuts** — a desktop crutch.
  Replace with plain buttons and gestures; don't port the palette.
- **Container config, image builds, config manager editing raw TOML** —
  administrative/setup tasks. Not remote-control material.
- **Two distinct surfaces, not one.** The *agent conversation* should be a
  cleaned-up transcript / chat-like view (the user's need there is "read what the
  agent said and reply"), while the *shell* is a faithful ANSI terminal (§6). Don't
  force the agent chat into a raw terminal, and don't try to chat-ify the shell.

## 8. The single sentence to design around

> **"An agent I launched is now working somewhere else; tell me the moment it
> finishes or gets stuck, let me see what it did, and let me answer it — from my
> phone, without being at my desk."**

If the mobile app nails that loop — **push notification → tap → read the agent's
state → type a reply / approve → done** — it delivers the core value. Everything
else (starting agents, git pushes, multi-project management) is supporting cast
around that loop.

## 9. Voice control

Voice fits this app's premise: the user is away from the keyboard, so being able
to talk to an agent — and hear it back — is a natural fit, not a gimmick. Separate
what the platform gives for free from what must be designed deliberately.

### What iOS provides for free

- **System dictation** — any standard native text field automatically shows the
  keyboard microphone button. Basic speech-to-text is essentially free the moment
  a native text field is used, which covers "talk instead of type your prompt" for
  an MVP.
- **iOS Voice Control (accessibility)** — a system-wide hands-free mode that lets
  users tap buttons and dictate by voice with zero app-specific work, *provided the
  app is properly accessible* (labeled buttons, standard controls). A strong extra
  reason to get accessibility labels right regardless.

The catch: built-in dictation is tuned for prose, not code. It mangles
identifiers, symbols, file paths, and jargon (`useEffect`, `npm`, `PR #482`,
`src/lib.rs`), auto-stops on pauses, and has no "send when I'm done." Fine for
casual follow-ups; frustrating for precise instructions.

### What must be designed (the free keyboard button does not decide these)

- **Edit-before-send, always.** Voice → transcript in the compose field → user
  reviews → taps send. Never auto-send a raw transcription. This single rule
  prevents most voice frustration.
- **A first-class mic affordance** in the agent chat — prominent, not hidden on the
  keyboard. Ideally push-to-talk (hold to talk, release to review) so it is
  deliberate and interruptible.
- **Voice for high-value quick actions**, especially **approving/denying permission
  prompts** hands-free ("approve" / "deny"). This eyes-free unblock-an-agent moment
  is arguably where voice earns its keep most — more than dictating long prompts.
- **Text-to-speech readback (the other half of "voice control").** Hands-free
  supervision means also *hearing* what the agent said or asked, not only speaking
  to it. iOS `AVSpeechSynthesizer` is built-in and free, but the design must decide
  *what* to read aloud (the agent's question? a summary? — all output is too much)
  and add a play/stop control. Treat this as a real design surface.
- **Accuracy vs. build-cost trade-off.** If built-in dictation proves too weak for
  code-heavy prompts, the alternative is a custom speech engine (on-device or
  server, e.g. a Whisper-class model) tuned for technical vocabulary — better
  accuracy at the cost of latency, model size/battery, and **privacy** (prompts
  about a private repo would leave the device). Recommendation: ship with built-in
  dictation, measure whether accuracy is actually a problem, and invest in custom
  STT only if it is. Do not build it speculatively.

### Recommendation, staged

- **MVP:** native text fields (free dictation) + a clear mic button in compose +
  strict edit-before-send. Zero extra engineering; covers ~80%.
- **Fast-follow:** TTS readback of the agent's latest message/question, and
  voice-driven Approve/Deny on permission prompts — this is what makes it a
  genuinely *hands-free* remote.
- **Only if needed:** custom technical-vocabulary STT, with an on-device/privacy
  story.

**Design constraint to carry forward:** because voice input and TTS output are both
in play, the agent-chat screen should be usable **eyes-free for the common loop**
(hear the question → speak/confirm the answer) while still degrading gracefully to
full visual reading and typing. This dual-mode requirement affects the layout of
the compose area and the permission action bar (see §6).

## 10. Screenshots to include with this briefing

To pair with this text, include: (1) the **main window** showing the project tabs,
agents sidebar with varied statuses, and the terminal pane; and if available:
(2) an agent in the **working** (red spinner) state vs **idle**, (3) a
**waiting-for-input** prompt, (4) the **git info bar / push → compare-URL** flow,
and (5) an **OS notification** firing when an agent finishes. Those five
illustrate the full monitor-and-respond loop the mobile app is meant to put in the
user's pocket.
