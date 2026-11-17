#!/usr/bin/env bash
# Tutorial: "Start a new project" (about 1:40).
#   Open a project…: the worker's projects, browse, clone, new project ->
#   create "recipes" (a git repo with a first commit) -> first agent builds a
#   small site -> npm run dev -> the session browser shows it.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
STATE="$ROOT/tutorials/worker/agent-state.sh"
AGENT="recipe cards"

"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" recipes "$AGENT" >/dev/null
"$VM" record start

"$VM" run - <<EOF
caption "Start a new project on the worker"
pause 0.4
say "Every project in FlightDeck is a git repository on the worker. Open a project shows all the ways to get one."
glide 855 766 1.0
pause 0.3
click
pause 1.4
waitvoice 0.2

caption "Projects it already has, any folder, or a clone"
highlight 16 112 1408 60
say "Repositories already in the worker's projects folder are listed at the top."
waitvoice 0.2
unhighlight
highlight 16 192 1408 100
say "Browse opens any folder on the worker."
waitvoice 0.2
unhighlight
highlight 16 306 1408 62
say "Clone takes a git URL, from GitHub or anywhere else the worker can reach."
waitvoice 0.2
unhighlight

caption "Or start from scratch: New project"
say "Or start from nothing. Pick the projects folder, give the new project a name, and Create."
glide 63 273 1.0
pause 0.3
click
pause 0.8
highlight 16 380 1408 64
glide 600 422 0.8
pause 0.3
click
pause 0.4
type "recipes"
pause 0.4
waitvoice 0.2
unhighlight
glide 1385 422 0.8
pause 0.3
click
pause 1.5

caption "A fresh git repository, ready for agents"
highlight 180 782 660 24
say "FlightDeck creates the folder with a git repository and a first commit, and opens it. Add a remote when you're ready to push."
waitvoice 0.4
unhighlight

caption "The first agent builds the first version"
say "Now the first agent can build the first version."
glide 93 750 1.0
pause 0.3
click
pause 1.0
type "$AGENT"
pause 0.4
key enter
pause 5
type "Build a small recipe website: one page with three recipes as cards, each with its ingredients and steps. Serve it with npm run dev on port 3000, using a tiny Node server with no dependencies. Commit your work when you're done." 34
pause 0.4
key enter
waitfor 60 $STATE "$AGENT" working
waitvoice 0.2

caption "The agent works on devbox (sped up)"
fast 6
waitfor 500 $STATE "$AGENT" idle
normal
pause 1

caption "Run it and open it in the session's browser"
say "Done. Start it, and open it in the session's browser."
glide 199 750 1.0
pause 0.3
click
pause 1.0
glide 93 208 0.8
pause 0.3
click
pause 0.6
type "npm run dev"
pause 0.3
key enter
pause 1.5
glide 242 750 1.0
pause 0.3
click
pause 1.4
glide 788 108 0.9
pause 0.3
click
pause 0.4
type "localhost:3000"
pause 0.3
key enter
pause 2.5
glide 760 420 0.8
waitvoice 0.4
say "From an empty folder to a working first version, without leaving FlightDeck."
waitvoice 1.2
caption
pause 0.6
cut
EOF

"$VM" record stop new-project
