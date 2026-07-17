#!/usr/bin/env bash
# fake-agent.sh — deterministic, network-free, LLM-free stand-in for a real
# coding agent (claude/codex/opencode), used by the FlightDeck Remote E2E
# harness to drive status transitions and reply delivery without any real
# LLM. Launched by the desktop exactly as a configured `agents.<key>.command`
# (cwd = the agent's worktree; see remote-control-c3m plan, "Fake-agent
# contract" + "Shared harness building blocks" item 2).
#
# Agent-status contract (must match the desktop reader):
#   The desktop polls `<worktree>/.flightdeck/agent-status` — one keyword per
#   line, appended (never truncated) — and interprets only the *latest* line.
#   Recognized keywords come from `status_keyword_to_interpreted()` in
#   `src/app/state.rs` (approx. lines 317-330):
#     working  -> InterpretedStatus::Working        (also: busy, in_progress,
#                                                      in-progress, thinking)
#     idle     -> InterpretedStatus::Idle
#     waiting  -> InterpretedStatus::WaitingForInput (also: waiting_for_input,
#                                                      input)
#   This stub emits ONLY the canonical `working` / `idle` / `waiting`
#   keywords, one per line, flushed immediately (each append is its own
#   open+write+close — no buffering, no sleeps) so the desktop's poller sees
#   every transition in order.
#
# Replies log:
#   Every line read from stdin (a phone reply / prompt forwarded by the
#   desktop) is appended verbatim to `.flightdeck/agent-replies.log` (one per
#   line) so the harness can assert a reply arrived, and is also echoed to
#   stdout so it is visible in the PTY transcript.
#
# Sentinel:
#   A stdin line that is exactly `__WAIT__` is handled like any other line
#   (logged + echoed) but makes the stub emit `waiting` instead of `idle`
#   afterwards, letting the harness exercise the `needs_input` / waiting
#   state on demand. Any other line emits `idle` after processing.
#
# Lifecycle:
#   1. Print a short banner to stdout.
#   2. Append `working` to agent-status immediately (signals the turn/agent
#      has started).
#   3. For each stdin line: append it to agent-replies.log, echo it, then
#      append `working` (new turn started) BEFORE the *next* line if this
#      isn't the first line, and append `idle`/`waiting` after handling the
#      current line — i.e. `working` while "processing", `idle` (or
#      `waiting` on the sentinel) when done.
#   4. On EOF, append a final `idle` and exit 0.
#
# No network calls, no randomness, no real agent behavior — purely
# deterministic status/log bookkeeping for E2E assertions.

set -euo pipefail

STATUS_DIR=".flightdeck"
STATUS_FILE="${STATUS_DIR}/agent-status"
REPLIES_LOG="${STATUS_DIR}/agent-replies.log"
SENTINEL="__WAIT__"

mkdir -p "${STATUS_DIR}"

# Append a single status keyword. Each invocation opens/writes/closes the
# file on its own, so writes are immediately visible to a poller — no extra
# buffering or flush step needed.
emit_status() {
    printf '%s\n' "$1" >>"${STATUS_FILE}"
}

log_reply() {
    printf '%s\n' "$1" >>"${REPLIES_LOG}"
}

printf 'fake-agent: starting (deterministic E2E stub, no network, no LLM)\n'
printf 'fake-agent: status file: %s\n' "${STATUS_FILE}"
printf 'fake-agent: replies log: %s\n' "${REPLIES_LOG}"

emit_status working

first_line=1
# `|| [[ -n "$line" ]]` ensures a final unterminated line (no trailing
# newline before EOF) is still processed instead of silently dropped.
while IFS= read -r line || [[ -n "${line}" ]]; do
    if [[ "${first_line}" -eq 0 ]]; then
        # A new reply arrived after a prior turn finished: signal work
        # resuming before we process it.
        emit_status working
    fi
    first_line=0

    log_reply "${line}"
    printf '%s\n' "${line}"

    if [[ "${line}" == "${SENTINEL}" ]]; then
        emit_status waiting
    else
        emit_status idle
    fi
done

emit_status idle
printf 'fake-agent: stdin closed, exiting\n'
exit 0
