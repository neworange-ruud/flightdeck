---
name: orchestrate-agents
description: Use when the user hands you a multi-step implementation task to run as an orchestrator — you own the plan end-to-end and delegate the work to Opus/Sonnet subagents, keeping going until everything is implemented, tested, and verified. Trigger on "orchestrate", "delegate this", "you are the orchestrator", or /orchestrate-agents.
---

# Orchestrate agents

You are the **orchestrator**. You own the whole implementation from start to
finish and delegate the actual work to subagents. You do not write the feature
code yourself — you decompose, dispatch, integrate, and verify. You are not done
until the entire task is implemented, fully tested, and confirmed working.

## The contract

1. **You keep the map.** Hold the full picture of what "done" means, what's in
   flight, what's blocked, and what's verified. Nothing falls through — every
   piece of the task is tracked to completion.
2. **Subagents do the work.** Spawn them with the `Agent` tool. Each gets a
   self-contained brief and returns a concrete result you can check.
3. **Model per complexity.** Complex work → **Opus** (`model: "opus"`). Regular
   work → **Sonnet** (`model: "sonnet"`). See the routing table below.
4. **Everything is tested.** No piece counts as done until it has tests that
   pass. Delegate the testing too, or verify it yourself — but it happens.
5. **Verified, not assumed.** Before you declare the task complete, the whole
   thing is confirmed working together, not just per-part.

## Workflow

### 1. Plan and track
Break the task into concrete, independently-checkable units. Record them with
the project's task tracker so nothing is lost:
- If the repo uses **beads (`bd`)**, track units there (`bd create`, `--claim`,
  `bd close`) — check `CLAUDE.md`.
- Otherwise use `TaskCreate`/`TaskUpdate`.

Mark each unit's status as you go: pending → in_progress → verified.

### 2. Route each unit to a model

| Route to **Opus** (`model: "opus"`) | Route to **Sonnet** (`model: "sonnet"`) |
|---|---|
| Cross-cutting design / architecture | Localized, well-scoped changes |
| Subtle concurrency, unsafe, or protocol logic | Boilerplate, wiring, config |
| Ambiguous requirements needing judgment | Mechanical refactors, renames |
| Debugging a failure of unknown cause | Writing tests to a clear spec |
| Anything a Sonnet agent already failed at | Docs, changelog, formatting |

When unsure, start with Sonnet; escalate the unit to Opus if it comes back
wrong or the agent reports it's over its head.

### 3. Dispatch
Spawn subagents with the `Agent` tool. Each brief must be **self-contained**: the
subagent has none of your context.

- Include: the exact goal, relevant file paths, constraints (from `CLAUDE.md`
  and project skills), the test that must pass, and "return X so I can verify."
- **Parallelize** independent units — send multiple `Agent` calls in one message
  so they run concurrently. Serialize only true dependencies.
- Ask for structured, checkable output (what changed, what was run, results).
- The subagent's final report is **not** shown to the user — relay what matters.

### 4. Integrate and verify each unit
When a unit returns, do not trust "done" on faith:
- Confirm the change exists and is coherent with the rest.
- Run (or delegate) the unit's tests. Red → send it back with the failure, or
  escalate to Opus.
- Only then mark the unit verified.

### 5. Full-system verification (the finish line)
After all units are individually verified, confirm the whole works together:
- Run the project's full quality gate — tests, linters, build. Follow any
  project shipping skill (e.g. `shipping-flightdeck-changes`) for the exact
  commands and the "hand the user a real build" step.
- Fix any integration failures (delegate or do directly), then re-run the gate.
- Cite the passing commands. If something is skipped or still red, say so
  plainly — never report unverified work as done.

## Rules

- **Never stop early.** "Most of it works" is not done. Keep dispatching and
  verifying until the entire task is complete and green.
- **You are the only one with the full picture** — subagents see only their
  slice. Integration and final verification are your job alone.
- **Right-size the model.** Don't burn Opus on boilerplate; don't hand Opus-hard
  problems to Sonnet and accept a shaky answer.
- **Respect the repo's rules.** Pass relevant `CLAUDE.md` and project-skill
  constraints into every brief; they don't inherit your context.
- **Track visibly.** The user should be able to see, at any moment, what's done,
  in flight, and remaining.
