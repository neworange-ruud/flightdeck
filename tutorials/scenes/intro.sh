#!/usr/bin/env bash
# The homepage introduction: FlightDeck's main features in about 90 s.
#   two agents in parallel on Pinboard -> Mission control, one asks a
#   question, answered from its tile -> both work (sped up) -> try one in
#   the session's browser -> Finish merges it.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
STATE="$ROOT/tutorials/worker/agent-state.sh"
A1="search notes"; A2="sort notes"

"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" pinboard "$A1" "$A2" >/dev/null
"$VM" run - <<EOF
glide 855 766 0.3
click
pause 1.5
glide 1379 156 0.3
click
pause 2
move 760 420
EOF
"$VM" record start

"$VM" run - <<EOF
caption "FlightDeck: your coding agents, side by side"
pause 0.4
say "This is FlightDeck. It runs your AI coding agents side by side: here on devbox, a worker machine, driven from the Mac."
waitvoice 0.2

caption "Every agent gets its own branch and worktree"
say "Start an agent, and tell it what to build. Every agent gets its own git branch and worktree."
glide 93 750 0.9
pause 0.3
click
pause 0.9
type "$A1" 18
pause 0.3
key enter
pause 5
key opt-1
pause 1.2
move 840 400
click
pause 0.5
type "Add a search box above the board that filters the notes as you type. Only change public/index.html and public/app.js. Commit your work when you're done." 45
pause 0.3
key enter
waitvoice 0.2

caption "Start another one while the first one works"
say "Then start another, while the first one works."
glide 93 750 0.9
pause 0.3
click
pause 0.9
type "$A2" 18
pause 0.3
key enter
pause 5
key opt-2
pause 1.2
move 840 400
click
pause 0.5
type "I want sorting on the board. Before you change anything, ask me which order I want: newest first, oldest first, or alphabetical. Use a multiple-choice question." 45
pause 0.3
key enter
waitvoice 0.2

caption "Mission control: every agent at a glance"
say "Mission control shows every agent at once, as live tiles."
glide 375 21 1.0
pause 0.3
click
pause 1.0
glide 760 420 0.6
waitfor 120 $STATE "$A2" waiting
waitvoice 0.2
# The first agent finishes first (sped up), so the waiting tile is the only
# live one and fills the width: its answer buttons are then always in place.
fast 6
waitfor 300 $STATE "$A1" idle
normal
pause 0.8
EOF

"$VM" run - <<EOF
highlighttext "Oldest first" -1 10
say "When an agent needs you, it comes first. Answer right from its tile."
waitvoice 0.2
clicktext "Oldest first"
unhighlight
waitfor 30 $STATE "$A2" working

caption "The agents work on devbox (sped up)"
fast 6
waitfor 600 sh -c "$STATE '$A1' idle && $STATE '$A2' idle"
normal
pause 0.6

caption "Try the change in the session's own browser"
say "Both are done. Try a change in the session's own browser, where localhost is the worker."
glide 240 21 1.0
pause 0.3
click
pause 0.8
key opt-1
pause 1.0
glide 199 750 0.9
pause 0.3
click
pause 0.9
glide 93 208 0.7
pause 0.3
click
pause 0.5
type "npm run dev"
pause 0.3
key enter
pause 1.4
glide 242 750 0.9
pause 0.3
click
pause 1.3
glide 788 108 0.8
pause 0.3
click
pause 0.4
type "localhost:3000"
pause 0.3
key enter
pause 2.2
glide 760 420 0.8
waitvoice 0.6

caption "Finish merges the agent's branch into main"
say "Happy with it? Finish merges the branch into main, and cleans up the worktree."
glide 1302 64 1.0
pause 0.3
click
pause 1.0
glide 922 447 0.8
pause 0.3
click
pause 1.8
glide 760 420 0.8
waitvoice 0.4

caption "FlightDeck: many agents, one window, you in control"
say "That's FlightDeck: many agents, one window, and you stay in control. The tutorials show every step in detail."
waitvoice 1.2
caption
pause 0.6
cut
EOF

"$VM" record stop intro
