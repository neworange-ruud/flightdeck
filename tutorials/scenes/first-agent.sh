#!/usr/bin/env bash
# Tutorial: "Your first agent on a worker" (about 1:45).
#   open Pinboard on devbox -> new agent -> prompt -> it works -> terminal +
#   dev server -> session browser -> try the change -> commit -> Finish.
#
#   tutorials/scenes/first-agent.sh     stage, record, cut -> target/tutorials/videos/first-agent.mp4
#
# Narrated (`say`, ElevenLabs) with highlights; coordinates are screen points
# on the 1440x810 guest display.
#
# Uses a real Claude Code agent, so each take differs a little. The one
# position that depends on the agent is its delete button: the scene stops
# (cut) after hovering a note and saves a screenshot; if the button is not at
# DELETE_AT, the take goes on with a click in the wrong place, so look at
# target/tutorials/videos/first-agent-hover.png and re-run with
# DELETE_AT="x y" (screen points).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
STATE="$ROOT/tutorials/worker/agent-state.sh"
AGENT="delete notes"
read -r DEL_X DEL_Y <<< "${DELETE_AT:-492 264}"

"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" pinboard "$AGENT" >/dev/null
"$VM" record start

"$VM" run - <<EOF
caption "FlightDeck Desktop, connected to the worker devbox"
pause 0.6
say "This is FlightDeck Desktop on a Mac. It's connected to devbox: a worker machine, where the agents and the code live."
pause 1.2
highlight 78 8 128 26
waitvoice 0.4
unhighlight

caption "Open a project on the worker"
say "Let's open a project on the worker."
glide 855 766 1.0
pause 0.4
click
pause 1.2
highlight 22 138 1398 38
pause 1.4
glide 1379 156 0.9
pause 0.4
click
unhighlight
pause 1.4

caption "New agent: it gets its own branch and worktree"
say "Now start a new agent. Every agent gets its own git branch and worktree, so agents can work side by side without getting in each other's way."
glide 93 750 1.0
pause 0.4
click
pause 1.2
type "$AGENT"
pause 0.6
highlight 797 487 165 30
waitvoice 0.3
unhighlight
key enter
pause 1.5

caption "Claude Code starts in the agent's worktree on devbox"
say "Claude Code starts right in that worktree, on devbox."
pause 0.8
highlight 396 52 250 24
waitvoice 0.6
unhighlight

caption "Tell the agent what to build"
say "Tell it what to build."
pause 0.4
type "Add a delete button to each note on the board. Keep the look consistent with the rest of the app." 20
pause 0.8
key enter

caption "The agent works on devbox (sped up)"
waitfor 30 $STATE "$AGENT" working
say "While it works, the sidebar keeps you posted on what each agent is doing."
pause 1.5
highlight 9 88 253 115
waitvoice 0.3
fast 6
pause 9
unhighlight
waitfor 400 $STATE "$AGENT" idle
normal

caption "Done: the agent explains what it changed"
say "Done. The agent sums up what it changed, and the header shows the changes waiting in its worktree."
pause 2.5
highlight 655 53 76 22
waitvoice 0.5
unhighlight

caption "Open a terminal in the agent's worktree"
say "To try it, open a terminal in the same worktree, and start the app."
glide 199 750 1.0
pause 0.4
click
pause 1.0
glide 93 208 0.8
pause 0.3
click
pause 0.8
type "npm run dev"
pause 0.4
key enter
waitvoice 0.5

caption "Open a browser in the session: localhost is devbox's"
say "Then open a browser inside the session. Its requests are made by the worker, so localhost here is devbox."
glide 242 750 1.0
pause 0.4
click
pause 1.4
highlight 1278 95 78 26
glide 788 108 1.0
pause 0.3
click
pause 0.4
type "localhost:3000"
pause 0.4
key enter
waitvoice 0.3
unhighlight

caption "Hover a note: there's the new delete button"
say "Hover a note, and there's the new delete button."
glide 405 300 1.0
pause 0.8
highlight $((DEL_X - 12)) $((DEL_Y - 12)) 24 24
waitvoice 0.3
EOF

"$VM" mark cut
"$VM" shot "$ROOT/target/tutorials/videos/first-agent-hover.png" >/dev/null
"$VM" mark uncut

"$VM" run - <<EOF
unhighlight
glide $DEL_X $DEL_Y 0.7
pause 0.5
click
caption "Deleted. The change works, live from devbox"
say "One click, and the note is gone. It works."
pause 0.6
glide 720 560 0.8
waitvoice 0.6

caption "Before you finish, ask the agent to commit its work"
say "Before finishing, ask the agent to commit its work."
glide 92 181 1.0
pause 0.4
click
pause 0.8
type "Commit this" 16
pause 0.5
key enter
waitfor 30 $STATE "$AGENT" working
fast 6
waitfor 300 $STATE "$AGENT" idle
normal

caption "Finish merges the agent's branch into main"
say "Committed. Now, Finish merges the agent's branch into main."
pause 1
highlight 1266 50 74 28
glide 1302 64 1.2
waitvoice 0.2
unhighlight
click
pause 1.2
highlight 476 355 476 46
say "FlightDeck asks first: merge into main, then remove the worktree."
waitvoice 0.4
unhighlight

caption "Merge into main: the worktree is cleaned up"
glide 922 447 0.9
pause 0.4
click
pause 1.2
highlight 184 786 372 20
glide 720 480 1.0
caption "The delete button is now on main. That's the whole loop."
say "That's the whole loop: an agent on a remote worker, from the first prompt to a change merged into main."
waitvoice 1.2
unhighlight
caption
pause 0.8
cut
EOF

"$VM" record stop first-agent
