#!/usr/bin/env bash
# Tutorial: "Your first agent on a worker" (about 85 s).
#   open Pinboard on devbox -> new agent -> prompt -> it works -> terminal +
#   dev server -> session browser -> try the change -> commit -> Finish.
#
#   tutorials/scenes/first-agent.sh     stage, record, cut -> target/tutorials/videos/first-agent.mp4
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
AGENT="delete notes"
read -r DEL_X DEL_Y <<< "${DELETE_AT:-496 264}"

"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" pinboard "$AGENT" >/dev/null
"$VM" record start

"$VM" run - <<EOF
caption "FlightDeck Desktop, connected to the worker devbox"
pause 2.5
caption "Open a project on the worker"
glide 1095 1036 1.0
pause 0.4
click
pause 1.6
glide 1859 156 1.0
pause 0.5
click
pause 1.8
caption "New agent: it gets its own branch and worktree"
glide 93 1020 1.0
pause 0.4
click
pause 1.4
type "$AGENT"
pause 0.8
key enter
caption "Claude Code starts in the agent's worktree on devbox"
pause 6
caption "Tell the agent what to build"
pause 0.8
type "Add a delete button to each note on the board. Keep the look consistent with the rest of the app." 20
pause 0.8
key enter
caption "The agent works on devbox (sped up)"
waitfor 30 $ROOT/tutorials/worker/agent-state.sh "$AGENT" working
fast 6
waitfor 400 $ROOT/tutorials/worker/agent-state.sh "$AGENT" idle
normal
caption "Done: the agent explains what it changed"
pause 4
caption "Open a terminal in the agent's worktree"
glide 200 1020 1.0
pause 0.4
click
pause 1.2
glide 93 208 0.8
pause 0.3
click
pause 1.0
caption "Start the app on devbox"
type "npm run dev"
pause 0.5
key enter
pause 2
caption "Open a browser in the session: localhost is devbox's"
glide 241 1020 1.0
pause 0.4
click
pause 2
glide 1035 108 1.0
pause 0.3
click
pause 0.5
type "localhost:3000"
pause 0.4
key enter
pause 2.5
caption "Hover a note: there's the new delete button"
glide 410 300 1.0
pause 1.5
EOF

"$VM" mark cut
"$VM" shot "$ROOT/target/tutorials/videos/first-agent-hover.png" >/dev/null
"$VM" mark uncut

"$VM" run - <<EOF
glide $DEL_X $DEL_Y 0.7
pause 0.6
click
caption "Deleted. The change works, live from devbox"
pause 1.2
glide 760 560 0.8
pause 2.5
caption "Before you finish, ask the agent to commit its work"
glide 93 181 1.0
pause 0.4
click
pause 1.0
type "Commit this" 16
pause 0.5
key enter
waitfor 30 $ROOT/tutorials/worker/agent-state.sh "$AGENT" working
fast 6
waitfor 300 $ROOT/tutorials/worker/agent-state.sh "$AGENT" idle
normal
caption "Committed: the branch is ready to merge"
pause 3
caption "Finish merges the agent's branch into main"
glide 1782 64 1.2
pause 0.6
click
pause 2
caption "Merge into main: the worktree is cleaned up"
pause 1.5
glide 1162 581 0.9
pause 0.5
click
pause 3
glide 960 640 1.0
caption "The delete button is now on main. That's the whole loop."
pause 4
caption
pause 1
cut
EOF

"$VM" record stop first-agent
