#!/usr/bin/env bash
# Tutorial: "Review before you merge" (about 1:40).
#   an agent has finished (uncommitted) -> the header's change count -> a
#   terminal in its worktree: git diff, npm test -> ask for a revision ->
#   it commits -> Finish.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
STATE="$ROOT/tutorials/worker/agent-state.sh"
AGENT="delete notes"

"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" pinboard "$AGENT" >/dev/null
# Off camera: the agent has done its first pass and stopped, uncommitted.
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
paste "Add a delete button to each note on the board. Keep the look consistent with the rest of the app. Don't commit yet."
key enter
waitfor 30 $STATE "$AGENT" working
waitfor 400 $STATE "$AGENT" idle
pause 1
move 760 420
EOF
"$VM" record start

"$VM" run - <<EOF
caption "Review an agent's work before you merge it"
pause 0.4
say "The agent says it's done. Before you merge, check its work."
waitvoice 0.2
highlight 652 53 82 22
say "The header shows how many files it changed. Nothing is committed yet."
waitvoice 0.4
unhighlight

caption "A terminal in its worktree: the diff"
say "Open a terminal in the agent's worktree, and look at the diff."
glide 199 750 1.0
pause 0.3
click
pause 1.0
glide 93 208 0.8
pause 0.3
click
pause 0.6
type "git diff --stat"
pause 0.3
key enter
waitvoice 1.2

caption "Run the tests"
say "Then run the tests, right there on the worker."
type "npm test"
pause 0.3
key enter
pause 3.5
waitvoice 1.0

caption "Ask the agent for a change"
say "They pass. But deleting a note can't be undone, so ask the agent to confirm first."
glide 93 181 1.0
pause 0.3
click
pause 1.2
move 840 400
click
pause 0.6
type "Ask for confirmation before deleting a note. Then run the tests and commit your work." 30
pause 0.4
key enter
waitfor 30 $STATE "$AGENT" working
waitvoice 0.2

caption "The agent revises its work (sped up)"
fast 6
waitfor 400 $STATE "$AGENT" idle
normal
pause 1

caption "Revised, tested and committed"
highlight 652 53 60 22
say "Done: revised, tested and committed. The worktree is clean again."
waitvoice 0.4
unhighlight

caption "Now it's ready: Finish merges it into main"
say "Now it's ready. Finish merges the branch into main."
glide 1302 64 1.2
pause 0.3
click
pause 1.2
glide 922 447 0.9
pause 0.3
click
pause 2
glide 760 420 1.0
waitvoice 1.0
caption
pause 0.6
cut
EOF

"$VM" record stop review-changes
