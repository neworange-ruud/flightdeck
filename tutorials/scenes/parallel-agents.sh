#!/usr/bin/env bash
# Tutorial: "Run agents in parallel" (about 2:00).
#   three agents on Pinboard at once (dark theme, search, README), each told
#   which files to leave alone -> watch them in the sidebar -> merge all three.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
STATE="$ROOT/tutorials/worker/agent-state.sh"
A1="dark theme"; A2="search notes"; A3="readme"

"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" pinboard "$A1" "$A2" "$A3" >/dev/null
# Off camera: Pinboard open.
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

# new_agent NAME N PROMPT: New agent, select it with Option-N (selecting
# also puts the keys in its terminal), type the prompt.
new_agent() {
  cat <<EOF
glide 93 750 0.9
pause 0.3
click
pause 1.0
type "$1" 18
pause 0.4
key enter
pause 5
key opt-$2
pause 0.8
type "$3" 40
pause 0.4
key enter
pause 1
EOF
}

{
cat <<'EOF'
caption "Run several agents at once"
pause 0.4
say "One agent is useful. Several at once is where FlightDeck shines. Every agent gets its own branch and its own worktree, so they never edit the same files."
waitvoice 0.2

caption "Agent one: a dark theme"
say "The first agent adds a dark theme."
EOF
new_agent "$A1" 1 "Add a dark theme that follows the system setting. Only change public/style.css. Commit your work when you're done."
cat <<'EOF'
caption "Agent two: search, while the first one works"
say "While it works, start a second agent, for search. Tell it to leave the stylesheet alone: that's the first agent's job."
EOF
new_agent "$A2" 2 "Add a search box above the board that filters the notes as you type. Only change public/index.html and public/app.js: other agents are working on the stylesheet and the README. Commit your work when you're done."
cat <<'EOF'
caption "Agent three: the README"
say "And a third, to rewrite the README."
EOF
new_agent "$A3" 3 "Rewrite README.md: what Pinboard is, how to run it, and how to run the tests. Only change README.md. Commit your work when you're done."
cat <<EOF
caption "All three work at the same time"
highlight 8 86 256 280
say "Now three agents are working at once. The sidebar shows each one's state, and its branch."
waitvoice 0.4
unhighlight
say "Switch between them with Option and a number, or a click, to watch one or steer it."
pause 0.6
key opt-1
pause 2.4
key opt-2
pause 2.4
waitvoice 0.2

caption "The agents work on devbox (sped up)"
fast 6
waitfor 600 sh -c "$STATE '$A1' idle && $STATE '$A2' idle && $STATE '$A3' idle"
normal

caption "Done: three branches, ready to merge"
say "All three are done, each on its own branch, with its work committed. Finish merges them into main, one by one."
key opt-1
pause 0.8
glide 1302 64 1.0
pause 0.3
click
pause 1.2
glide 922 447 0.8
pause 0.3
click
pause 2.5
key opt-1
pause 0.8
glide 1302 64 1.0
pause 0.3
click
pause 1.2
glide 922 447 0.8
pause 0.3
click
pause 2.5
key opt-1
pause 0.8
glide 1302 64 1.0
pause 0.3
click
pause 1.2
glide 922 447 0.8
pause 0.3
click
pause 2.5
glide 760 420 0.8
waitvoice 0.3

caption "Three features, built in parallel, all on main"
highlight 184 786 400 20
say "Three features, built side by side in the time of one, and all of them merged into main."
waitvoice 1.0
unhighlight
caption
pause 0.6
cut
EOF
} | "$VM" run -

"$VM" record stop parallel-agents
