#!/usr/bin/env bash
# Bring the tutorial stage to its starting point, off camera:
#   a fresh macOS VM with FlightDeck.app, a fresh worker container, and
#   FlightDeck paired with the worker, its worker window full screen.
#
#   tutorials/prepare.sh                 everything from scratch
#   SKIP_BUILD=0 tutorials/prepare.sh    also rebuild the worker binary
#   STAGE=launcher tutorials/prepare.sh  stop at the launcher (for a pairing tutorial)
#
# Coordinates are screen points, 1440x810 (see tutorials/vm/vnc_drive.py). The launcher
# and the Connect form open centred; the worker window is made full screen
# through the View menu, whose place never changes.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VMCTL="$ROOT/tutorials/vm/vm"
WORKER="${WORKER_NAME:-fd-tutorial-worker}"

fail() { "$VMCTL" shot "$ROOT/target/tutorials/prepare-failed.png" >/dev/null 2>&1 || true
         echo "prepare: $1 (screen: target/tutorials/prepare-failed.png)" >&2; exit 1; }

"$ROOT/tutorials/vm/up.sh"                 # first: the worker binds to the VM bridge
SKIP_BUILD="${SKIP_BUILD:-1}" "$ROOT/tutorials/worker/up.sh" >/dev/null

"$VMCTL" ssh 'open /Applications/FlightDeck.app'
"$VMCTL" ssh 'for i in $(seq 1 50); do pgrep -q flightdeck-desktop && break; sleep 0.2; done'
sleep 3
[[ "${STAGE:-}" == "launcher" ]] && { echo "stage: launcher"; exit 0; }

CODE="$(podman exec "$WORKER" flightdeck worker pair --json \
  | python3 -c 'import sys, json; print(json.load(sys.stdin)["code"])')"

# Launcher: Connect to remote… -> address, code -> Connect -> Connect anyway.
"$VMCTL" run - <<EOF
move 849 516
click
pause 1.5
move 720 305
click
pause 0.4
paste devbox:7420
move 720 370
click
pause 0.4
paste $CODE
pause 0.3
move 505 457
click
pause 1.5
move 536 565
click
EOF

attached() {
  podman exec "$WORKER" flightdeck worker status --json \
    | python3 -c 'import json, sys; sys.exit(0 if json.load(sys.stdin).get("attached") else 1)'
}
for _ in $(seq 1 40); do attached && break; sleep 0.25; done
attached || fail "the Desktop did not attach to the worker"
sleep 2.5

# The worker window is now the key window: View > Enter Full Screen.
"$VMCTL" run - <<'EOF'
move 200 14
click
pause 0.8
move 260 262
click
pause 2.5
move 1100 420
EOF
"$VMCTL" shot >/dev/null
echo "stage: worker window, full screen ($("$VMCTL" ip))"
