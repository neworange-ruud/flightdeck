#!/usr/bin/env bash
# Tutorial: "When an agent needs you" (about 1:10).
#   an agent asks a question -> the sidebar, the Mission control badge and
#   the project tab show it -> answer in the terminal -> it carries on.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
STATE="$ROOT/tutorials/worker/agent-state.sh"
AGENT="sort notes"

"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" pinboard "$AGENT" >/dev/null
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
move 760 420
EOF
"$VM" record start

"$VM" run - <<EOF
caption "When an agent needs you"
pause 0.4
say "Agents sometimes stop and wait for you: to answer a question, or to allow something. Let's give this one a task where it has to ask first."
pause 1.0
type "I want sorting on the board. Before you change anything, ask me which order I want: newest first, oldest first, or alphabetical. Use a multiple-choice question." 34
pause 0.4
key enter
waitfor 120 $STATE "$AGENT" waiting
waitvoice 0.3

caption "Waiting for you: the sidebar shows it"
highlight 9 88 253 115
say "The agent is waiting. Its card in the sidebar turns amber and says so."
waitvoice 0.3
unhighlight
highlight 318 9 124 25
highlight 568 9 94 25
say "Mission control gets a badge, and so does the project's tab, so you notice it from anywhere in FlightDeck. Desktop notifications can tell you too, even when FlightDeck is in the background."
waitvoice 0.3
unhighlight

caption "Answer it in the agent's terminal"
highlight 294 240 1122 210
say "The question itself is in the agent's terminal. Pick an answer, and press Enter."
pause 1.6
key down
pause 0.8
key down
pause 0.8
waitvoice 0.2
unhighlight
key enter
pause 1.5

caption "The agent carries on (sped up)"
say "And it carries on, with your answer."
waitfor 30 $STATE "$AGENT" working
fast 6
waitfor 400 $STATE "$AGENT" idle
normal
pause 1

caption "Done, sorted alphabetically"
say "Done. Waiting agents never block the others: they keep working while this one waits for you."
pause 1.0
highlight 9 88 253 115
waitvoice 1.0
unhighlight
caption
pause 0.6
cut
EOF

"$VM" record stop agent-needs-you
