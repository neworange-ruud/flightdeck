#!/usr/bin/env bash
# Tutorial: "Keep an agent's branch up to date" (about 1:20).
#   a teammate pushes to main while an agent worked -> git pull in a worker
#   shell -> the header's "target +1" -> Rebase Worktree (type the name) ->
#   git log shows the agent's commit on top.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
STATE="$ROOT/tutorials/worker/agent-state.sh"
AGENT="search notes"

"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" pinboard "$AGENT" >/dev/null
# Off camera: the agent adds search and commits.
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
paste "Add a search box above the board that filters the notes as you type. Only change public/index.html and public/app.js. Commit your work when you're done."
key enter
waitfor 30 $STATE "$AGENT" working
waitfor 400 $STATE "$AGENT" idle
pause 1
move 760 420
EOF
# Meanwhile a teammate pushed a change to main.
podman exec fd-tutorial-worker sh -c 'rm -rf /tmp/sam && git clone -q --no-local /srv/git/pinboard.git /tmp/sam && cd /tmp/sam \
  && printf "\nPinboard is looked after by the Pinboard team.\n" >> README.md && git add README.md \
  && git -c user.name=Sam -c user.email=sam@example.com commit -q -m "README: who looks after Pinboard" \
  && git push -q origin main'
"$VM" record start

"$VM" run - <<EOF
caption "Keep an agent's branch up to date"
pause 0.4
say "While this agent worked on search, a teammate pushed a change to main. Let's bring that change into the agent's branch."
waitvoice 0.3

caption "Pull main on the worker, in a worker shell"
say "First, update the project's main branch on the worker. Open a worker shell, and pull."
glide 508 21 1.0
pause 0.3
click
pause 1.0
glide 600 300 0.6
type "cd ~/flightdeck/pinboard && git pull"
pause 0.3
key enter
pause 2.0
waitvoice 0.4

caption "The agent's target branch has moved on"
say "Back in the project, the agent's header now says main has moved ahead, by one commit."
glide 592 21 1.0
pause 0.3
click
pause 1.2
highlight 812 53 70 22
waitvoice 0.6
unhighlight

caption "Rebase Worktree puts the agent's work on top"
say "Rebase Worktree, from the command palette, moves the agent's commits on top of the new main."
glide 1340 21 1.0
pause 0.3
click
pause 0.8
type "rebase"
pause 0.6
key enter
pause 1.2
highlight 460 277 519 256
waitvoice 0.2
say "It rewrites the branch, so FlightDeck asks you to type the session's name first. If anything conflicts, it stops and changes nothing."
pause 1.5
type "$AGENT"
waitvoice 0.3
unhighlight
glide 919 506 0.8
pause 0.3
click
pause 2.0

caption "The agent's commit, on top of your teammate's"
say "Done. In a terminal in the worktree, the log shows the agent's commit, on top of your teammate's."
glide 199 750 1.0
pause 0.3
click
pause 1.0
glide 93 208 0.8
pause 0.3
click
pause 0.6
type "git log --oneline -3"
pause 0.3
key enter
pause 0.8
highlight 288 120 520 56
pause 2.5
waitvoice 1.0
unhighlight
caption
pause 0.6
cut
EOF

"$VM" record stop keep-up-to-date
