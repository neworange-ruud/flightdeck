#!/usr/bin/env bash
# Tutorial: "Connect FlightDeck Desktop to a worker" (about 1:20).
#   Terminal: ssh devbox -> worker status -> worker pair; FlightDeck:
#   Connect to remote… -> address + code -> the unencrypted-link warning ->
#   the worker's window.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"

STAGE=launcher "$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/vm/terminal.sh"
"$VM" shot "$ROOT/target/tutorials/connect-worker-start.png" >/dev/null
"$VM" record start

"$VM" run - <<'EOF'
caption "Connect FlightDeck Desktop to a worker"
pause 0.5
say "A worker is the machine where your agents run: a Linux server, or a Mac mini under the desk. This one is called devbox, and FlightDeck is already installed on it."
waitvoice 0.3

caption "On the worker: is FlightDeck running?"
say "Log in to the worker, and check that FlightDeck's worker is running."
pause 0.5
type "ssh devbox"
pause 0.3
key enter
pause 1.2
type "flightdeck worker status"
pause 0.3
key enter
pause 0.8
highlight 4 110 420 19
waitvoice 0.8
unhighlight

caption "Ask the worker for a pairing code"
say "Then ask it for a pairing code. It shows the address to connect to, and a four-digit code that works once, for two minutes."
pause 0.6
type "flightdeck worker pair"
pause 0.3
key enter
pause 1.0
highlight 4 290 470 36
waitvoice 0.8
unhighlight
EOF

CODE="$(podman exec fd-tutorial-worker sed -n 's/^Pairing code: \([0-9]*\).*/\1/p' /tmp/fd-last-pair)"

"$VM" run - <<EOF
caption "In FlightDeck: Connect to remote…"
say "Now switch to FlightDeck Desktop, and choose Connect to remote."
pause 0.4
key cmd-tab
pause 1.2
glide 849 516 1.0
pause 0.4
click
pause 1.2

caption "Enter the worker's address and the code"
say "Enter the worker's address, and the code."
glide 720 305 0.8
pause 0.3
click
pause 0.4
type devbox:7420
pause 0.4
glide 720 370 0.6
pause 0.3
click
pause 0.4
type $CODE
pause 0.5
highlight 498 399 152 30
say "Control lets you drive the worker. Observe only watches."
waitvoice 0.4
unhighlight
glide 505 457 0.8
pause 0.3
click
pause 1.2

caption "Not on Tailscale? FlightDeck warns you once"
highlight 463 441 512 151
say "On Tailscale, the link is encrypted for you. On any other network, FlightDeck warns you once, because this link is not encrypted. This is a private network, so connect anyway."
waitvoice 0.3
unhighlight
glide 536 565 0.8
pause 0.3
click
pause 2.5
glide 760 420 1.0

caption "Connected: the worker's own window"
highlight 163 41 127 22
say "You're connected. This window is the worker: its projects, its agents and its shells, all running on devbox. The pairing is remembered, so next time it's one click."
waitvoice 1.0
unhighlight
caption
pause 0.6
cut
EOF

"$VM" record stop connect-worker
