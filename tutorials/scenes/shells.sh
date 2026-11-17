#!/usr/bin/env bash
# Tutorial: "Shells on the worker and in an agent's worktree" (about 1:30).
#   Terminal: ssh devbox -> cd into an agent's worktree -> git status;
#   FlightDeck: the Worker tab's shells (+, side by side) -> a terminal in
#   the agent's worktree -> Open Worktree in VS Code in the palette.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VM="$ROOT/tutorials/vm/vm"
AGENT="dark theme"

"$ROOT/tutorials/prepare.sh"
"$ROOT/tutorials/worker/trust.sh" pinboard "$AGENT" >/dev/null
# Off camera: Pinboard open, an agent started (no prompt: it just waits).
"$VM" run - <<EOF
glide 855 766 0.3
click
pause 1.5
glide 1379 156 0.3
click
pause 1.8
glide 93 750 0.3
click
pause 1.2
type "$AGENT"
key enter
pause 6
EOF
"$ROOT/tutorials/vm/terminal.sh"
"$VM" record start

"$VM" run - <<'EOF'
caption "A shell on the worker: SSH in"
pause 0.4
say "Sometimes you want a shell on the worker yourself: to look around, run a build, or check an agent's work. The worker is an ordinary machine, so you can SSH in."
pause 1.5
type "ssh devbox"
pause 0.3
key enter
waitvoice 0.3

caption "Each agent works in its own worktree"
say "Every agent works in its own git worktree, inside the project's dot flightdeck folder. Change into it, and you're on the agent's branch."
pause 0.5
type "cd ~/flightdeck/pinboard/.flightdeck/worktrees/dark-theme"
pause 0.3
key enter
pause 0.6
type "git status"
pause 0.3
key enter
pause 0.8
highlight 4 128 290 19
waitvoice 0.6
unhighlight

caption "No SSH needed: shells in FlightDeck"
say "But you don't need SSH at all. FlightDeck Desktop has shells on the worker built in."
pause 0.3
key ctrl-right
pause 1.5
glide 508 21 1.0
pause 0.3
click
pause 0.8
highlight 463 9 85 25
waitvoice 0.2
say "The Worker tab holds shells on the worker itself, outside any project. They keep running when you disconnect."
pause 0.8
unhighlight
glide 300 300 0.6
type "uname -n; ls ~/flightdeck"
pause 0.3
key enter
waitvoice 0.4

caption "Open more shells, side by side"
say "Open as many as you need, and show them side by side."
glide 115 61 0.9
pause 0.3
click
pause 1.0
type "top"
pause 0.3
key enter
pause 0.6
glide 1389 61 1.0
pause 0.3
click
waitvoice 1.5
glide 1389 61 0.4
click
pause 0.6

caption "A terminal in the agent's worktree"
say "For an agent's worktree, open a terminal from the sidebar. It starts in the agent's folder, on its branch."
glide 592 21 1.0
pause 0.3
click
pause 1.0
glide 199 750 1.0
pause 0.3
click
pause 1.0
glide 93 208 0.8
pause 0.3
click
pause 0.6
type "git status"
pause 0.3
key enter
pause 0.6
highlight 288 121 250 19
waitvoice 0.5
unhighlight

caption "Edit the worktree in VS Code, over SSH"
say "And to edit an agent's files in your editor, Open Worktree in VS Code opens the folder over SSH, right on the worker."
glide 1340 21 1.0
pause 0.3
click
pause 0.8
type "vs code"
pause 0.5
highlight 420 172 600 30
waitvoice 0.8
unhighlight
key esc
caption
pause 0.6
cut
EOF

"$VM" record stop shells
