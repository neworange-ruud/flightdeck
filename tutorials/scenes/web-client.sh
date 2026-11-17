#!/usr/bin/env bash
# Tutorial: "FlightDeck Web: your worker in a browser" (about 1:30).
#   worker shell: flightdeck worker pair -> the "In a browser" link ->
#   Safari: open it, signed in at once -> projects, agents, live terminals,
#   the command palette -> back in the Desktop: taken over, Reconnect.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
STATE="$ROOT/tutorials/worker/agent-state.sh"
AGENT="settings panel"

"$VM" ssh 'killall Safari 2>/dev/null; true'
"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" pinboard "$AGENT" >/dev/null
# Off camera: Pinboard open, an agent at work; a Safari window filling the
# screen on a blank page (no Start Page), then back to FlightDeck.
"$VM" run - <<EOF
glide 855 766 0.3
click
pause 1.5
glide 1379 156 0.3
click
pause 2
glide 93 750 0.3
click
pause 1
type "$AGENT"
key enter
pause 6
paste "Add a settings panel, opened from a gear button in the header, with a note size option and a choice of board background. Keep the settings in localStorage. Commit your work when you're done."
key enter
waitfor 60 $STATE "$AGENT" working
EOF
"$VM" ssh 'rm -rf ~/Library/Saved\ Application\ State/com.apple.Safari.savedState; open -a Safari "about:blank"'
sleep 3
"$VM" run - <<'EOF'
move 423 14
click
pause 0.6
move 431 96
click
pause 1
move 377 98
click
pause 0.4
key cmd-w
pause 0.8
key ctrl-right
pause 1.5
move 760 420
EOF
"$VM" record start

"$VM" run - <<'EOF'
caption "FlightDeck Web: your worker in any browser"
pause 0.4
say "Every worker also serves FlightDeck Web: the same control surface, in a browser. Use it from a laptop without the Desktop app, or from a tablet."
waitvoice 0.3

caption "Ask the worker for a pairing link"
say "To pair a browser, ask the worker for a pairing code, here in a worker shell."
glide 508 21 1.0
pause 0.3
click
pause 1.0
glide 600 300 0.6
type "flightdeck worker pair"
pause 0.3
key enter
pause 1.0
highlight 16 147 310 20
say "Next to the address and the code, it prints a link for a browser. The code is in the link, so opening it signs the browser in."
waitvoice 0.4
unhighlight
EOF

CODE="$(podman exec fd-tutorial-worker sed -n 's/^Pairing code: \([0-9]*\).*/\1/p' /tmp/fd-last-pair)"

"$VM" run - <<EOF
caption "Open the link in a browser"
say "Open it in any browser that can reach the worker."
key ctrl-left
pause 1.5
key cmd-l
pause 0.5
type "devbox:7420/#$CODE"
pause 0.4
key enter
pause 3
glide 760 420 0.8
waitvoice 0.3

caption "Projects, agents and live terminals"
highlight 6 176 290 64
say "You're in. The browser shows the worker's projects, its agents, and their live terminals."
waitvoice 0.4
unhighlight
highlight 1300 147 130 24
say "Start agents and shells here, and type into any terminal, just like in the Desktop."
waitvoice 0.4
unhighlight

caption "Ctrl-G: the command palette"
say "Control G opens the command palette, with the same commands as the Desktop."
key ctrl-g
pause 1.2
waitvoice 1.2
key esc
pause 0.8

caption "One window at a time: the Desktop gives way"
say "A worker serves one window at a time. Back in the Desktop app, the browser has taken over."
key ctrl-right
pause 1.5
highlight 530 340 380 140
waitvoice 0.4
unhighlight
say "Reconnect takes it back. The browser stays paired, so it can take over again whenever you need it."
glide 719 417 1.0
pause 0.3
click
pause 2.0
glide 760 300 0.8
waitvoice 1.0
caption
pause 0.6
cut
EOF

"$VM" record stop web-client
