#!/usr/bin/env bash
# Print an agent's state on the tutorial worker, or test it.
#
#   tutorials/worker/agent-state.sh "delete notes"          -> idle | working | ... | missing
#   tutorials/worker/agent-state.sh "delete notes" working  exit 0 when it is in that state
#
# For scene scripts: `waitfor 180 tutorials/worker/agent-state.sh "delete notes" idle`.
set -euo pipefail
NAME="${WORKER_NAME:-fd-tutorial-worker}"
agent="$1"; want="${2:-}"
state="$(podman exec "$NAME" flightdeck worker status --json | python3 -c '
import json, sys
name = sys.argv[1]
for project in json.load(sys.stdin).get("projects", []):
    for a in project.get("agents", []):
        if a.get("name") == name:
            print(a.get("status")); sys.exit()
print("missing")
' "$agent")"
if [[ -n "$want" ]]; then [[ "$state" == "$want" ]]; else echo "$state"; fi
