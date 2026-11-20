#!/usr/bin/env bash
# An agent's state for a terminal-UI scene (MODE=noworker), where there is no
# worker to ask (see agent-state.sh): the status its hook writes in the
# worktree, `.flightdeck/agent-status` (working, idle, waiting, ...).
#
#   tutorials/worker/tui-agent-state.sh "dark theme"        -> idle | working | ... | missing
#   tutorials/worker/tui-agent-state.sh "dark theme" idle   exit 0 when it is in that state
#
# PROJECT picks the project (default pinboard).
set -euo pipefail
NAME="${WORKER_NAME:-fd-tutorial-worker}"
project="${PROJECT:-pinboard}"
slug="$(echo "$1" | tr '[:upper:]' '[:lower:]' | tr ' ' '-')"
want="${2:-}"
state="$(podman exec -u dev "$NAME" sh -c \
  "tail -n1 ~/flightdeck/$project/.flightdeck/worktrees/$slug/.flightdeck/agent-status 2>/dev/null || echo missing")"
state="$(echo "$state" | tr -d '[:space:]')"
if [[ -n "$want" ]]; then [[ "$state" == "$want" ]]; else echo "$state"; fi
