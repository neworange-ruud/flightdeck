#!/usr/bin/env bash
# Bring the tutorial stage to its starting point, off camera:
#   a fresh macOS VM with FlightDeck.app, a fresh worker container, and
#   FlightDeck full screen, paired with the worker, its worker window full screen.
#
#   tutorials/prepare.sh                 everything from scratch
#   SKIP_BUILD=1 tutorials/prepare.sh    reuse the last worker binary
#   STAGE=launcher tutorials/prepare.sh  stop at the launcher (for a pairing tutorial)
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VMCTL="$ROOT/tutorials/vm/vm"

"$ROOT/tutorials/vm/up.sh"                 # first: the worker binds to the VM bridge
SKIP_BUILD="${SKIP_BUILD:-1}" "$ROOT/tutorials/worker/up.sh" >/dev/null

"$VMCTL" ssh 'open /Applications/FlightDeck.app'
"$VMCTL" ssh 'for i in $(seq 1 50); do pgrep -q flightdeck-desktop && break; sleep 0.2; done'
sleep 3

# Launcher full screen: View > Enter Full Screen.
"$VMCTL" run - <<'EOF'
move 200 14
click
pause 0.8
move 260 262
click
pause 2
EOF
[[ "${STAGE:-}" == "launcher" ]] && { echo "stage: launcher"; exit 0; }

CODE="$(podman exec fd-tutorial-worker flightdeck worker pair --json \
  | python3 -c 'import sys, json; print(json.load(sys.stdin)["code"])')"

# Connect to remote… -> address, code -> Connect -> Connect anyway.
"$VMCTL" run - <<EOF
move 1089 636
click
pause 1.5
move 960 440
click
paste devbox:7420
move 960 505
click
paste $CODE
move 745 591
click
pause 1.5
move 776 699
click
pause 4
EOF

# The worker window opens as a normal window: its green button -> full screen.
"$VMCTL" run - <<'EOF'
move 389 177
click
pause 2.5
move 960 1300
EOF
"$VMCTL" shot >/dev/null
echo "stage: worker window, full screen ($("$VMCTL" ip))"
