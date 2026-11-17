#!/usr/bin/env bash
# Tutorial: "Mission control" (about 1:15).
#   two agents on Pinboard, one waiting on a question -> Mission control's
#   live tiles, needs-you first -> answer from the tile -> both finish ->
#   Earlier today cards with one-click Finish.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
STATE="$ROOT/tutorials/worker/agent-state.sh"
ASK="sort notes"; WORK="settings panel"

"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" pinboard "$ASK" "$WORK" >/dev/null
# Off camera: Pinboard open; one agent asks a question and waits, the other
# starts a longer task.
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
type "$ASK"
key enter
pause 5
key opt-1
pause 1.2
move 840 400
click
pause 0.6
paste "I want sorting on the board. Before you change anything, ask me which order I want: newest first, oldest first, or alphabetical. Use a multiple-choice question."
key enter
waitfor 120 $STATE "$ASK" waiting
glide 93 750 0.3
click
pause 1
type "$WORK"
key enter
pause 5
key opt-2
pause 1.2
move 840 400
click
pause 0.6
paste "Add a settings panel, opened from a gear button in the header, with a note size option (small, medium, large) and a choice of board background. Keep the settings in localStorage. Commit your work when you're done."
key enter
waitfor 60 $STATE "$WORK" working
move 760 420
EOF
"$VM" record start

"$VM" run - <<EOF
caption "Mission control: every agent at a glance"
pause 0.4
say "With several agents running, Mission control shows all of them at once, across every project, as live tiles."
glide 375 21 1.0
pause 0.3
click
pause 1.2
glide 760 420 0.8
waitvoice 0.3

caption "The ones that need you come first"
highlight 8 52 254 90
say "Agents that need you come first. This one is asking which sort order to use."
waitvoice 0.3
unhighlight
highlight 284 737 566 30
say "You can answer right from the tile, without opening the session."
waitvoice 0.2
glide 617 752 1.0
pause 0.5
click
unhighlight
pause 1.5

caption "Back to work: live tiles for both"
say "It's back to work. Every tile is a live terminal, so you can follow along."
waitvoice 1.5

caption "The agents work on devbox (sped up)"
fast 6
waitfor 600 sh -c "$STATE '$ASK' idle && $STATE '$WORK' idle"
normal
pause 1

caption "Done: finished sessions wait under Earlier today"
say "Finished sessions move down to Earlier today, with Push, Pull base and Finish one click away. Press Option M to switch back to the projects at any time."
pause 1.2
highlight 284 640 1144 130
waitvoice 0.8
unhighlight
caption
pause 0.6
cut
EOF

"$VM" record stop mission-control
