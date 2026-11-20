#!/usr/bin/env bash
# Tutorial: "Hooks: every new worktree ready to run" (about 1:30).
#   bakery (an Express app) -> a new agent's worktree has the .env FlightDeck
#   links in, but no node_modules, so `npm run dev` fails -> a worktree_created
#   hook in .flightdeck/hooks.toml (nano, in the Worker tab's shell) -> the next
#   agent's worktree is installed: the app starts, and shows in the browser.
# Off camera: bakery staged with its .env, never installed (as a worker's
# checkout usually is: only the agents' worktrees run anything), then opened.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
W="${WORKER_NAME:-fd-tutorial-worker}"
FIRST="add specials"
SECOND="opening hours"

"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/stage-project.sh" bakery >/dev/null
# The checkout has its .env, and no node_modules: Node would find a root
# install from the worktrees too (it looks in parent folders). Installing once
# and removing it fills npm's cache, so the hook's install is quick.
podman exec -u dev -w /home/dev/flightdeck/bakery "$W" sh -c 'cp .env.example .env && npm install --silent && rm -rf node_modules' >/dev/null
"$ROOT/tutorials/worker/trust.sh" bakery "$FIRST" "$SECOND" >/dev/null

# Off camera: open bakery (the first row of the worker's project list).
"$VM" run - <<'EOF'
glide 855 766 0.3
click
pause 1.5
glide 1379 156 0.3
click
pause 2.0
glide 760 420 0.3
EOF
"$VM" record start

"$VM" run - <<EOF
caption "Every agent starts in a fresh worktree"
pause 0.4
say "Every agent works in its own git worktree: a fresh checkout of the project. FlightDeck links your dot env file into it, but anything else git doesn't track stays behind, like node_modules."
glide 93 750 1.0
pause 0.3
click
pause 1.2
type "$FIRST" 18
pause 0.4
key enter
pause 2.0
waitvoice 0.3

caption "No node_modules: the app won't start"
say "Open a terminal in the new worktree, and look."
glide 199 750 1.0
pause 0.3
click
pause 1.0
glide 93 208 0.8
pause 0.3
click
pause 0.5
type "ls -a"
pause 0.3
key enter
pause 0.6
waitvoice 0.2
say "The dot env file is there, but no dependencies, so the app won't start."
type "npm run dev"
pause 0.3
key enter
pause 1.0
highlighttext "Cannot find module" -1 8
waitvoice 0.6
unhighlight

caption "Hooks: commands for every new worktree"
say "Hooks fix that. Each project has a hooks file in its dot flightdeck folder. Commands under worktree created run in every new worktree, as soon as FlightDeck creates it."
glide 508 21 1.0
pause 0.3
click
pause 0.8
glide 700 400 0.6
type "nano +27 ~/flightdeck/bakery/.flightdeck/hooks.toml"
pause 0.3
key enter
pause 1.0
waitvoice 0.2
say "Here, install the dependencies."
key ctrl-k
type 'commands = ["npm install"]' 18
pause 0.3
key enter
pause 0.6
key ctrl-o
pause 0.6
key enter
pause 0.6
key ctrl-x
waitvoice 0.4

caption "The next agent's worktree is ready to run"
say "Now start another agent."
glide 592 21 1.0
pause 0.3
click
pause 0.8
glide 93 750 1.0
pause 0.3
click
pause 1.2
type "$SECOND" 18
pause 0.4
key enter
pause 5.0
# A new agent is not selected while another exists (remote-control-bncp).
key opt-2
pause 1.0
waitvoice 0.2
say "The hook installed its dependencies, so the app starts right away."
glide 199 750 1.0
pause 0.3
click
pause 1.0
clicktext "shell 1" -1
pause 0.5
type "ls && npm run dev"
pause 0.3
key enter
pause 1.5
highlighttext "is open on" -1 8
waitvoice 0.6
unhighlight
glide 242 750 1.0
pause 0.3
click
pause 1.4
glide 788 108 1.0
pause 0.3
click
pause 0.4
type "localhost:3000"
pause 0.4
key enter
pause 1.5
glide 760 420 0.8

caption "Gitignored by default: commit it to share"
say "The hooks file is gitignored, so your hooks only run on your machine. To share them with your team, take it out of the gitignore and commit it."
waitvoice 1.0
caption
pause 0.6
cut
EOF

"$VM" record stop hooks
